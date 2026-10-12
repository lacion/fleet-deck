//! The outbox of deck-owned intent (M2 P3-7b; A5, PLAN §8.4).
//!
//! An outbox item is intent that the deck owns for one station (mail and
//! task assignment of Phase 6): it must reach the station once, also when
//! the station is away for a while. Each delivery attempt is one routed
//! command of [`super::commands`]; the attempt's command is inserted in the
//! **same transaction** that moves the item to `sending`, so a crash never
//! leaves an item that is sent but not recorded, or recorded twice. The
//! router resends the attempt's command with the same `command_id` until
//! its `retry_until`, so the station's receipt store deduplicates. That
//! `retry_until` is never after the item's `expires_at`: expired intent is
//! never sent or resent.
//!
//! [`DeckStore::reconcile_outbox`] moves a `sending` item by its command:
//!
//! - `done` with an accepted receipt → `delivered`;
//! - `done` with a stored rejection → `dead` (`rejected: <code>`);
//! - `failed` with a reason that proves the station never ran it
//!   (`not_sent`, `station_reset`, `expired`) → `queued` again with a new
//!   command on the next attempt, after a backoff of
//!   [`ATTEMPT_BACKOFF_MS`] · 2^(attempts − 1), at most [`MAX_BACKOFF_MS`];
//! - any other `failed` → `dead` with the reason;
//! - `ambiguous` → `dead` (`ambiguous: <reason>`): the station may have run
//!   it, so the deck never sends it again by itself (G3);
//!
//! and a `queued` item past `expires_at` → `dead` (`expired`). Removing the
//! station makes its open items `dead` (`station_removed`) in the removal
//! transaction.

use fleetdeck_core::ids::new_id;
use fleetdeck_proto::command::{DeckCommandState, ReceiptState};
use fleetdeck_proto::{CommandId, StationId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::commands::{self, NewCommand};
use super::{DeckError, DeckStore, Result, call};

/// The first backoff after an attempt that the station never ran.
pub const ATTEMPT_BACKOFF_MS: i64 = 5_000;
/// The longest backoff between attempts.
pub const MAX_BACKOFF_MS: i64 = 5 * 60 * 1000;

/// The command failure reasons that prove the station never ran the
/// command: a new attempt with a new `command_id` is safe.
const NEVER_RAN: [&str; 3] = ["not_sent", "station_reset", "expired"];

/// The state of an outbox item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxState {
    /// Waits for `next_at` and a live link of its station.
    Queued,
    /// The command of the current attempt is routed.
    Sending,
    /// The station accepted it.
    Delivered,
    /// It will not be delivered; `reason` says why.
    Dead,
}

impl OutboxState {
    fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sending => "sending",
            Self::Delivered => "delivered",
            Self::Dead => "dead",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "queued" => Self::Queued,
            "sending" => Self::Sending,
            "delivered" => Self::Delivered,
            "dead" => Self::Dead,
            other => return Err(DeckError::Corrupt(format!("outbox state {other}"))),
        })
    }
}

/// One outbox item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxItem {
    pub outbox_id: String,
    pub station_id: StationId,
    /// `mail`, `coordinator` or `board`.
    pub origin: String,
    /// The intent (a router command kind as JSON).
    pub payload: Value,
    pub state: OutboxState,
    /// The command of the current or last attempt.
    pub command_id: Option<CommandId>,
    pub attempts: u32,
    pub next_at: i64,
    pub expires_at: i64,
    pub reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// An item to queue.
#[derive(Debug, Clone)]
pub struct NewOutboxItem {
    pub station_id: StationId,
    pub origin: String,
    pub payload: Value,
    /// Unix ms; a queued item past it is `dead` (`expired`).
    pub expires_at: i64,
}

const COLUMNS: &str = "outbox_id, station_id, origin, payload, state, command_id, attempts, \
                       next_at, expires_at, reason, created_at, updated_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<(OutboxItem, String, String)> {
    let attempts: i64 = row.get(6)?;
    Ok((
        OutboxItem {
            outbox_id: row.get(0)?,
            station_id: StationId(row.get(1)?),
            origin: row.get(2)?,
            payload: Value::Null,
            state: OutboxState::Queued,
            command_id: row.get::<_, Option<String>>(5)?.map(CommandId),
            attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
            next_at: row.get(7)?,
            expires_at: row.get(8)?,
            reason: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        },
        row.get(3)?,
        row.get(4)?,
    ))
}

