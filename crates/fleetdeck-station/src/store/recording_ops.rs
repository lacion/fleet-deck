//! The durable record of the monitor recordings (R13 P5b task S2, design
//! `omarchy-p5b.md` §5.1, §5.3, §5.4, §7.5, §8). A recording is a row of
//! `capture_ops` with `operation_kind = 'recording'`; the V36 columns hold the
//! seconds, the link epoch, the phase, the stop latch and the result. Each
//! [`Store`] method runs one transaction on the one station connection.
//!
//! - [`Store::recording_begin`] decides a new request, a repeat, a conflict or a
//!   quota refusal, and reserves [`PEAK_RESERVATION`] for a new one.
//! - [`Store::recording_phase`] records the progress; [`Store::recording_latch`]
//!   is the stop latch `none → save → discard` (the first discard reason wins).
//! - [`Store::recording_commit`] makes the artifact (kind `video`, one file
//!   `capture.mp4`), its version and the `committed` state in one transaction;
//!   the reservation becomes the bytes of the staging copy.
//! - [`Store::recording_fail`] ends a `running` recording as `failed`; the
//!   reservation stays until the durable removal ([`Store::capture_cleaned`]).
//!   [`Store::capture_staging_cleaned`] frees the reservation of a committed
//!   operation after its staging copy is removed durably.
//! - The orphan scan of the startup recovery (§5.4 step 6) counts what it finds
//!   in `capture_garbage` until the durable removal ([`Store::garbage_record`],
//!   [`Store::garbage_delete`]).

use std::collections::HashSet;

use fleetdeck_proto::artifacts::ArtifactKind;
use fleetdeck_proto::capture::CaptureKind;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::recording::{PEAK_RESERVATION, RECORDING_FILE, RecordingPhase, StopReason};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use super::{
    Begin, COLUMNS, CaptureOp, CommitSpec, NewCaptureArtifact, accounted, commit, finish_op,
    raw_op, read_op,
};
use crate::desktop::capture::CAPTURE_QUOTA;
use crate::store::artifacts::{CommitError, Committed};
use crate::store::{Result, Store, StoreError, call, to_i64, to_u64};

/// The stop latch of a recording (§4.4, §7.5): `none → save → discard` and
/// `none → discard`. It never goes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopIntent {
    None,
    Save,
    Discard,
}

impl StopIntent {
    fn parse(text: &str) -> Result<Self> {
        match text {
            "none" => Ok(Self::None),
            "save" => Ok(Self::Save),
            "discard" => Ok(Self::Discard),
            other => Err(StoreError::Corrupt(format!("stop intent {other}"))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Save => "save",
            Self::Discard => "discard",
        }
    }
}

/// What [`Store::recording_begin`] needs for a new recording.
#[derive(Debug, Clone, Copy)]
pub struct RecordingBegin<'a> {
    pub deck_fp: &'a str,
    pub capture_id: &'a str,
    pub action_hash: &'a str,
    pub monitor: &'a str,
    pub artifact_id: &'a str,
    pub seconds: u32,
    /// The `local_desktop_epoch` of the link that started the recording.
    pub link_epoch: &'a str,
    /// The coordinator context of a coordinator recording (R13 P5b-2 §13.3),
    /// stored as JSON in `capture_ops.coordinator`; `None` for an owner
    /// recording.
    pub coordinator: Option<&'a CoordinatorContext>,
    pub now: i64,
}

/// A recording row: the operation and the V36 columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingRow {
    pub op: CaptureOp,
    pub requested_seconds: u32,
    pub link_epoch: Option<String>,
    pub phase: Option<RecordingPhase>,
    pub stop_intent: StopIntent,
    pub stop_reason: Option<StopReason>,
    /// Station Unix ms of the actual recorder start.
    pub recorder_started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// The area of a `capture_garbage` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GarbageArea {
    /// An entry of `<station data>/captures`.
    Staging,
    /// An entry of `<station data>/artifacts`.
    Artifacts,
}

impl GarbageArea {
    fn as_str(self) -> &'static str {
        match self {
            Self::Staging => "staging",
            Self::Artifacts => "artifacts",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "staging" => Ok(Self::Staging),
            "artifacts" => Ok(Self::Artifacts),
            other => Err(StoreError::Corrupt(format!("garbage area {other}"))),
        }
    }
}

