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
//! - `done` rejected with `invalid_state` while the deck shows the target
//!   session `starting`, `exited` or `lost` → `queued` and held (`reason =
//!   session_ended`, `next_at = expires_at`) until the session can take
//!   input ([`can_take_input`]: `running` or `starting`; [`DeckStore::release_held`]);
//!   only a follow-up or a steer is held ([`target_session`]);
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

use std::sync::Arc;

use fleetdeck_core::ids::new_id;
use fleetdeck_proto::command::{CommandEnvelope, DeckCommandState, ReceiptState, SessionCommand};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::{CommandId, RejectCode, SessionId, SessionSummary, StationId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::commands::{self, NewCommand};
use super::coordinator::{Check, recheck, strict_session};
use super::{DeckError, DeckStore, Result, call};
use crate::link::router::CommandKind;

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
    /// The authority of a fleet-control intent (ADR-008 §6.1); `None` for
    /// every other item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<CommandAuthority>,
    /// The `coordinator.call` that made the item; `None` for every other
    /// item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
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
                       next_at, expires_at, reason, created_at, updated_at, authority, call_id";

type RawItem = (OutboxItem, String, String, Option<String>);

fn from_row(row: &Row<'_>) -> rusqlite::Result<RawItem> {
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
            authority: None,
            call_id: row.get(13)?,
        },
        row.get(3)?,
        row.get(4)?,
        row.get(12)?,
    ))
}

