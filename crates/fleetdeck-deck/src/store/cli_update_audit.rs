//! The audit import of provider updates (B28, design `cli-update.md` §3.11).
//!
//! The station keeps its attempts in `station.db`. The deck copies each
//! finished attempt into its audit log, once. One transaction writes the
//! seen rows, the audit rows and the cursor `(incarnation, seq)` of the
//! station: a failed insert leaves the cursor where it was.
//!
//! A cursor of another incarnation counts as 0 (a snapshot restore gives the
//! station a new incarnation). `cli_update_seen` then stops an attempt that
//! the deck imported before the restore from showing twice.

use fleetdeck_proto::cli_update::{CliUpdateAttempt, UpdateOutcome, is_release};
use fleetdeck_proto::{IncarnationId, StationId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::{DeckStore, Result, call, to_i64, to_u64};

/// The audit action of an imported attempt.
pub const AUDIT_ACTION: &str = "cli_update.attempt";
/// The newest rows of `cli_update_seen` that a station keeps.
pub const SEEN_KEEP: i64 = 2000;
/// The longest audit detail, in characters.
const DETAIL_MAX: usize = 300;
/// The longest reason code that the detail shows, in characters.
const REASON_MAX: usize = 48;

/// The snake case name of a wire enum.
fn name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// A version for the detail: itself when it is a release, else `?`.
fn version(v: Option<&str>) -> String {
    match v {
        None => "-".to_owned(),
        Some(v) if is_release(v) => v.to_owned(),
        Some(_) => "?".to_owned(),
    }
}

/// The audit result of an outcome (§3.11 step 4).
#[must_use]
pub fn audit_result(outcome: UpdateOutcome) -> &'static str {
    match outcome {
        UpdateOutcome::Updated
        | UpdateOutcome::UpToDate
        | UpdateOutcome::ReportOnly
        | UpdateOutcome::SkippedBusy
        | UpdateOutcome::Verified => "ok",
        UpdateOutcome::Refused => "refused",
        _ => "failed",
    }
}

/// The audit detail of an attempt:
/// `"<trigger> <method> <from or -> -> <to or target or ->: <outcome>"` and
/// `" (<reason>)"` when there is a reason. At most 300 characters. A reason
/// that is not a plain code shows as `?`: the station chose its text.
#[must_use]
pub fn audit_detail(a: &CliUpdateAttempt) -> String {
    let to = a.to_version.as_deref().or(a.target_version.as_deref());
    let mut text = format!(
        "{} {} {} -> {}: {}",
        name(&a.trigger),
        name(&a.method),
        version(a.from_version.as_deref()),
        version(to),
        name(&a.outcome),
    );
    if let Some(reason) = &a.reason {
        let plain = !reason.is_empty()
            && reason.chars().count() <= REASON_MAX
            && reason
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        text.push_str(" (");
        text.push_str(if plain { reason } else { "?" });
        text.push(')');
    }
    text.chars().take(DETAIL_MAX).collect()
}

/// The stored cursor of a station: `(incarnation, seq)`.
fn cursor(conn: &Connection, station_id: &str) -> Result<Option<(String, u64)>> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT incarnation, seq FROM cli_update_cursor WHERE station_id = ?1",
            [station_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(inc, seq)| Ok((inc, to_u64(seq)?))).transpose()
}

/// One transaction (§3.11 step 3). Returns the number of audit rows written.
fn import(
    conn: &mut Connection,
    station_id: &str,
    station_name: &str,
    incarnation: &str,
    rows: &[CliUpdateAttempt],
    now: i64,
) -> Result<usize> {
    let tx = conn.transaction()?;
    let stored = match cursor(&tx, station_id)? {
        Some((inc, seq)) if inc == incarnation => seq,
        _ => 0,
    };
    let mut written = 0;
    let mut top = stored;
    for row in rows.iter().filter(|r| r.seq > stored) {
        top = top.max(row.seq);
        let added = tx.execute(
            "INSERT OR IGNORE INTO cli_update_seen (station_id, attempt_id, imported_at)
             VALUES (?1, ?2, ?3)",
            params![station_id, row.attempt_id, now],
        )?;
        if added == 0 {
            continue;
        }
        tx.execute(
            "INSERT INTO audit (at, actor, action, target, command_id, result, detail)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
            params![
                now,
                format!("station:{station_name}"),
                AUDIT_ACTION,
                format!("{station_name}/{}", name(&row.provider)),
                audit_result(row.outcome),
                audit_detail(row),
            ],
        )?;
        written += 1;
    }
    if top > stored {
        tx.execute(
            "INSERT INTO cli_update_cursor (station_id, incarnation, seq) VALUES (?1, ?2, ?3)
             ON CONFLICT(station_id) DO UPDATE
             SET incarnation = excluded.incarnation, seq = excluded.seq",
            params![station_id, incarnation, to_i64(top)?],
        )?;
    }
    tx.execute(
        "DELETE FROM cli_update_seen WHERE station_id = ?1 AND rowid NOT IN (
             SELECT rowid FROM cli_update_seen WHERE station_id = ?1
             ORDER BY imported_at DESC, rowid DESC LIMIT ?2)",
        params![station_id, SEEN_KEEP],
    )?;
    tx.commit()?;
    Ok(written)
}

impl DeckStore {
    /// The audit cursor of a station: the incarnation of its database and
    /// the highest `seq` that the deck imported. `None` before the first
    /// import.
    ///
    /// # Errors
    /// A database error.
    pub async fn cli_update_cursor(
        &self,
        station: &StationId,
    ) -> Result<Option<(IncarnationId, u64)>> {
        let id = station.as_str().to_owned();
        let row = call(&self.inner.db, move |conn| cursor(conn, &id)).await?;
        Ok(row.map(|(inc, seq)| (IncarnationId::from(inc), seq)))
    }

    /// Copies the finished attempts `rows` of a station database with
    /// `incarnation` into the audit log, once each, and moves the cursor to
    /// the highest `seq`, all in one transaction. Returns the number of audit
    /// rows written.
    ///
    /// # Errors
    /// A database error; nothing is written.
    pub async fn import_cli_update_attempts(
        &self,
        station_id: StationId,
        station_name: String,
        incarnation: IncarnationId,
        rows: Vec<CliUpdateAttempt>,
    ) -> Result<usize> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            import(
                conn,
                station_id.as_str(),
                &station_name,
                incarnation.as_str(),
                &rows,
                now,
            )
        })
        .await
    }
}

#[cfg(test)]
#[path = "cli_update_audit_tests.rs"]
mod tests;