/// What the orphan scan of the recovery may touch (§5.4 step 6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanSets {
    /// The names of the staging entries that belong to an operation: a
    /// `running` one, or one that still holds a reservation (its own cleanup
    /// removes them and its reservation counts them).
    pub staging_keep: HashSet<String>,
    /// The names of `<artifacts>/` entries that a capture operation named
    /// and that have no version row and no operation that is `running` or
    /// still holds a reservation: garbage.
    pub artifact_orphans: HashSet<String>,
}

/// The snake_case text of a stop reason.
pub(super) fn reason_text(r: StopReason) -> Result<String> {
    match serde_json::to_value(r)? {
        Value::String(text) => Ok(text),
        other => Err(StoreError::Corrupt(format!("stop reason {other}"))),
    }
}

fn parse_reason(text: &str) -> Result<StopReason> {
    serde_json::from_value(Value::String(text.to_owned()))
        .map_err(|_| StoreError::Corrupt(format!("stop reason {text}")))
}

fn phase_text(p: RecordingPhase) -> Result<String> {
    match serde_json::to_value(p)? {
        Value::String(text) => Ok(text),
        other => Err(StoreError::Corrupt(format!("recording phase {other}"))),
    }
}

fn parse_phase(text: &str) -> Result<RecordingPhase> {
    serde_json::from_value(Value::String(text.to_owned()))
        .map_err(|_| StoreError::Corrupt(format!("recording phase {text}")))
}

fn small(v: i64, what: &str) -> Result<u32> {
    u32::try_from(v).map_err(|_| StoreError::Corrupt(format!("recording {what} {v}")))
}

