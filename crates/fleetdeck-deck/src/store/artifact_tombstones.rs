//! The permanent artifact tombstones (R13 P5a task C, A4 deck side,
//! `docs/v2/design/omarchy-p5a.md` §3).
//!
//! A tombstone names `(station id, artifact id)`. It is never deleted: a
//! station acknowledgement only moves it from `pending` to `deleted`, and a
//! removed or re-enrolled station does not touch it (no foreign key). One
//! transaction ([`DeckStore::artifact_delete`]) inserts the tombstone and
//! removes every deck copy of the artifact. Every artifact write checks
//! [`tombstoned`] inside its own transaction, and every read leaves a
//! tombstoned identity out.

use fleetdeck_proto::StationId;
use rusqlite::{Connection, OptionalExtension, params};

use super::{DeckStore, Result, call};

/// The migration (V31).
pub(super) const SQL: &str = "
CREATE TABLE artifact_tombstones (       -- R13 P5a A4: permanent, never deleted
    station_id  TEXT    NOT NULL,        -- the stable station id; no foreign key
    artifact_id TEXT    NOT NULL,
    state       TEXT    NOT NULL,        -- pending, deleted
    reason      TEXT,                    -- offline, unsupported, retry (pending only)
    attempts    INTEGER NOT NULL DEFAULT 0,
    next_at     INTEGER,                 -- unix ms of the next timed try (reason retry)
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (station_id, artifact_id)
) STRICT;
";

/// True when `(station, artifact_id)` has a tombstone: one indexed lookup.
///
/// # Errors
/// A database error.
pub(crate) fn tombstoned(
    conn: &Connection,
    station: &str,
    artifact_id: &str,
) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM artifact_tombstones WHERE station_id = ?1 AND artifact_id = ?2",
            params![station, artifact_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// The current state of a tombstone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneView {
    /// `pending` or `deleted`.
    pub state: String,
    /// `offline`, `unsupported` or `retry` while `pending`.
    pub reason: Option<String>,
}

/// What [`DeckStore::artifact_delete`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteStart {
    /// A new tombstone and the removal of every deck copy.
    Created,
    /// A tombstone existed: its current state, nothing changed.
    Existing(TombstoneView),
    /// No tombstone and no index row.
    Unknown,
}

/// The result of one station deletion try.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryOutcome {
    Deleted,
    Offline,
    Unsupported,
    Retry,
}

/// When a pending tombstone is tried again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TombstoneDue {
    /// Reason `offline` or `unsupported`: tried once on each new link.
    OnLink,
    /// Reason `retry` and `next_at` is empty or has passed.
    Due,
    /// Reason `retry` and `next_at` is in the future.
    Waiting,
}

/// The delay after a failed try, in ms. `attempts` is the value before the
/// failure is counted: the first failure waits 1 s, then 2, 4, 8, 16, 32 s,
/// then 60 s.
#[must_use]
pub fn retry_delay_ms(attempts: u32) -> i64 {
    match attempts {
        0 => 1_000,
        1 => 2_000,
        2 => 4_000,
        3 => 8_000,
        4 => 16_000,
        5 => 32_000,
        _ => 60_000,
    }
}

fn view_of(conn: &Connection, station: &str, artifact_id: &str) -> Result<Option<TombstoneView>> {
    Ok(conn
        .query_row(
            "SELECT state, reason FROM artifact_tombstones
             WHERE station_id = ?1 AND artifact_id = ?2",
            params![station, artifact_id],
            |r| {
                Ok(TombstoneView {
                    state: r.get(0)?,
                    reason: r.get(1)?,
                })
            },
        )
        .optional()?)
}

