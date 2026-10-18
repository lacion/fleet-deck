//! The deck record of the owner screenshots (R13 P5a task E, design
//! `docs/v2/design/omarchy-p5a.md` §6.2, §6.3 and §4).
//!
//! One row of `desktop_captures` for each `(station, capture id)`, kept for
//! ever (about 200 bytes). The row is committed as `prepared` before the
//! deck sends, becomes `attempted` before the network call, and the deck never
//! sends a capture id that it recorded. `deleted` is not stored: a tombstone
//! of the published artifact wins in every answer ([`CaptureRow::deleted`]).
//!
//! [`DeckStore::complete_capture`] stores the station result and indexes the
//! artifact, its version, its provenance and the row state in one
//! transaction, with the tombstone check inside it.
//!
//! The capture cache is the part of `artifact_cache` with `capture = 1`: it
//! holds at most [`CAPTURE_CACHE_LIMIT`] payload bytes. [`cache_put_tx`]
//! inserts a file and evicts the capture files with the oldest `last_used_at`
//! in the same transaction. It never deletes an artifact or a version row.

use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::ArtifactPublished;
use fleetdeck_proto::capture::CaptureProvenance;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::artifact_tombstones::tombstoned;
use super::artifacts::{note_artifact_tx, put_artifact_versions_tx};
use super::{DeckError, DeckStore, Result, call};

/// The migration (V32).
pub(super) const SQL: &str = "
CREATE TABLE desktop_captures (          -- R13 P5a: one row per owner screenshot, kept forever
    station_id  TEXT    NOT NULL,
    capture_id  TEXT    NOT NULL,
    action_hash TEXT    NOT NULL,
    state       TEXT    NOT NULL,        -- prepared, attempted, committed, failed, unknown
    monitor     TEXT    NOT NULL,
    published   TEXT,                    -- ArtifactPublished JSON (station result, before or after indexing)
    indexed     INTEGER NOT NULL DEFAULT 0,
    code        TEXT,                    -- failed: the station code
    message     TEXT,                    -- failed: the fixed text
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (station_id, capture_id)
) STRICT;
ALTER TABLE artifact_versions ADD COLUMN capture TEXT;          -- CaptureProvenance JSON
ALTER TABLE artifact_cache ADD COLUMN capture INTEGER NOT NULL DEFAULT 0;
ALTER TABLE artifact_cache ADD COLUMN last_used_at INTEGER NOT NULL DEFAULT 0;
";

/// The most payload bytes that the capture cache holds (256 MiB).
pub const CAPTURE_CACHE_LIMIT: u64 = 268_435_456;

/// The time after which an `attempted` capture that the station does not
/// know becomes the terminal `unknown` (24 h).
pub const UNKNOWN_AFTER_MS: i64 = 24 * 60 * 60 * 1000;

/// The send state of a capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureState {
    /// Committed; the deck has not begun to send.
    Prepared,
    /// Committed right before the network call. A crash after this point
    /// leaves the outcome unknown: the deck never sends it again.
    Attempted,
    /// The station committed the capture.
    Committed,
    /// The station refused or failed the capture.
    Failed,
    /// No station result is known, and none will be asked for.
    Unknown,
}

impl CaptureState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Attempted => "attempted",
            Self::Committed => "committed",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "prepared" => Self::Prepared,
            "attempted" => Self::Attempted,
            "committed" => Self::Committed,
            "failed" => Self::Failed,
            "unknown" => Self::Unknown,
            other => return Err(DeckError::Corrupt(format!("capture state {other}"))),
        })
    }
}

