//! A running turn is use of its grant (G6, design `grant-idle.md` §3).
//!
//! The deck moves `last_used_at` of a bound grant in effect to the deck
//! time while the read model shows a running turn of the grant's session
//! and the station is fresh (§3.2). The sweep, each event batch, each
//! snapshot and each `station.info` answer of a live link do it, in the
//! database step that can make the station fresh (§3.3). Every idle check
//! reads `last_used_at`, so it needs no change.

use std::collections::BTreeMap;
use std::sync::Arc;

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::session::SessionSummary;
use fleetdeck_proto::{IncarnationId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::{active_grants, in_effect};
use crate::store::grants::{GRANT_COLUMNS, Grant, finish, grant_row};
use crate::store::{DeckStore, Inner, Result, call};

/// The incarnation and the head of one `station.info` answer (§3.2).
pub(crate) type Stream = (IncarnationId, u64);

/// A running turn (G6 §3.1): a run is active and the process lives.
#[must_use]
pub(crate) fn turn_running(summary: &SessionSummary) -> bool {
    summary.current_run.is_some()
        && !matches!(summary.state, SessionState::Exited | SessionState::Lost)
}

/// True when the read model of `station` has the incarnation of `stream`
/// and its cursor is at least the head (§3.2 item 2).
fn caught_up(tx: &Connection, station: &StationId, stream: &Stream) -> Result<bool> {
    let row: Option<(Option<String>, i64)> = tx
        .query_row(
            "SELECT incarnation_id, cursor FROM stations WHERE station_id = ?1",
            [station.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.is_some_and(|(inc, cursor)| {
        inc.as_deref() == Some(stream.0.as_str())
            && u64::try_from(cursor).is_ok_and(|c| c >= stream.1)
    }))
}

/// Touches `grant` when §3.1 holds; returns true when it wrote.
fn touch_if_running(tx: &Connection, grant: &Grant, stream: &Stream, now: i64) -> Result<bool> {
    if grant.binding.is_none() || !in_effect(grant, now) {
        return Ok(false);
    }
    if !caught_up(tx, &grant.station_id, stream)? {
        return Ok(false);
    }
    let text: Option<String> = tx
        .query_row(
            "SELECT summary FROM sessions WHERE station_id = ?1 AND session_id = ?2",
            params![grant.station_id.as_str(), grant.session_id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let running = text
        .and_then(|t| serde_json::from_str::<SessionSummary>(&t).ok())
        .is_some_and(|s| turn_running(&s));
    if !running {
        return Ok(false);
    }
    let n = tx.execute(
        "UPDATE tokens SET last_used_at = MAX(COALESCE(last_used_at, ?2), ?2)
         WHERE grant_id = ?1 AND state = 'active'",
        params![grant.grant_id, now],
    )?;
    Ok(n == 1)
}

/// Touch point 1 (the sweep): every active grant whose station is fresh.
/// The stream of each station is read once, inside this transaction.
/// Returns the number of touched grants.
pub(in crate::store) fn touch_running_turns(
    tx: &Connection,
    inner: &Inner,
    now: i64,
) -> Result<usize> {
    let mut streams: BTreeMap<StationId, Option<Stream>> = BTreeMap::new();
    let mut touched = 0;
    for grant in active_grants(tx)? {
        let stream = streams
            .entry(grant.station_id.clone())
            .or_insert_with(|| inner.current_stream(&grant.station_id));
        if let Some(stream) = stream
            && touch_if_running(tx, &grant, stream, now)?
        {
            touched += 1;
        }
    }
    Ok(touched)
}

/// Touch points 2-4: every active grant of `station` (all its sessions).
/// Returns the number of touched grants.
pub(crate) fn touch_station_turns(
    tx: &Connection,
    station: &StationId,
    stream: &Stream,
    now: i64,
) -> Result<usize> {
    let mut stmt = tx.prepare(&format!(
        "SELECT {GRANT_COLUMNS} FROM tokens WHERE station_id = ?1 AND state = 'active'"
    ))?;
    let rows = stmt
        .query_map([station.as_str()], grant_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut touched = 0;
    for raw in rows {
        if touch_if_running(tx, &finish(raw)?, stream, now)? {
            touched += 1;
        }
    }
    Ok(touched)
}

/// Touch points 2 and 3 in the caller's transaction: reads the stream of
/// `station` (§3.2 item 1) and the clock now, and touches when the station
/// has a stream. Called after `set_cursor`, before `tx.commit()`.
pub(in crate::store) fn touch_after_fold(
    tx: &Connection,
    inner: &Inner,
    station: &StationId,
) -> Result<usize> {
    match inner.current_stream(station) {
        Some(stream) => {
            touch_station_turns(tx, station, &stream, crate::store::now_ms(&inner.clock))
        }
        None => Ok(0),
    }
}

impl DeckStore {
    /// Touch point 4: records the stream of a `station.info` answer of link
    /// `token` (§3.2) and touches the running turns of `station`, in one
    /// closure on the database thread. Returns `Ok(0)` without a write
    /// when the link ended (the answer is not recorded).
    ///
    /// # Errors
    /// A database error (the stream stays recorded; the next batch or
    /// sweep touches).
    pub async fn record_link_stream(
        &self,
        station: StationId,
        token: u64,
        stream: Stream,
    ) -> Result<usize> {
        let inner = Arc::clone(&self.inner);
        call(&self.inner.db, move |conn| {
            if !inner.set_link_stream(&station, token, stream.clone()) {
                return Ok(0);
            }
            let tx = conn.transaction()?;
            let now = crate::store::now_ms(&inner.clock);
            let n = touch_station_turns(&tx, &station, &stream, now)?;
            tx.commit()?;
            Ok(n)
        })
        .await
    }
}

#[cfg(test)]
#[path = "grant_turns_tests.rs"]
mod tests;
