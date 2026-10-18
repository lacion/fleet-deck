//! The V35 migration (R13 P5b, task F): the recording columns of
//! `desktop_captures`. Forward only: no existing row is rewritten, and every
//! old row reads `operation_kind = 'screenshot'`. To reverse it, drop the
//! index and the added columns.
//!
//! The functions (task K1): the recording row, its `prepared` insert, the stop
//! latch, the first-start mark and the terminal-audit mark. A recording is a
//! row of `desktop_captures`, so the P5a functions (`capture_attempted`,
//! `capture_failed`, `complete_capture`) apply to it unchanged.

use fleetdeck_proto::StationId;
use fleetdeck_proto::recording::StopReason;
use rusqlite::{Connection, params};

use super::desktop_captures::{CaptureRow, CaptureState, Prepared, get_row};
use super::{DeckError, DeckStore, Result, call};

/// The migration (V35).
pub(crate) const SQL: &str = "
ALTER TABLE desktop_captures ADD COLUMN operation_kind TEXT NOT NULL DEFAULT 'screenshot'
  CHECK (operation_kind IN ('screenshot','recording'));
ALTER TABLE desktop_captures ADD COLUMN requested_seconds INTEGER
  CHECK (requested_seconds IS NULL OR requested_seconds BETWEEN 1 AND 120);
ALTER TABLE desktop_captures ADD COLUMN owner_login TEXT;      -- login_sessions.session_hash of the initiator
ALTER TABLE desktop_captures ADD COLUMN link_epoch TEXT;       -- Route.epoch at dispatch
ALTER TABLE desktop_captures ADD COLUMN stop_intent TEXT NOT NULL DEFAULT 'none'
  CHECK (stop_intent IN ('none','save','discard'));
ALTER TABLE desktop_captures ADD COLUMN stop_reason TEXT;
ALTER TABLE desktop_captures ADD COLUMN started_at INTEGER;    -- first record-phase lease (start-audit dedupe)
ALTER TABLE desktop_captures ADD COLUMN audited_terminal INTEGER NOT NULL DEFAULT 0
  CHECK (audited_terminal IN (0,1));
CREATE INDEX desktop_captures_recording ON desktop_captures (station_id, state)
  WHERE operation_kind = 'recording';
";

/// The kind of a capture operation (`desktop_captures.operation_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Screenshot,
    Recording,
}

impl OperationKind {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "screenshot" => Ok(Self::Screenshot),
            "recording" => Ok(Self::Recording),
            other => Err(DeckError::Corrupt(format!("operation kind {other}"))),
        }
    }
}

/// The stop intent of a recording (`desktop_captures.stop_intent`): a
/// monotonic latch `none -> save -> discard`, and `none -> discard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopIntent {
    None,
    Save,
    Discard,
}

impl StopIntent {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Save => "save",
            Self::Discard => "discard",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "none" => Ok(Self::None),
            "save" => Ok(Self::Save),
            "discard" => Ok(Self::Discard),
            other => Err(DeckError::Corrupt(format!("stop intent {other}"))),
        }
    }
}

/// One row of `desktop_captures` with its recording columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingRow {
    pub capture: CaptureRow,
    pub kind: OperationKind,
    pub requested_seconds: Option<u32>,
    /// The `session_hash` of the login that started the recording.
    pub owner_login: Option<String>,
    /// The `Hello.desktop_epoch` of the link that carried the start.
    pub link_epoch: Option<String>,
    pub stop_intent: StopIntent,
    pub stop_reason: Option<StopReason>,
    /// Deck Unix ms of the first `record` lease that answered `continue`.
    pub started_at: Option<i64>,
    pub audited_terminal: bool,
}

/// What [`DeckStore::recording_prepare`] stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRecording {
    pub station: StationId,
    pub capture_id: String,
    pub action_hash: String,
    pub monitor: String,
    pub seconds: u32,
    pub owner_login: String,
    pub link_epoch: String,
}