fn finish_row(raw: RawItem) -> Result<OutboxItem> {
    let (mut item, payload, state, authority) = raw;
    item.payload = serde_json::from_str(&payload)?;
    item.state = OutboxState::parse(&state)?;
    item.authority = authority.as_deref().map(serde_json::from_str).transpose()?;
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

/// The session that an outbox payload targets, when a rejection of the
/// payload because the session ended holds the item. Only a follow-up or a
/// steer is held: every other payload gives `None`.
fn target_session(payload: &Value) -> Option<SessionId> {
    match serde_json::from_value::<CommandKind>(payload.clone()).ok()? {
        CommandKind::Session {
            command:
                SessionCommand::FollowUp { session_id, .. } | SessionCommand::Steer { session_id, .. },
        } => Some(session_id),
        _ => None,
    }
}

/// Whether a held item may try again for a session in `state`. `starting`
/// may: a Claude process reports `session.started` only after its first
/// input, so waiting for `running` could wait forever; a try before the
/// process exists is refused `invalid_state` and held again ([`holds`]),
/// and the next summary of the session releases it once more. `stopping`
/// and an ended session may not.
#[must_use]
pub(crate) fn can_take_input(state: SessionState) -> bool {
    matches!(state, SessionState::Running | SessionState::Starting)
}

/// Whether the done command `cmd` of `item` holds the item instead of
/// making it `dead`: the station rejected it with `invalid_state`, the
/// payload targets one session, and the deck shows that session `starting`,
/// `exited` or `lost` (it cannot take input yet; [`can_take_input`] releases
/// the item when it can).
fn holds(conn: &Connection, item: &OutboxItem, cmd: &commands::DeckCommand) -> Result<bool> {
    let invalid_state = cmd.state == DeckCommandState::Done
        && cmd.receipt.as_ref().is_some_and(|r| {
            r.state == ReceiptState::Rejected
                && r.reject
                    .as_ref()
                    .is_some_and(|j| j.code == RejectCode::InvalidState)
        });
    if !invalid_state {
        return Ok(false);
    }
    let Some(session) = target_session(&item.payload) else {
        return Ok(false);
    };
    let text: Option<String> = conn
        .query_row(
            "SELECT summary FROM sessions WHERE station_id = ?1 AND session_id = ?2",
            params![item.station_id.as_str(), session.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(text) = text else {
        return Ok(false);
    };
    // A summary that does not parse holds nothing: the item is dead as
    // before, and the reconcile goes on with the other items.
    let Ok(summary) = serde_json::from_str::<SessionSummary>(&text) else {
        return Ok(false);
    };
    Ok(matches!(
        summary.state,
        SessionState::Starting | SessionState::Exited | SessionState::Lost
    ))
}

/// Releases the items of `station` that are held for `session`: they are
/// due now. Returns the number of released items.
pub(crate) fn release_held_tx(
    conn: &Connection,
    station: &StationId,
    session: &SessionId,
    now: i64,
) -> Result<usize> {
    let held = list(
        conn,
        "WHERE station_id = ?1 AND state = 'queued' AND reason = 'session_ended'",
        params![station.as_str()],
    )?;
    let mut released = 0;
    for item in held {
        if target_session(&item.payload).as_ref() != Some(session) {
            continue;
        }
        released += conn.execute(
            "UPDATE outbox SET next_at = ?2, reason = NULL, updated_at = ?2
             WHERE outbox_id = ?1 AND state = 'queued' AND reason = 'session_ended'",
            params![item.outbox_id, now],
        )?;
    }
    Ok(released)
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
        let inner = Arc::clone(&self.inner);
        let out = call(&self.inner.db, move |conn| {
            let env = inner.fleet_env();
            let tx = conn.transaction()?;
            let found: Option<(i64, String, String, Option<String>)> = tx
                .query_row(
                    "SELECT expires_at, station_id, payload, authority FROM outbox
                     WHERE outbox_id = ?1 AND state = 'queued' AND expires_at > ?2",
                    params![outbox_id, now],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let Some((expires_at, station, payload, authority)) = found else {
                return Ok(None);
            };
            let has_authority = authority.is_some();
            // A fleet-control item checks its authority at the start of
            // every attempt (ADR-008 section 6.2): when it is gone the item
            // ends `dead` (`authority_lost`) and no command is made; a
            // station without a live link makes the item wait.
            if let Some(text) = authority {
                let authority: CommandAuthority = serde_json::from_str(&text)?;
                let target = crate::store::coordinator::strict_session_of(&payload);
                let station = StationId(station);
                match recheck(&tx, &env, &authority, (&station, target.as_ref()), now)? {
                    Check::Valid => {}
                    Check::Wait => return Ok(None),
                    Check::Lost(_) => {
                        set_state(
                            &tx,
                            &outbox_id,
                            OutboxState::Dead,
                            Some("authority_lost"),
                            now,
                        )?;
                        tx.commit()?;
                        return Ok(None);
                    }
                }
            }
            // A mail of a member checks its share at the start of every
            // attempt (NV-17): when the share is gone the item ends `dead`
            // (`authority_lost`) and no command is made.
            if !has_authority && !mail_still_valid(&tx, &outbox_id, now)? {
                set_state(
                    &tx,
                    &outbox_id,
                    OutboxState::Dead,
                    Some("authority_lost"),
                    now,
                )?;
                tx.commit()?;
                return Ok(None);
            }
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
                if state == OutboxState::Dead && holds(&tx, &item, &cmd)? {
                    // The target session ended: keep the item until it
                    // runs again (`release_held`) or the item expires.
                    tx.execute(
                        "UPDATE outbox SET state = 'queued', reason = 'session_ended',
                             next_at = expires_at, updated_at = ?2
                         WHERE outbox_id = ?1",
                        params![item.outbox_id, now],
                    )?;
                } else if state == OutboxState::Queued {
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

    /// Makes the items that are held for `session` of `station` due now,
    /// because the session runs again (module rules). Returns the number
    /// of released items.
    ///
    /// # Errors
    /// A database error.
    pub async fn release_held(&self, station: StationId, session: SessionId) -> Result<usize> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let released = release_held_tx(&tx, &station, &session, now)?;
            tx.commit()?;
            Ok(released)
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

/// The answer of a gate before a send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Every check holds: send.
    Open,
    /// A station has no live link: send later.
    Wait,
    /// The authority is gone: the command and its item ended.
    Lost,
}

impl DeckStore {
    /// The gate before a send or a resend of a routed command with
    /// `authority` (ADR-008 section 6.2). When the authority is gone, the
    /// command ends `failed` (`authority_lost`) and its item `dead`
    /// (`authority_lost`), in one transaction.
    ///
    /// # Errors
    /// A database error (the caller stops this send and tries on the next
    /// tick).
    pub async fn gate_authority(
        &self,
        command: CommandId,
        station: StationId,
        authority: CommandAuthority,
        target: Option<SessionId>,
    ) -> Result<Gate> {
        let now = self.now_ms();
        let inner = Arc::clone(&self.inner);
        let gate = call(&self.inner.db, move |conn| {
            let env = inner.fleet_env();
            let tx = conn.transaction()?;
            match recheck(&tx, &env, &authority, (&station, target.as_ref()), now)? {
                Check::Valid => return Ok(Gate::Open),
                Check::Wait => return Ok(Gate::Wait),
                Check::Lost(_) => {}
            }
            tx.execute(
                "UPDATE commands SET state = 'failed', reason = 'authority_lost', updated_at = ?2
                 WHERE command_id = ?1 AND state = 'pending'",
                params![command.as_str(), now],
            )?;
            tx.execute(
                "UPDATE outbox SET state = 'dead', reason = 'authority_lost', updated_at = ?2
                 WHERE command_id = ?1 AND state IN ('queued', 'sending')",
                params![command.as_str(), now],
            )?;
            tx.commit()?;
            Ok(Gate::Lost)
        })
        .await?;
        if gate == Gate::Lost {
            self.commands_changed();
        }
        Ok(gate)
    }
}

/// The authority and the acted-on session of a stored command envelope,
/// when it carries an authority.
#[must_use]
pub fn authority_of(envelope: &serde_json::Value) -> Option<(CommandAuthority, Option<SessionId>)> {
    let env: CommandEnvelope = serde_json::from_value(envelope.clone()).ok()?;
    let target = strict_session(&env.command).cloned();
    env.authority.map(|a| (a, target))
}

#[cfg(test)]
#[path = "outbox_hold_tests.rs"]
mod hold_tests;

#[cfg(test)]
#[path = "outbox_tests.rs"]
mod tests;

#[path = "outbox_mail.rs"]
mod mail;
use mail::mail_still_valid;