impl DeckStore {
    /// Starts the deletion of an artifact in one transaction: when a
    /// tombstone exists the answer is [`DeleteStart::Existing`]; else when
    /// the index row exists it inserts the tombstone (`pending`, reason
    /// `retry`) and deletes every row of the artifact from `artifacts`,
    /// `artifact_versions`, `artifact_cache` and `artifact_shares`.
    ///
    /// # Errors
    /// A database error.
    pub async fn artifact_delete(
        &self,
        station: StationId,
        artifact_id: String,
    ) -> Result<DeleteStart> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let (st, id) = (station.as_str(), artifact_id.as_str());
            if let Some(view) = view_of(&tx, st, id)? {
                return Ok(DeleteStart::Existing(view));
            }
            let indexed = tx
                .query_row(
                    "SELECT 1 FROM artifacts WHERE station_id = ?1 AND artifact_id = ?2",
                    params![st, id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !indexed {
                return Ok(DeleteStart::Unknown);
            }
            tx.execute(
                "INSERT INTO artifact_tombstones
                     (station_id, artifact_id, state, reason, attempts, next_at,
                      created_at, updated_at)
                 VALUES (?1, ?2, 'pending', 'retry', 0, NULL, ?3, ?3)",
                params![st, id, now],
            )?;
            for table in [
                "artifacts",
                "artifact_versions",
                "artifact_cache",
                "artifact_shares",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE station_id = ?1 AND artifact_id = ?2"),
                    params![st, id],
                )?;
            }
            tx.commit()?;
            Ok(DeleteStart::Created)
        })
        .await
    }

    /// The tombstone of an identity, if any.
    ///
    /// # Errors
    /// A database error.
    pub async fn tombstone(
        &self,
        station: StationId,
        artifact_id: String,
    ) -> Result<Option<TombstoneView>> {
        call(&self.inner.db, move |conn| {
            view_of(conn, station.as_str(), &artifact_id)
        })
        .await
    }

    /// Records the result of one station try at `now` (unix ms). A `deleted`
    /// row never goes back to `pending`; an unknown identity is ignored.
    ///
    /// # Errors
    /// A database error.
    pub async fn tombstone_set(
        &self,
        station: StationId,
        artifact_id: String,
        outcome: TryOutcome,
        now: i64,
    ) -> Result<()> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let (st, id) = (station.as_str(), artifact_id.as_str());
            let row: Option<(String, u32)> = tx
                .query_row(
                    "SELECT state, attempts FROM artifact_tombstones
                     WHERE station_id = ?1 AND artifact_id = ?2",
                    params![st, id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((state, attempts)) = row else {
                return Ok(());
            };
            if state == "deleted" {
                return Ok(());
            }
            let (state, reason, attempts, next_at): (&str, Option<&str>, u32, Option<i64>) =
                match outcome {
                    TryOutcome::Deleted => ("deleted", None, attempts, None),
                    TryOutcome::Offline => ("pending", Some("offline"), attempts, None),
                    TryOutcome::Unsupported => ("pending", Some("unsupported"), attempts, None),
                    TryOutcome::Retry => (
                        "pending",
                        Some("retry"),
                        attempts.saturating_add(1),
                        Some(now.saturating_add(retry_delay_ms(attempts))),
                    ),
                };
            tx.execute(
                "UPDATE artifact_tombstones
                 SET state = ?3, reason = ?4, attempts = ?5, next_at = ?6, updated_at = ?7
                 WHERE station_id = ?1 AND artifact_id = ?2",
                params![st, id, state, reason, attempts, next_at, now],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// The pending tombstones of a station, oldest first, with whether each
    /// one waits for a link or for a timed retry. A `retry` row with no
    /// `next_at` (the handler ended before it recorded a result) is due.
    ///
    /// # Errors
    /// A database error.
    pub async fn tombstones_pending(
        &self,
        station: StationId,
    ) -> Result<Vec<(String, TombstoneDue)>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT artifact_id, reason, next_at FROM artifact_tombstones
                 WHERE station_id = ?1 AND state = 'pending'
                 ORDER BY created_at, artifact_id",
            )?;
            let rows = stmt
                .query_map([station.as_str()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows
                .into_iter()
                .map(|(id, reason, next_at)| {
                    let due = match reason.as_deref() {
                        Some("offline" | "unsupported") => TombstoneDue::OnLink,
                        _ if next_at.is_none_or(|t| t <= now) => TombstoneDue::Due,
                        _ => TombstoneDue::Waiting,
                    };
                    (id, due)
                })
                .collect())
        })
        .await
    }
}

#[cfg(test)]
#[path = "artifact_tombstones_tests.rs"]
mod tests;