/// The wire string of a stop reason (snake case).
#[must_use]
pub fn reason_str(r: StopReason) -> String {
    serde_json::to_value(r)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn parse_reason(s: &str) -> Result<StopReason> {
    serde_json::from_value(serde_json::Value::String(s.to_owned()))
        .map_err(|_| DeckError::Corrupt(format!("stop reason {s}")))
}

/// The recording columns of one row.
struct Extra {
    kind: String,
    seconds: Option<i64>,
    login: Option<String>,
    epoch: Option<String>,
    intent: String,
    reason: Option<String>,
    started: Option<i64>,
    audited: i64,
}

fn get_recording(
    conn: &Connection,
    station: &str,
    capture_id: &str,
) -> Result<Option<RecordingRow>> {
    let Some(capture) = get_row(conn, station, capture_id)? else {
        return Ok(None);
    };
    let extra = conn.query_row(
        "SELECT operation_kind, requested_seconds, owner_login, link_epoch, stop_intent,
                stop_reason, started_at, audited_terminal
         FROM desktop_captures WHERE station_id = ?1 AND capture_id = ?2",
        params![station, capture_id],
        |r| {
            Ok(Extra {
                kind: r.get(0)?,
                seconds: r.get(1)?,
                login: r.get(2)?,
                epoch: r.get(3)?,
                intent: r.get(4)?,
                reason: r.get(5)?,
                started: r.get(6)?,
                audited: r.get(7)?,
            })
        },
    )?;
    Ok(Some(RecordingRow {
        capture,
        kind: OperationKind::parse(&extra.kind)?,
        requested_seconds: extra.seconds.and_then(|n| u32::try_from(n).ok()),
        owner_login: extra.login,
        link_epoch: extra.epoch,
        stop_intent: StopIntent::parse(&extra.intent)?,
        stop_reason: extra.reason.as_deref().map(parse_reason).transpose()?,
        started_at: extra.started,
        audited_terminal: extra.audited != 0,
    }))
}

impl DeckStore {
    /// Records a new owner recording as `prepared` in one transaction: no row
    /// inserts one; a row with another action hash is a conflict; a row with
    /// the same hash is returned and nothing sends again.
    ///
    /// # Errors
    /// A database error.
    pub async fn recording_prepare(&self, new: NewRecording) -> Result<Prepared> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if let Some(row) = get_row(&tx, new.station.as_str(), &new.capture_id)? {
                return Ok(if row.action_hash == new.action_hash {
                    Prepared::Existing(Box::new(row))
                } else {
                    Prepared::Conflict
                });
            }
            tx.execute(
                "INSERT INTO desktop_captures
                     (station_id, capture_id, action_hash, state, monitor, created_at, updated_at,
                      operation_kind, requested_seconds, owner_login, link_epoch)
                 VALUES (?1, ?2, ?3, 'prepared', ?4, ?5, ?5, 'recording', ?6, ?7, ?8)",
                params![
                    new.station.as_str(),
                    new.capture_id,
                    new.action_hash,
                    new.monitor,
                    now,
                    new.seconds,
                    new.owner_login,
                    new.link_epoch
                ],
            )?;
            tx.commit()?;
            Ok(Prepared::New)
        })
        .await
    }

    /// One capture row with its recording columns. Any row answers, a
    /// screenshot too (`kind`).
    ///
    /// # Errors
    /// A database error.
    pub async fn recording_get(
        &self,
        station: StationId,
        capture_id: String,
    ) -> Result<Option<RecordingRow>> {
        call(&self.inner.db, move |conn| {
            get_recording(conn, station.as_str(), &capture_id)
        })
        .await
    }

    /// Commits the stop intent of a recording before any send, with the latch
    /// rules in one transaction: a stored `discard` is never replaced, a
    /// stored `save` is replaced only by a `discard`, and only a recording that
    /// is `prepared` or `attempted` changes. `updated_at` does not change (it
    /// is the time of the `attempted` state). Returns the row as it is after
    /// the call; `None` when no such capture exists.
    ///
    /// # Errors
    /// A database error.
    pub async fn recording_stop_intent(
        &self,
        station: StationId,
        capture_id: String,
        save: bool,
        reason: StopReason,
    ) -> Result<Option<RecordingRow>> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(row) = get_recording(&tx, station.as_str(), &capture_id)? else {
                return Ok(None);
            };
            let open = matches!(
                row.capture.state,
                CaptureState::Prepared | CaptureState::Attempted
            );
            let replaces = match (row.stop_intent, save) {
                (StopIntent::Discard, _) | (StopIntent::Save, true) => false,
                (StopIntent::Save, false) | (StopIntent::None, _) => true,
            };
            if row.kind == OperationKind::Recording && open && replaces {
                let intent = if save {
                    StopIntent::Save
                } else {
                    StopIntent::Discard
                };
                tx.execute(
                    "UPDATE desktop_captures SET stop_intent = ?3, stop_reason = ?4
                     WHERE station_id = ?1 AND capture_id = ?2",
                    params![
                        station.as_str(),
                        capture_id,
                        intent.as_str(),
                        reason_str(reason)
                    ],
                )?;
            }
            let fresh = get_recording(&tx, station.as_str(), &capture_id)?;
            tx.commit()?;
            Ok(fresh)
        })
        .await
    }

    /// Sets `started_at` of an `attempted` recording once: true only for the
    /// call that set it (that call writes the start audit row).
    ///
    /// # Errors
    /// A database error.
    pub async fn recording_started(&self, station: StationId, capture_id: String) -> Result<bool> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            Ok(conn.execute(
                "UPDATE desktop_captures SET started_at = ?3
                 WHERE station_id = ?1 AND capture_id = ?2 AND operation_kind = 'recording'
                   AND state = 'attempted' AND started_at IS NULL",
                params![station.as_str(), capture_id, now],
            )? == 1)
        })
        .await
    }

    /// Sets `audited_terminal` of a recording once: true only for the call
    /// that set it (that call writes the terminal audit row).
    ///
    /// # Errors
    /// A database error.
    pub async fn recording_audited_terminal(
        &self,
        station: StationId,
        capture_id: String,
    ) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            Ok(conn.execute(
                "UPDATE desktop_captures SET audited_terminal = 1
                 WHERE station_id = ?1 AND capture_id = ?2 AND operation_kind = 'recording'
                   AND audited_terminal = 0",
                params![station.as_str(), capture_id],
            )? == 1)
        })
        .await
    }
}
