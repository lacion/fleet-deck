//! The rolling rate windows of the coordinator desktop tools (R13 P6 §8.2),
//! as functions on the transaction that decides. A window is the last
//! [`limits::RATE_WINDOW_MS`] of one `(scope_key, bucket)`. The time of a
//! window never moves back: `t` is the larger of the clock and the newest
//! stored row, so a backward clock opens no capacity (O12-4 item 12).

use fleetdeck_proto::coordinator_desktop_text::limits;
use rusqlite::{Transaction, params};

use crate::store::Result;

pub(super) const MUTATION: &str = "mutation";
pub(super) const SCREENSHOT: &str = "screenshot";
pub(super) const QUERY: &str = "query";
pub(super) const IMAGE: &str = "image";
pub(super) const NOTIFY: &str = "notify";
pub(super) const INPUT: &str = "input";

/// Rows older than this (milliseconds before `t`) are deleted when a row is
/// added.
const PRUNE_MS: i64 = 120_000;

fn window_ms() -> i64 {
    i64::try_from(limits::RATE_WINDOW_MS).unwrap_or(i64::MAX)
}

/// The window time: `max(now, the newest stored row)`.
fn window_time(tx: &Transaction<'_>, scope_key: &str, bucket: &str, now: i64) -> Result<i64> {
    // `MAX` of no row is one row with `NULL`.
    let newest: Option<i64> = tx.query_row(
        "SELECT MAX(at) FROM coordinator_desktop_rate WHERE scope_key = ?1 AND bucket = ?2",
        params![scope_key, bucket],
        |r| r.get(0),
    )?;
    Ok(newest.map_or(now, |at| at.max(now)))
}

/// The rows of the window of `scope_key` and `bucket`.
pub(super) fn count(tx: &Transaction<'_>, scope_key: &str, bucket: &str, now: i64) -> Result<u32> {
    let t = window_time(tx, scope_key, bucket, now)?;
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM coordinator_desktop_rate
         WHERE scope_key = ?1 AND bucket = ?2 AND at > ?3",
        params![scope_key, bucket, t.saturating_sub(window_ms())],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Adds one row at the window time and deletes the rows that are older than
/// two minutes before it.
pub(super) fn charge(tx: &Transaction<'_>, scope_key: &str, bucket: &str, now: i64) -> Result<()> {
    let t = window_time(tx, scope_key, bucket, now)?;
    tx.execute(
        "INSERT INTO coordinator_desktop_rate (scope_key, bucket, at) VALUES (?1, ?2, ?3)",
        params![scope_key, bucket, t],
    )?;
    tx.execute(
        "DELETE FROM coordinator_desktop_rate WHERE scope_key = ?1 AND bucket = ?2 AND at <= ?3",
        params![scope_key, bucket, t.saturating_sub(PRUNE_MS)],
    )?;
    Ok(())
}