type RawExtra = (
    Option<i64>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

const EXTRA: &str = "requested_seconds, link_epoch, phase, stop_intent, stop_reason,
                     recorder_started_at, ended_at, duration_ms, width, height";

/// The number of the first column of [`EXTRA`] after [`COLUMNS`].
const EXTRA_AT: usize = 13;

fn finish_row(raw: (super::RawOp, RawExtra)) -> Result<RecordingRow> {
    let (op, extra) = raw;
    let (seconds, link_epoch, phase, intent, reason, started, ended, duration, width, height) =
        extra;
    let op = finish_op(op)?;
    let seconds = seconds.ok_or_else(|| {
        StoreError::Corrupt(format!("recording {} has no seconds", op.capture_id))
    })?;
    Ok(RecordingRow {
        op,
        requested_seconds: small(seconds, "seconds")?,
        link_epoch,
        phase: phase.as_deref().map(parse_phase).transpose()?,
        stop_intent: StopIntent::parse(&intent)?,
        stop_reason: reason.as_deref().map(parse_reason).transpose()?,
        recorder_started_at: started,
        ended_at: ended,
        duration_ms: duration.map(to_u64).transpose()?,
        width: width.map(|v| small(v, "width")).transpose()?,
        height: height.map(|v| small(v, "height")).transpose()?,
    })
}

impl Store {
    /// Decides a recording request in one transaction. A row with another
    /// action hash, another kind, another monitor or other seconds is a
    /// conflict; a row of the same request is returned and nothing changes;
    /// otherwise the quota is checked (the accounted capture bytes and the new
    /// reservation of [`PEAK_RESERVATION`] must stay within [`CAPTURE_QUOTA`])
    /// and a `running` row in the phase `preparing` that holds the
    /// reservation is inserted.
    ///
    /// # Errors
    /// A database error; a `seconds` outside 1 to 120 (the column check).
    pub async fn recording_begin(&self, b: RecordingBegin<'_>) -> Result<Begin> {
        let (deck_fp, capture_id) = (b.deck_fp.to_owned(), b.capture_id.to_owned());
        let (action_hash, monitor) = (b.action_hash.to_owned(), b.monitor.to_owned());
        let (artifact_id, link_epoch) = (b.artifact_id.to_owned(), b.link_epoch.to_owned());
        let (seconds, now) = (b.seconds, b.now);
        let coordinator = b.coordinator.map(serde_json::to_string).transpose()?;
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if let Some(row) = read_op(&tx, &deck_fp, &capture_id)? {
                let stored: Option<i64> = tx.query_row(
                    "SELECT requested_seconds FROM capture_ops
                     WHERE deck_fp = ?1 AND capture_id = ?2",
                    params![deck_fp, capture_id],
                    |r| r.get(0),
                )?;
                let same = row.kind == CaptureKind::Recording
                    && row.action_hash == action_hash
                    && row.monitor == monitor
                    && stored == Some(i64::from(seconds));
                return Ok(if same {
                    Begin::Existing(row)
                } else {
                    Begin::Conflict
                });
            }
            if accounted(&tx)?.saturating_add(PEAK_RESERVATION) > CAPTURE_QUOTA {
                return Ok(Begin::Quota);
            }
            tx.execute(
                "INSERT INTO capture_ops
                     (deck_fp, capture_id, action_hash, state, monitor, artifact_id,
                      reserved_bytes, started_at, updated_at, operation_kind,
                      requested_seconds, link_epoch, phase, coordinator)
                 VALUES (?1, ?2, ?3, 'running', ?4, ?5, ?6, ?7, ?7, 'recording', ?8, ?9,
                         'preparing', ?10)",
                params![
                    deck_fp,
                    capture_id,
                    action_hash,
                    monitor,
                    artifact_id,
                    to_i64(PEAK_RESERVATION)?,
                    now,
                    i64::from(seconds),
                    link_epoch,
                    coordinator
                ],
            )?;
            tx.commit()?;
            Ok(Begin::Started)
        })
        .await
    }

    /// Records the phase of a `running` recording, and the actual recorder
    /// start when `started_at` is set (an earlier start is kept when it is
    /// `None`). A row that is not a running recording does not change.
    ///
    /// # Errors
    /// A database error.
    pub async fn recording_phase(
        &self,
        deck_fp: &str,
        capture_id: &str,
        phase: RecordingPhase,
        started_at: Option<i64>,
    ) -> Result<()> {
        let now = self.now();
        let (deck_fp, capture_id) = (deck_fp.to_owned(), capture_id.to_owned());
        let phase = phase_text(phase)?;
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE capture_ops
                 SET phase = ?3, recorder_started_at = COALESCE(?4, recorder_started_at),
                     updated_at = ?5
                 WHERE deck_fp = ?1 AND capture_id = ?2 AND state = 'running'
                   AND operation_kind = 'recording'",
                params![deck_fp, capture_id, phase, started_at, now],
            )?;
            Ok(())
        })
        .await
    }

    /// The stop latch of a running recording (§4.4, §7.5), in one
    /// transaction. `save` says whether the request saves; `reason` must be
    /// a save reason for it and a discard reason otherwise.
    ///
    /// - `none → save` and `none → discard` store the reason.
    /// - `save → discard` stores the discard reason.
    /// - The first discard reason wins: a later discard keeps it, and a save
    ///   after a discard changes nothing.
    /// - A second save keeps the first save reason.
    ///
    /// It answers the intent and the reason that the row holds afterwards, or
    /// `None` when the row is not a running recording.
    ///
    /// # Errors
    /// A database error; `reason` that does not fit `save`.
    pub async fn recording_latch(
        &self,
        deck_fp: &str,
        capture_id: &str,
        save: bool,
        reason: StopReason,
    ) -> Result<Option<(StopIntent, StopReason)>> {
        if save != reason.is_save() {
            return Err(StoreError::Corrupt(format!(
                "stop reason {reason:?} does not fit a {} request",
                if save { "save" } else { "discard" }
            )));
        }
        let now = self.now();
        let (deck_fp, capture_id) = (deck_fp.to_owned(), capture_id.to_owned());
        let text = reason_text(reason)?;
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let current: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT stop_intent, stop_reason FROM capture_ops
                     WHERE deck_fp = ?1 AND capture_id = ?2 AND state = 'running'
                       AND operation_kind = 'recording'",
                    params![deck_fp, capture_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((intent, held)) = current else {
                return Ok(None);
            };
            let intent = StopIntent::parse(&intent)?;
            let next = match (intent, save) {
                (StopIntent::None, true) => Some(StopIntent::Save),
                (StopIntent::None | StopIntent::Save, false) => Some(StopIntent::Discard),
                (StopIntent::Save, true) | (StopIntent::Discard, _) => None,
            };
            let Some(next) = next else {
                let held = held.ok_or_else(|| {
                    StoreError::Corrupt(format!("recording {capture_id} has a latch and no reason"))
                })?;
                return Ok(Some((intent, parse_reason(&held)?)));
            };
            tx.execute(
                "UPDATE capture_ops SET stop_intent = ?3, stop_reason = ?4, updated_at = ?5
                 WHERE deck_fp = ?1 AND capture_id = ?2",
                params![deck_fp, capture_id, next.as_str(), text, now],
            )?;
            tx.commit()?;
            Ok(Some((next, reason)))
        })
        .await
    }

    /// Makes the artifact (kind `video`, the one file `capture.mp4`), its
    /// version 1 and the `committed` state of the recording in one
    /// transaction. The bytes are already in `<artifacts>/<artifact_id>/1/`
    /// and synced. The row keeps `staging_bytes` reserved until its staging
    /// copy is removed ([`Store::capture_staging_cleaned`]). The recording
    /// metadata of the provenance gives the duration, the size and the stop
    /// reason of the row.
    ///
    /// # Errors
    /// [`CommitError::UnknownArtifact`] when the artifact ID has a
    /// tombstone; a store error when the provenance is not a recording with a
    /// save reason, or when the operation is not a running recording or has a
    /// discard latch. Nothing changes on an error.
    pub async fn recording_commit(
        &self,
        deck_fp: &str,
        capture_id: &str,
        new: NewCaptureArtifact,
        staging_bytes: u64,
    ) -> std::result::Result<Committed, CommitError> {
        let invalid = |what: &str| {
            CommitError::Store(StoreError::Corrupt(format!(
                "recording {capture_id} cannot commit: {what}"
            )))
        };
        if new.provenance.kind != CaptureKind::Recording {
            return Err(invalid("the provenance is not a recording"));
        }
        let Some(meta) = new.provenance.recording.clone() else {
            return Err(invalid("the provenance has no recording metadata"));
        };
        if !meta.stop_reason.is_save() {
            return Err(invalid("the stop reason is not a save reason"));
        }
        let now = self.now();
        let (deck_fp, capture_id) = (deck_fp.to_owned(), capture_id.to_owned());
        call(&self.inner.db, move |conn| {
            let spec = CommitSpec {
                kind: ArtifactKind::Video,
                file: RECORDING_FILE,
                operation: CaptureKind::Recording,
                reserved: staging_bytes,
                recording: Some(&meta),
            };
            Ok(commit(conn, &deck_fp, &capture_id, &new, now, &spec))
        })
        .await?
    }

    /// Ends a `running` recording as `failed` with its code, its text and the
    /// reason that stopped it (the caller passes the latched reason, so the
    /// first discard reason wins there). The reservation stays: the files of the
    /// operation may still exist, and [`Store::capture_cleaned`] frees it
    /// after they are removed. A row that is not a running recording does
    /// not change.
    ///
    /// # Errors
    /// A database error.
    pub async fn recording_fail(
        &self,
        deck_fp: &str,
        capture_id: &str,
        code: &str,
        message: &str,
        reason: StopReason,
    ) -> Result<()> {
        let now = self.now();
        let (deck_fp, capture_id) = (deck_fp.to_owned(), capture_id.to_owned());
        let (code, message) = (code.to_owned(), message.to_owned());
        let reason = reason_text(reason)?;
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE capture_ops
                 SET state = 'failed', code = ?3, message = ?4, stop_reason = ?5,
                     ended_at = ?6, updated_at = ?6
                 WHERE deck_fp = ?1 AND capture_id = ?2 AND state = 'running'
                   AND operation_kind = 'recording'",
                params![deck_fp, capture_id, code, message, reason, now],
            )?;
            Ok(())
        })
        .await
    }

    /// Frees the reservation of a `committed` operation whose staging copy
    /// is removed durably. A row in another state does not change.
    ///
    /// # Errors
    /// A database error.
    pub async fn capture_staging_cleaned(&self, deck_fp: &str, capture_id: &str) -> Result<()> {
        let now = self.now();
        let (deck_fp, capture_id) = (deck_fp.to_owned(), capture_id.to_owned());
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE capture_ops SET reserved_bytes = 0, updated_at = ?3
                 WHERE deck_fp = ?1 AND capture_id = ?2 AND state = 'committed'",
                params![deck_fp, capture_id, now],
            )?;
            Ok(())
        })
        .await
    }

    /// One recording row; `None` for an unknown ID and for a screenshot.
    ///
    /// # Errors
    /// A database error.
    pub async fn recording_get(
        &self,
        deck_fp: &str,
        capture_id: &str,
    ) -> Result<Option<RecordingRow>> {
        let (deck_fp, capture_id) = (deck_fp.to_owned(), capture_id.to_owned());
        call(&self.inner.db, move |conn| {
            conn.query_row(
                &format!(
                    "SELECT {COLUMNS}, {EXTRA} FROM capture_ops
                     WHERE deck_fp = ?1 AND capture_id = ?2 AND operation_kind = 'recording'"
                ),
                params![deck_fp, capture_id],
                |r| {
                    Ok((
                        raw_op(r)?,
                        (
                            r.get(EXTRA_AT)?,
                            r.get(EXTRA_AT + 1)?,
                            r.get(EXTRA_AT + 2)?,
                            r.get(EXTRA_AT + 3)?,
                            r.get(EXTRA_AT + 4)?,
                            r.get(EXTRA_AT + 5)?,
                            r.get(EXTRA_AT + 6)?,
                            r.get(EXTRA_AT + 7)?,
                            r.get(EXTRA_AT + 8)?,
                            r.get(EXTRA_AT + 9)?,
                        ),
                    ))
                },
            )
            .optional()?
            .map(finish_row)
            .transpose()
        })
        .await
    }

    /// The names that the orphan scan keeps and may remove (§5.4 step 6).
    ///
    /// # Errors
    /// A database error.
    pub async fn capture_scan_sets(&self) -> Result<ScanSets> {
        call(&self.inner.db, move |conn| {
            let names = |sql: &str| -> Result<HashSet<String>> {
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<HashSet<_>>>()?;
                Ok(rows)
            };
            Ok(ScanSets {
                staging_keep: names(
                    "SELECT capture_id FROM capture_ops
                     WHERE state = 'running' OR reserved_bytes > 0",
                )?,
                artifact_orphans: names(
                    "SELECT DISTINCT o.artifact_id FROM capture_ops o
                     WHERE NOT EXISTS (SELECT 1 FROM artifact_versions v
                                       WHERE v.artifact_id = o.artifact_id)
                       AND NOT EXISTS (SELECT 1 FROM capture_ops r
                                       WHERE r.artifact_id = o.artifact_id
                                         AND (r.state = 'running' OR r.reserved_bytes > 0))",
                )?,
            })
        })
        .await
    }

    /// Counts a garbage entry in the capture usage with its size, until
    /// [`Store::garbage_delete`] runs after its durable removal. A repeated
    /// call replaces the row.
    ///
    /// # Errors
    /// A database error.
    pub async fn garbage_record(&self, area: GarbageArea, name: &str, bytes: u64) -> Result<()> {
        let now = self.now();
        let name = name.to_owned();
        let bytes = to_i64(bytes)?;
        call(&self.inner.db, move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO capture_garbage (area, name, bytes, found_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![area.as_str(), name, bytes, now],
            )?;
            Ok(())
        })
        .await
    }

    /// Deletes the garbage row of an entry that is removed durably.
    ///
    /// # Errors
    /// A database error.
    pub async fn garbage_delete(&self, area: GarbageArea, name: &str) -> Result<()> {
        let name = name.to_owned();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "DELETE FROM capture_garbage WHERE area = ?1 AND name = ?2",
                params![area.as_str(), name],
            )?;
            Ok(())
        })
        .await
    }

    /// The garbage rows that are still counted, as `(area, name, bytes)`.
    ///
    /// # Errors
    /// A database error.
    pub async fn garbage_list(&self) -> Result<Vec<(GarbageArea, String, u64)>> {
        call(&self.inner.db, move |conn| {
            let mut stmt =
                conn.prepare("SELECT area, name, bytes FROM capture_garbage ORDER BY area, name")?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|(area, name, bytes)| Ok((GarbageArea::parse(&area)?, name, to_u64(bytes)?)))
                .collect()
        })
        .await
    }
}

#[cfg(test)]
#[path = "recording_ops_tests.rs"]
mod tests;