/// One row of `desktop_captures`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureRow {
    pub station_id: StationId,
    pub capture_id: String,
    pub action_hash: String,
    pub state: CaptureState,
    pub monitor: String,
    /// The station result, normalized (see [`stamp_published`]).
    pub published: Option<ArtifactPublished>,
    pub indexed: bool,
    pub code: Option<String>,
    pub message: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// True when the artifact of `published` has a tombstone.
    pub deleted: bool,
    /// The compact JSON of the `CoordinatorContext` of a coordinator capture
    /// (R13 P6 §4.7); `None` for an owner capture. A value is never an owner
    /// capture, whatever it holds.
    pub coordinator: Option<String>,
}

impl CaptureRow {
    /// True while the deck asks the station for the result: `attempted`, or
    /// `committed` and not indexed, and not deleted.
    #[must_use]
    pub fn unresolved(&self) -> bool {
        !self.deleted
            && (self.state == CaptureState::Attempted
                || (self.state == CaptureState::Committed && !self.indexed))
    }
}

/// The answer of [`DeckStore::capture_prepare`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prepared {
    /// A new row: the caller sends.
    New,
    /// The id exists with another action hash.
    Conflict,
    /// The id exists with the same hash: the caller sends nothing.
    Existing(Box<CaptureRow>),
}

/// The answer of [`DeckStore::complete_capture`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completed {
    /// The artifact, its version and its provenance are indexed.
    Indexed,
    /// The artifact has a tombstone: nothing is indexed.
    Deleted,
}

/// `published` with the deck's stable station id in the artifact and
/// `provenance` (with the same station id) in the version.
#[must_use]
pub fn stamp_published(
    station: &StationId,
    mut published: ArtifactPublished,
    provenance: &CaptureProvenance,
) -> ArtifactPublished {
    station
        .as_str()
        .clone_into(&mut published.artifact.station_id);
    let mut provenance = provenance.clone();
    station.as_str().clone_into(&mut provenance.station_id);
    published.version.capture = Some(provenance);
    published
}

const COLUMNS: &str = "c.station_id, c.capture_id, c.action_hash, c.state, c.monitor,
    c.published, c.indexed, c.code, c.message, c.created_at, c.updated_at,
    EXISTS (SELECT 1 FROM artifact_tombstones t
            WHERE t.station_id = c.station_id
              AND t.artifact_id = json_extract(c.published, '$.artifact.artifact_id')),
    c.coordinator";

struct Raw {
    station: String,
    capture_id: String,
    action_hash: String,
    state: String,
    monitor: String,
    published: Option<String>,
    indexed: i64,
    code: Option<String>,
    message: Option<String>,
    created_at: i64,
    updated_at: i64,
    deleted: bool,
    coordinator: Option<String>,
}

fn raw_row(r: &Row<'_>) -> rusqlite::Result<Raw> {
    Ok(Raw {
        station: r.get(0)?,
        capture_id: r.get(1)?,
        action_hash: r.get(2)?,
        state: r.get(3)?,
        monitor: r.get(4)?,
        published: r.get(5)?,
        indexed: r.get(6)?,
        code: r.get(7)?,
        message: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
        deleted: r.get(11)?,
        coordinator: r.get(12)?,
    })
}

fn row_of(raw: Raw) -> Result<CaptureRow> {
    Ok(CaptureRow {
        station_id: StationId::from(raw.station),
        capture_id: raw.capture_id,
        action_hash: raw.action_hash,
        state: CaptureState::parse(&raw.state)?,
        monitor: raw.monitor,
        published: raw
            .published
            .map(|p| serde_json::from_str(&p))
            .transpose()?,
        indexed: raw.indexed != 0,
        code: raw.code,
        message: raw.message,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
        deleted: raw.deleted,
        coordinator: raw.coordinator,
    })
}

pub(super) fn get_row(
    conn: &Connection,
    station: &str,
    capture_id: &str,
) -> Result<Option<CaptureRow>> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM desktop_captures c
             WHERE c.station_id = ?1 AND c.capture_id = ?2"
        ),
        params![station, capture_id],
        raw_row,
    )
    .optional()?
    .map(row_of)
    .transpose()
}

