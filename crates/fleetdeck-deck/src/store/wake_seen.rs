//! Restart-safe wake deduplication (R13 §5F item 4, §5G item 2).
//!
//! One operation, [`DeckStore::wake_reserve`], decides in one transaction
//! whether a request may wake a wall. Only `Reserved` (one row inserted and
//! committed) may lead to a wake. A request that was seen, a suppressed
//! state, a write error and an unknown outcome never wake.
//!
//! The steps, in order:
//! 1. the persisted suppression state (`wake_state.suspended_until_ms`);
//! 2. rows older than 24 hours are deleted;
//! 3. while the table holds 10 000 rows or more, the oldest rows that are
//!    older than 120 seconds are deleted;
//! 4. when the table still holds 10 000 rows (all younger than 120 s), the
//!    suppression state is set to now plus 120 s and the answer is
//!    `Suppressed`;
//! 5. `INSERT ... ON CONFLICT DO NOTHING`, then the tombstone check: one row
//!    inserted and no tombstone is `Reserved`; else `Seen`.
//!
//! The key is `(station_id, session_id, request_id)` (V49, G5): request ids
//! are per session. A V48 row is a legacy tombstone (session `''`): it makes
//! its request id `Seen` in every session of its station until it ages out.
//!
//! A row younger than 120 s is never evicted: 120 s is longer than the 60 s
//! window of a new request plus the 5 s skew.

use rusqlite::{Connection, params};

use super::{DeckStore, Result, call};

/// A row is kept for 24 hours.
pub const WAKE_SEEN_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// The table holds at most this many rows.
pub const WAKE_SEEN_MAX: i64 = 10_000;
/// A row younger than this is never evicted.
pub const WAKE_SEEN_PROTECT_MS: i64 = 120_000;
/// How long a saturated table suppresses every wake.
pub const WAKE_SUSPEND_MS: i64 = 120_000;

/// The answer of [`DeckStore::wake_reserve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReserve {
    /// One row was inserted and committed: the only answer that may wake.
    Reserved,
    /// The request was observed before.
    Seen,
    /// The persisted suppression state is set or was just set.
    Suppressed,
    /// A database error: never wakes.
    Error,
}

fn reserve(
    conn: &mut Connection,
    station_id: &str,
    session_id: &str,
    request_id: &str,
    now: i64,
) -> Result<WakeReserve> {
    if session_id == schema::LEGACY_SESSION {
        return Ok(WakeReserve::Error);
    }
    let tx = conn.transaction()?;
    let suspended: i64 = tx.query_row(
        "SELECT suspended_until_ms FROM wake_state WHERE id = 1",
        [],
        |r| r.get(0),
    )?;
    if now < suspended {
        tx.commit()?;
        return Ok(WakeReserve::Suppressed);
    }
    tx.execute(
        "DELETE FROM wake_seen WHERE first_seen_ms < ?1",
        [now - WAKE_SEEN_TTL_MS],
    )?;
    let mut rows: i64 = tx.query_row("SELECT count(*) FROM wake_seen", [], |r| r.get(0))?;
    if rows >= WAKE_SEEN_MAX {
        let excess = rows - WAKE_SEEN_MAX + 1;
        tx.execute(
            "DELETE FROM wake_seen WHERE rowid IN (
                 SELECT rowid FROM wake_seen WHERE first_seen_ms < ?1
                 ORDER BY first_seen_ms, rowid LIMIT ?2)",
            params![now - WAKE_SEEN_PROTECT_MS, excess],
        )?;
        rows = tx.query_row("SELECT count(*) FROM wake_seen", [], |r| r.get(0))?;
    }
    if rows >= WAKE_SEEN_MAX {
        tx.execute(
            "UPDATE wake_state SET suspended_until_ms = ?1 WHERE id = 1",
            [now + WAKE_SUSPEND_MS],
        )?;
        tx.commit()?;
        return Ok(WakeReserve::Suppressed);
    }
    let n = tx.execute(
        "INSERT INTO wake_seen (station_id, session_id, request_id, first_seen_ms)
         VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
        params![station_id, session_id, request_id, now],
    )?;
    let legacy: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM wake_seen
         WHERE station_id = ?1 AND session_id = '' AND request_id = ?2)",
        params![station_id, request_id],
        |r| r.get(0),
    )?;
    tx.commit()?;
    Ok(if n == 1 && !legacy {
        WakeReserve::Reserved
    } else {
        WakeReserve::Seen
    })
}

impl DeckStore {
    /// Records that the wake rule observed `(station_id, session_id,
    /// request_id)`. A legacy tombstone of `(station_id, request_id)` (V49)
    /// makes the answer `Seen`; an empty `session_id` is `Error`. Only
    /// [`WakeReserve::Reserved`] may lead to a wake. A baseline observation
    /// uses the same operation, because it must block later wakes.
    pub async fn wake_reserve(
        &self,
        station_id: &str,
        session_id: &str,
        request_id: &str,
        now_ms: i64,
    ) -> WakeReserve {
        let (station, session, request) = (
            station_id.to_owned(),
            session_id.to_owned(),
            request_id.to_owned(),
        );
        match call(&self.inner.db, move |conn| {
            reserve(conn, &station, &session, &request, now_ms)
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "wake_reserve failed; no wake");
                WakeReserve::Error
            }
        }
    }
}

#[path = "wake_seen_schema.rs"]
pub(super) mod schema;

#[cfg(test)]
#[path = "wake_seen_tests.rs"]
mod tests;