fn finish_row(raw: (OutboxItem, String, String)) -> Result<OutboxItem> {
    let (mut item, payload, state) = raw;
    item.payload = serde_json::from_str(&payload)?;
    item.state = OutboxState::parse(&state)?;
    Ok(item)
}

fn get(conn: &Connection, id: &str) -> Result<Option<OutboxItem>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM outbox WHERE outbox_id = ?1"),
        [id],
        from_row,
    )
    .optional()?
    .map(finish_row)
    .transpose()
}

fn list(conn: &Connection, filter: &str, p: impl rusqlite::Params) -> Result<Vec<OutboxItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM outbox {filter} ORDER BY created_at, outbox_id"
    ))?;
    let rows = stmt.query_map(p, from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(finish_row(row?)?);
    }
    Ok(out)
}

fn set_state(
    conn: &Connection,
    id: &str,
    state: OutboxState,
    reason: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE outbox SET state = ?2, reason = ?3, updated_at = ?4 WHERE outbox_id = ?1",
        params![id, state.name(), reason, now],
    )?;
    Ok(())
}

/// Makes the open items of a removed station `dead` (in the removal
/// transaction).
pub(crate) fn drop_station(conn: &Connection, station: &StationId, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE outbox SET state = 'dead', reason = 'station_removed', updated_at = ?2
         WHERE station_id = ?1 AND state IN ('queued', 'sending')",
        params![station.as_str(), now],
    )?)
}