/// Inserts one cache file in the caller's transaction (the tombstone check
/// is the caller's). A version with a `capture` value makes a capture row,
/// and the capture rows are then cut down to `limit` bytes: the oldest
/// `last_used_at` goes first (the oldest insert on a tie). A capture file
/// larger than `limit` is not cached.
pub(super) fn cache_put_tx(
    tx: &Transaction<'_>,
    key: (&str, &str, u32, &str),
    bytes: &[u8],
    now: i64,
    limit: u64,
) -> Result<()> {
    let (station, artifact_id, version, path) = key;
    let capture = tx
        .query_row(
            "SELECT capture IS NOT NULL FROM artifact_versions
             WHERE station_id = ?1 AND artifact_id = ?2 AND version = ?3",
            params![station, artifact_id, version],
            |r| r.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false);
    if capture && u64::try_from(bytes.len()).map_or(true, |n| n > limit) {
        return Ok(());
    }
    tx.execute(
        "INSERT OR REPLACE INTO artifact_cache
             (station_id, artifact_id, version, path, bytes, capture, last_used_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![station, artifact_id, version, path, bytes, capture, now],
    )?;
    if capture {
        evict_tx(tx, key, limit)?;
    }
    Ok(())
}

/// Deletes capture cache rows, oldest `last_used_at` first, until the sum of
/// their payload is at most `limit`. The row `keep` stays.
fn evict_tx(tx: &Transaction<'_>, keep: (&str, &str, u32, &str), limit: u64) -> Result<()> {
    let mut total: i64 = tx.query_row(
        "SELECT COALESCE(SUM(length(bytes)), 0) FROM artifact_cache WHERE capture = 1",
        [],
        |r| r.get(0),
    )?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    if total <= limit {
        return Ok(());
    }
    let rows = {
        let mut stmt = tx.prepare(
            "SELECT rowid, length(bytes) FROM artifact_cache
             WHERE capture = 1
               AND NOT (station_id = ?1 AND artifact_id = ?2 AND version = ?3 AND path = ?4)
             ORDER BY last_used_at, rowid",
        )?;
        stmt.query_map(params![keep.0, keep.1, keep.2, keep.3], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (rowid, len) in rows {
        if total <= limit {
            break;
        }
        tx.execute("DELETE FROM artifact_cache WHERE rowid = ?1", [rowid])?;
        total -= len;
    }
    Ok(())
}

impl DeckStore {
    /// Records a new capture as `prepared` in one transaction: no row inserts
    /// one; a row with another action hash is a conflict; a row with the
    /// same hash is returned and nothing sends again.
    ///
    /// # Errors
    /// A database error.
    pub async fn capture_prepare(
        &self,
        station: StationId,
        capture_id: String,
        action_hash: String,
        monitor: String,
    ) -> Result<Prepared> {
        self.prepare(station, capture_id, action_hash, monitor, None)
            .await
    }

    /// As [`Self::capture_prepare`] for a coordinator capture (R13 P6): the
    /// `coordinator` column holds `coordinator`, the compact JSON of the
    /// `CoordinatorContext`.
    ///
    /// # Errors
    /// A database error.
    pub async fn capture_prepare_coordinator(
        &self,
        station: StationId,
        capture_id: String,
        action_hash: String,
        monitor: String,
        coordinator: String,
    ) -> Result<Prepared> {
        self.prepare(station, capture_id, action_hash, monitor, Some(coordinator))
            .await
    }

    async fn prepare(
        &self,
        station: StationId,
        capture_id: String,
        action_hash: String,
        monitor: String,
        coordinator: Option<String>,
    ) -> Result<Prepared> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if let Some(row) = get_row(&tx, station.as_str(), &capture_id)? {
                return Ok(if row.action_hash == action_hash {
                    Prepared::Existing(Box::new(row))
                } else {
                    Prepared::Conflict
                });
            }
            tx.execute(
                "INSERT INTO desktop_captures
                     (station_id, capture_id, action_hash, state, monitor, created_at, updated_at,
                      coordinator)
                 VALUES (?1, ?2, ?3, 'prepared', ?4, ?5, ?5, ?6)",
                params![
                    station.as_str(),
                    capture_id,
                    action_hash,
                    monitor,
                    now,
                    coordinator
                ],
            )?;
            tx.commit()?;
            Ok(Prepared::New)
        })
        .await
    }

    /// `prepared` becomes `attempted`: committed before the network call.
    ///
    /// # Errors
    /// A database error.
    pub async fn capture_attempted(&self, station: StationId, capture_id: String) -> Result<()> {
        self.capture_move(station, capture_id, "attempted", &["prepared"], None)
            .await
    }

    /// The station failed the capture with `code` and the fixed `message`.
    /// A row that the station committed does not change.
    ///
    /// # Errors
    /// A database error.
    pub async fn capture_failed(
        &self,
        station: StationId,
        capture_id: String,
        code: String,
        message: String,
    ) -> Result<()> {
        self.capture_move(
            station,
            capture_id,
            "failed",
            &["prepared", "attempted"],
            Some((code, message)),
        )
        .await
    }

    /// The terminal `unknown` of an `attempted` capture.
    ///
    /// # Errors
    /// A database error.
    pub async fn capture_unknown(&self, station: StationId, capture_id: String) -> Result<()> {
        self.capture_move(station, capture_id, "unknown", &["attempted"], None)
            .await
    }

    /// At deck start: every `prepared` row becomes `unknown` (§6.2). A row
    /// is `attempted` before each send, so a `prepared` row after a restart
    /// was never sent; it must not answer `pending` for ever, and it is
    /// never sent again. Returns the number of rows.
    ///
    /// # Errors
    /// A database error.
    pub async fn captures_prepared_to_unknown(&self) -> Result<usize> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            Ok(conn.execute(
                "UPDATE desktop_captures SET state = 'unknown', updated_at = ?1
                 WHERE state = 'prepared'",
                params![now],
            )?)
        })
        .await
    }

    async fn capture_move(
        &self,
        station: StationId,
        capture_id: String,
        to: &'static str,
        from: &'static [&'static str],
        failure: Option<(String, String)>,
    ) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let (code, message) = failure.unzip();
            let from = from
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(",");
            conn.execute(
                &format!(
                    "UPDATE desktop_captures
                     SET state = ?3, code = COALESCE(?4, code), message = COALESCE(?5, message),
                         updated_at = ?6
                     WHERE station_id = ?1 AND capture_id = ?2 AND state IN ({from})"
                ),
                params![station.as_str(), capture_id, to, code, message, now],
            )?;
            Ok(())
        })
        .await
    }

    /// Keeps the station result in the row as `committed` and not indexed:
    /// the recovery indexes it later. Used when [`Self::complete_capture`]
    /// failed after the station commit. An indexed row does not change.
    ///
    /// # Errors
    /// A database error.
    pub async fn capture_keep_published(
        &self,
        station: StationId,
        capture_id: String,
        published: ArtifactPublished,
    ) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE desktop_captures
                 SET state = 'committed', published = ?3, updated_at = ?4
                 WHERE station_id = ?1 AND capture_id = ?2 AND indexed = 0",
                params![
                    station.as_str(),
                    capture_id,
                    serde_json::to_string(&published)?,
                    now
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Completes a capture in one transaction: stores `published` (with the
    /// deck's station id and `provenance`); when the artifact has a tombstone
    /// it stays unindexed; else it indexes the artifact and its version,
    /// writes the version's `capture` column, and sets the row to
    /// `committed` and indexed.
    ///
    /// # Errors
    /// A database error, or an unknown capture row. A failure rolls back
    /// everything: the caller keeps the result with
    /// [`Self::capture_keep_published`].
    pub async fn complete_capture(
        &self,
        station: StationId,
        capture_id: String,
        published: ArtifactPublished,
        mut provenance: CaptureProvenance,
    ) -> Result<Completed> {
        let now = self.now_ms();
        station.as_str().clone_into(&mut provenance.station_id);
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(row) = get_row(&tx, station.as_str(), &capture_id)? else {
                return Err(DeckError::Invalid(format!("unknown capture {capture_id}")));
            };
            let stamped = stamp_published(&station, published, &provenance);
            let json = serde_json::to_string(&stamped)?;
            let artifact_id = stamped.artifact.artifact_id.clone();
            let done = if tombstoned(&tx, station.as_str(), &artifact_id)? {
                tx.execute(
                    "UPDATE desktop_captures
                     SET state = 'committed', published = ?3, indexed = 0, updated_at = ?4
                     WHERE station_id = ?1 AND capture_id = ?2",
                    params![station.as_str(), capture_id, json, now],
                )?;
                Completed::Deleted
            } else if row.indexed {
                return Ok(Completed::Indexed);
            } else {
                note_artifact_tx(&tx, &station, &stamped.artifact)?;
                put_artifact_versions_tx(&tx, &station, std::slice::from_ref(&stamped.version))?;
                tx.execute(
                    "UPDATE artifact_versions SET capture = ?4
                     WHERE station_id = ?1 AND artifact_id = ?2 AND version = ?3",
                    params![
                        station.as_str(),
                        artifact_id,
                        stamped.version.version,
                        serde_json::to_string(&provenance)?
                    ],
                )?;
                tx.execute(
                    "UPDATE desktop_captures
                     SET state = 'committed', published = ?3, indexed = 1, updated_at = ?4
                     WHERE station_id = ?1 AND capture_id = ?2",
                    params![station.as_str(), capture_id, json, now],
                )?;
                Completed::Indexed
            };
            tx.commit()?;
            Ok(done)
        })
        .await
    }

    /// One capture row.
    ///
    /// # Errors
    /// A database error.
    pub async fn capture_get(
        &self,
        station: StationId,
        capture_id: String,
    ) -> Result<Option<CaptureRow>> {
        call(&self.inner.db, move |conn| {
            get_row(conn, station.as_str(), &capture_id)
        })
        .await
    }

    /// The coordinator capture whose artifact is `(station, artifact_id)`
    /// (R13 P6 §4.9): a row with a `coordinator` value only. An owner capture
    /// is never returned.
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_capture_by_artifact(
        &self,
        station: StationId,
        artifact_id: String,
    ) -> Result<Option<CaptureRow>> {
        call(&self.inner.db, move |conn| {
            conn.query_row(
                &format!(
                    "SELECT {COLUMNS} FROM desktop_captures c
                     WHERE c.station_id = ?1 AND c.coordinator IS NOT NULL
                       AND json_extract(c.published, '$.artifact.artifact_id') = ?2
                     ORDER BY c.created_at, c.capture_id LIMIT 1"
                ),
                params![station.as_str(), artifact_id],
                raw_row,
            )
            .optional()?
            .map(row_of)
            .transpose()
        })
        .await
    }

    /// The captures whose result the deck still asks the station for:
    /// `attempted`, or `committed` and not indexed, with no tombstone.
    ///
    /// # Errors
    /// A database error.
    pub async fn captures_unresolved(&self) -> Result<Vec<CaptureRow>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM desktop_captures c
                 WHERE c.state = 'attempted' OR (c.state = 'committed' AND c.indexed = 0)
                 ORDER BY c.created_at, c.capture_id"
            ))?;
            let raw = stmt
                .query_map([], raw_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let rows = raw.into_iter().map(row_of).collect::<Result<Vec<_>>>()?;
            Ok(rows.into_iter().filter(CaptureRow::unresolved).collect())
        })
        .await
    }
}

#[cfg(test)]
#[path = "desktop_captures_tests.rs"]
mod tests;