/// Inserts one `queued` item in the caller's transaction (mail and tasks
/// store their own row in the same transaction, M3 6-2/6-4).
pub(crate) fn insert_item(
    conn: &Connection,
    id: &str,
    item: &NewOutboxItem,
    now: i64,
) -> Result<OutboxItem> {
    let known = conn
        .query_row(
            "SELECT 1 FROM stations WHERE station_id = ?1",
            [item.station_id.as_str()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !known {
        return Err(DeckError::UnknownStation(item.station_id.0.clone()));
    }
    conn.execute(
        "INSERT INTO outbox (outbox_id, station_id, kind, origin, payload, state,
             next_at, expires_at, created_at, updated_at)
         VALUES (?1, ?2, 'command', ?3, ?4, 'queued', ?5, ?6, ?5, ?5)",
        params![
            id,
            item.station_id.as_str(),
            item.origin,
            serde_json::to_string(&item.payload)?,
            now,
            item.expires_at
        ],
    )?;
    get(conn, id)?.ok_or_else(|| DeckError::Corrupt("an inserted outbox item is gone".to_owned()))
}

/// The wire name of a reject code.
fn reject_code(code: fleetdeck_proto::RejectCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// The backoff before attempt `attempts + 1`.
fn backoff(attempts: u32) -> i64 {
    let shift = attempts.saturating_sub(1).min(20);
    ATTEMPT_BACKOFF_MS
        .saturating_mul(1 << shift)
        .min(MAX_BACKOFF_MS)
}

/// The next state of a `sending` item whose command is `cmd`; `None` while
/// the command is `pending`.
fn next(cmd: &commands::DeckCommand) -> Option<(OutboxState, Option<String>)> {
    match cmd.state {
        DeckCommandState::Pending => None,
        DeckCommandState::Done => Some(match &cmd.receipt {
            Some(r) if r.state == ReceiptState::Accepted => (OutboxState::Delivered, None),
            Some(r) => {
                let code = r
                    .reject
                    .as_ref()
                    .map_or_else(|| "unknown".to_owned(), |j| reject_code(j.code));
                (OutboxState::Dead, Some(format!("rejected: {code}")))
            }
            None => (OutboxState::Dead, Some("rejected: no receipt".to_owned())),
        }),
        DeckCommandState::Failed => {
            let reason = cmd.reason.clone().unwrap_or_default();
            if NEVER_RAN.contains(&reason.as_str()) {
                Some((OutboxState::Queued, Some(reason)))
            } else {
                Some((OutboxState::Dead, Some(reason)))
            }
        }
        DeckCommandState::Ambiguous => Some((
            OutboxState::Dead,
            Some(format!(
                "ambiguous: {}",
                cmd.reason.as_deref().unwrap_or("unknown")
            )),
        )),
    }
}

impl DeckStore {
    /// Queues one item.
    ///
    /// # Errors
    /// [`DeckError::UnknownStation`], or a database error.
    pub async fn enqueue(&self, item: NewOutboxItem) -> Result<OutboxItem> {
        let now = self.now_ms();
        let id: String = new_id(self.inner.ids.as_ref());
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let stored = insert_item(&tx, &id, &item, now)?;
            tx.commit()?;
            Ok(stored)
        })
        .await
    }

    /// The `queued` items of `station` that are due now, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn due_outbox(&self, station: StationId) -> Result<Vec<OutboxItem>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            list(
                conn,
                "WHERE station_id = ?1 AND state = 'queued' AND next_at <= ?2 AND expires_at > ?2",
                params![station.as_str(), now],
            )
        })
        .await
    }

    /// Starts one attempt of a `queued` item: inserts `cmd` as `pending`
    /// and moves the item to `sending` with it, in one transaction.
    /// Returns `None` when the item is no longer `queued` or has expired.
    ///
    /// # Errors
    /// [`DeckError::Corrupt`] when `cmd.retry_until` is after the item's
    /// `expires_at` (an attempt never outlives its item); a database error.
    pub async fn start_attempt(
        &self,
        outbox_id: String,
        cmd: NewCommand,
    ) -> Result<Option<OutboxItem>> {
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let expires_at: Option<i64> = tx
                .query_row(
                    "SELECT expires_at FROM outbox
                     WHERE outbox_id = ?1 AND state = 'queued' AND expires_at > ?2",
                    params![outbox_id, now],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(expires_at) = expires_at else {
                return Ok(None);
            };
            if cmd.retry_until > expires_at {
                return Err(DeckError::Corrupt(format!(
                    "an attempt of outbox item {outbox_id} outlives its expires_at"
                )));
            }
            commands::insert(&tx, &cmd, now)?;
            tx.execute(
                "UPDATE outbox SET state = 'sending', command_id = ?2, attempts = attempts + 1,
                     reason = NULL, updated_at = ?3
                 WHERE outbox_id = ?1",
                params![outbox_id, cmd.command_id.as_str(), now],
            )?;
            let item = get(&tx, &outbox_id)?;
            tx.commit()?;
            Ok(item)
        })
        .await?;
        if out.is_some() {
            self.commands_changed();
        }
        Ok(out)
    }

    /// Moves every `sending` item by its command and ends the `queued`
    /// items past `expires_at` (module rules). Returns the number of items
    /// that changed.
    ///
    /// # Errors
    /// A database error.
    pub async fn reconcile_outbox(&self) -> Result<usize> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let mut changed = tx.execute(
                "UPDATE outbox SET state = 'dead', reason = 'expired', updated_at = ?1
                 WHERE state = 'queued' AND expires_at <= ?1",
                [now],
            )?;
            for item in list(&tx, "WHERE state = 'sending'", [])? {
                let Some(id) = &item.command_id else {
                    return Err(DeckError::Corrupt(format!(
                        "outbox item {} is sending without a command",
                        item.outbox_id
                    )));
                };
                let cmd = commands::get(&tx, id)?
                    .ok_or_else(|| DeckError::Corrupt(format!("outbox command {id} is gone")))?;
                let Some((state, reason)) = next(&cmd) else {
                    continue;
                };
                if state == OutboxState::Queued {
                    tx.execute(
                        "UPDATE outbox SET state = 'queued', reason = ?2, next_at = ?3,
                             updated_at = ?4
                         WHERE outbox_id = ?1",
                        params![item.outbox_id, reason, now + backoff(item.attempts), now],
                    )?;
                } else {
                    set_state(&tx, &item.outbox_id, state, reason.as_deref(), now)?;
                }
                changed += 1;
            }
            tx.commit()?;
            Ok(changed)
        })
        .await
    }

    /// One item.
    ///
    /// # Errors
    /// A database error.
    pub async fn outbox_item(&self, outbox_id: String) -> Result<Option<OutboxItem>> {
        call(&self.inner.db, move |conn| get(conn, &outbox_id)).await
    }

    /// Every item, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn outbox(&self) -> Result<Vec<OutboxItem>> {
        call(&self.inner.db, |conn| list(conn, "", [])).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_doubles_from_five_seconds_up_to_five_minutes() {
        assert_eq!(backoff(1), 5_000);
        assert_eq!(backoff(2), 10_000);
        assert_eq!(backoff(4), 40_000);
        assert_eq!(backoff(7), MAX_BACKOFF_MS);
        assert_eq!(backoff(u32::MAX), MAX_BACKOFF_MS);
    }
}
