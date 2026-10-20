//! The command router's table (M2 P3-7; G3, PLAN §8.4).
//!
//! The deck stores every mutating command with its envelope as `pending`
//! **before** it sends it; the stored envelope never changes, and a resend
//! sends the same bytes with the same `command_id`. One transaction moves a
//! command to its next state:
//!
//! - a receipt from the station → `done` (accepted or rejected), except:
//! - a rejection with `incarnation_mismatch` or `expired` (the station
//!   stores no receipt) → `ambiguous` when the command was sent before this
//!   attempt, else `failed` (`station_reset` / `expired`);
//! - a refusal of the link layer (`forbidden`, `wrong_role`, ...; the
//!   station did not decide this send) → `failed` with the code on the
//!   first send, `ambiguous` with the code after an earlier send (that send
//!   may have run);
//! - `retry_until` passed while `pending` → `ambiguous` (`no_receipt`) when
//!   it was sent, else `failed` (`not_sent`, DL-052);
//! - the station was removed → `failed` (`station_removed`).
//!
//! Nothing is redispatched with a new id.

use std::collections::BTreeSet;

use fleetdeck_core::ids::new_id;
use fleetdeck_proto::command::{DeckCommandState, ReceiptState};
use fleetdeck_proto::{CommandId, Receipt, RejectCode, StationId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;

use super::{DeckError, DeckStore, Result, call};

pub use fleetdeck_proto::board::DeckCommand;

/// A command to store as `pending`.
#[derive(Debug, Clone)]
pub struct NewCommand {
    pub command_id: CommandId,
    pub station_id: StationId,
    pub method: String,
    pub origin: String,
    pub envelope: Value,
    pub retry_until: i64,
    /// Who sent it: `owner`, `member:<name>` or `token:<name>` (R11.5).
    pub author: Option<String>,
}

/// What came back for one send.
#[derive(Debug, Clone)]
pub enum Settle {
    /// The station's receipt.
    Receipt(Receipt),
    /// The link layer refused the request; the station decided nothing.
    Refused { code: String, message: String },
}

const COLUMNS: &str = "command_id, station_id, method, origin, envelope, retry_until, state, \
                       attempts, sent_at, receipt, reason, created_at, updated_at, author";

fn state_name(s: DeckCommandState) -> &'static str {
    match s {
        DeckCommandState::Pending => "pending",
        DeckCommandState::Done => "done",
        DeckCommandState::Failed => "failed",
        DeckCommandState::Ambiguous => "ambiguous",
    }
}

fn parse_state(s: &str) -> Result<DeckCommandState> {
    Ok(match s {
        "pending" => DeckCommandState::Pending,
        "done" => DeckCommandState::Done,
        "failed" => DeckCommandState::Failed,
        "ambiguous" => DeckCommandState::Ambiguous,
        other => return Err(DeckError::Corrupt(format!("command state `{other}`"))),
    })
}

type RawCommand = (
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
    i64,
    Option<i64>,
    Option<String>,
    Option<String>,
    i64,
    i64,
    Option<String>,
);

fn raw(r: &Row<'_>) -> rusqlite::Result<RawCommand> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
        r.get(12)?,
        r.get(13)?,
    ))
}

fn parse(r: RawCommand) -> Result<DeckCommand> {
    let (id, station, method, origin, envelope, retry_until, state, attempts, sent_at) =
        (r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8);
    Ok(DeckCommand {
        command_id: id.into(),
        station_id: station.into(),
        method,
        origin,
        envelope: serde_json::from_str(&envelope)?,
        retry_until,
        state: parse_state(&state)?,
        attempts: u32::try_from(attempts)
            .map_err(|_| DeckError::Corrupt(format!("attempts {attempts}")))?,
        sent_at,
        receipt: r.9.as_deref().map(serde_json::from_str).transpose()?,
        reason: r.10,
        created_at: r.11,
        updated_at: r.12,
        author: r.13,
    })
}

pub(crate) fn get(conn: &Connection, id: &CommandId) -> Result<Option<DeckCommand>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM commands WHERE command_id = ?1"),
        [id.as_str()],
        raw,
    )
    .optional()?
    .map(parse)
    .transpose()
}

fn list(conn: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<Vec<DeckCommand>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM commands {sql}"))?;
    let rows = stmt.query_map(p, raw)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(parse(row?)?);
    }
    Ok(out)
}

/// What a command's end changed besides the command: a review (the caller
/// tells the board) and the stations whose derived scopes changed because the
/// worker grant of the start ended (the caller notes them).
#[derive(Debug, Default)]
struct Finished {
    review: bool,
    scopes: BTreeSet<StationId>,
}

/// Moves a `pending` command to `state`. The review of the command (DL-179)
/// follows in the same transaction. A start whose worker grant cannot be
/// used revokes the grant in the same transaction (MCP P4a §6.3, §6.4):
/// `undelivered` for a start that was never sent, `start_failed` for a
/// failure through a refusal of the link layer or a rejected receipt; an
/// ambiguous command revokes nothing.
fn finish(
    conn: &Connection,
    id: &CommandId,
    state: DeckCommandState,
    receipt: Option<&Receipt>,
    reason: Option<&str>,
    now: i64,
) -> Result<Finished> {
    let json = receipt.map(serde_json::to_string).transpose()?;
    let moved = conn.execute(
        "UPDATE commands SET state = ?2, receipt = ?3, reason = ?4, updated_at = ?5
         WHERE command_id = ?1 AND state = 'pending'",
        params![id.as_str(), state_name(state), json, reason, now],
    )?;
    if moved == 0 {
        return Ok(Finished::default());
    }
    let rejected = receipt.is_some_and(|r| r.state == ReceiptState::Rejected);
    let revoke = match state {
        DeckCommandState::Failed if reason == Some("not_sent") => Some("undelivered"),
        DeckCommandState::Failed => Some("start_failed"),
        DeckCommandState::Done if rejected => Some("start_failed"),
        _ => None,
    };
    let mut scopes = match revoke {
        Some(why) => super::worker_grant::revoke_for_start(conn, id, why, now)?,
        None => BTreeSet::new(),
    };
    // A session that ended before its receipt was stored (MCP P4a PRE-PR R2).
    let named = receipt
        .filter(|r| r.state == ReceiptState::Accepted)
        .and_then(|r| r.result.as_ref())
        .and_then(|r| r.session_id.as_ref());
    if let Some(session) = named {
        scopes.extend(super::worker_grant_end::revoke_if_ended(
            conn, id, session, now,
        )?);
    }
    Ok(Finished {
        review: super::review::command_ended(conn, id, state, receipt, reason)?,
        scopes,
    })
}

/// Marks the pending commands of a removed station `failed`
/// (`station_removed`), inside the removal transaction.
pub(crate) fn fail_station(conn: &Connection, station: &StationId, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE commands SET state = 'failed', reason = 'station_removed', updated_at = ?2
         WHERE station_id = ?1 AND state = 'pending'",
        params![station.as_str(), now],
    )? + super::outbox::drop_station(conn, station, now)?)
}

/// Inserts `cmd` as `pending`.
///
/// # Errors
/// [`DeckError::UnknownStation`], or a database error.
pub(crate) fn insert(conn: &Connection, cmd: &NewCommand, now: i64) -> Result<()> {
    let known = conn
        .query_row(
            "SELECT 1 FROM stations WHERE station_id = ?1",
            [cmd.station_id.as_str()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !known {
        return Err(DeckError::UnknownStation(cmd.station_id.0.clone()));
    }
    conn.execute(
        "INSERT INTO commands (command_id, station_id, method, origin, envelope,
             retry_until, state, created_at, updated_at, author)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?7, ?8)",
        params![
            cmd.command_id.as_str(),
            cmd.station_id.as_str(),
            cmd.method,
            cmd.origin,
            serde_json::to_string(&cmd.envelope)?,
            cmd.retry_until,
            now,
            cmd.author
        ],
    )?;
    Ok(())
}

/// The next state of a pending command for `settle` at `attempts` sends.
fn next_state(settle: &Settle, attempts: u32) -> (DeckCommandState, Option<String>) {
    match settle {
        Settle::Receipt(r) if r.state == ReceiptState::Rejected => {
            let code = r.reject.as_ref().map(|x| x.code);
            let reason = match code {
                Some(RejectCode::IncarnationMismatch) => "station_reset",
                Some(RejectCode::Expired) => "expired",
                _ => return (DeckCommandState::Done, None),
            };
            if attempts > 1 {
                (DeckCommandState::Ambiguous, Some(reason.to_owned()))
            } else {
                (DeckCommandState::Failed, Some(reason.to_owned()))
            }
        }
        Settle::Receipt(_) => (DeckCommandState::Done, None),
        // The link layer refused this send, so the station did not decide
        // it now. An earlier send may have reached the station and run:
        // after one, the outcome is unknown (round 24 review).
        Settle::Refused { code, .. } if attempts > 1 => {
            (DeckCommandState::Ambiguous, Some(code.clone()))
        }
        Settle::Refused { code, .. } => (DeckCommandState::Failed, Some(code.clone())),
    }
}

impl DeckStore {
    /// A new ULID command id.
    #[must_use]
    pub fn new_command_id(&self) -> CommandId {
        new_id(self.inner.ids.as_ref())
    }

    /// A receiver of command state changes.
    #[must_use]
    pub fn watch_commands(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.commands.subscribe()
    }

    pub(crate) fn commands_changed(&self) {
        self.inner.commands.send_modify(|n| *n += 1);
    }

    /// Stores `cmd` as `pending` before the first send (G3).
    ///
    /// # Errors
    /// [`DeckError::UnknownStation`], or a database error (a reused id).
    pub async fn insert_command(&self, cmd: NewCommand) -> Result<DeckCommand> {
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            insert(&tx, &cmd, now)?;
            let stored = get(&tx, &cmd.command_id)?
                .ok_or_else(|| DeckError::Corrupt("an inserted command is gone".to_owned()))?;
            tx.commit()?;
            Ok(stored)
        })
        .await?;
        self.commands_changed();
        Ok(out)
    }

    /// [`DeckStore::insert_command`] for a command id chosen by the client
    /// (the board): a resend of the same command (same station, method and
    /// `command` payload) returns the stored command and `false`; the id
    /// with another command is [`DeckError::CommandIdReused`]. One
    /// transaction, so two concurrent resends store one command.
    ///
    /// # Errors
    /// [`DeckError::CommandIdReused`], [`DeckError::UnknownStation`], a
    /// database error.
    pub async fn insert_or_existing(&self, cmd: NewCommand) -> Result<(DeckCommand, bool)> {
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if let Some(old) = get(&tx, &cmd.command_id)? {
                let same = old.station_id == cmd.station_id
                    && old.method == cmd.method
                    && old.envelope.get("command") == cmd.envelope.get("command")
                    && old.envelope.get("change") == cmd.envelope.get("change");
                if !same {
                    return Err(DeckError::CommandIdReused(cmd.command_id.0.clone()));
                }
                return Ok((old, false));
            }
            insert(&tx, &cmd, now)?;
            let stored = get(&tx, &cmd.command_id)?
                .ok_or_else(|| DeckError::Corrupt("an inserted command is gone".to_owned()))?;
            tx.commit()?;
            Ok((stored, true))
        })
        .await?;
        if out.1 {
            self.commands_changed();
        }
        Ok(out)
    }

    /// Counts one send and returns the command, when it is still `pending`
    /// and before `retry_until`; else `None` (do not send). The send is
    /// counted before the bytes leave, so a crash in between can only make
    /// a later outcome `ambiguous`, never wrongly `failed`.
    ///
    /// # Errors
    /// A database error.
    pub async fn mark_sent(&self, id: CommandId) -> Result<Option<DeckCommand>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let n = tx.execute(
                "UPDATE commands SET attempts = attempts + 1, sent_at = COALESCE(sent_at, ?2),
                     updated_at = ?2
                 WHERE command_id = ?1 AND state = 'pending' AND retry_until > ?2",
                params![id.as_str(), now],
            )?;
            let out = if n == 1 { get(&tx, &id)? } else { None };
            tx.commit()?;
            Ok(out)
        })
        .await
    }

    /// Records what came back for a send of a `pending` command; a command
    /// in another state stays as it is. Returns the command.
    ///
    /// # Errors
    /// [`DeckError::UnknownCommand`], or a database error.
    pub async fn settle_command(&self, id: CommandId, settle: Settle) -> Result<DeckCommand> {
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let cmd = get(&tx, &id)?.ok_or_else(|| DeckError::UnknownCommand(id.0.clone()))?;
            let mut done = Finished::default();
            if cmd.state == DeckCommandState::Pending {
                let (state, reason) = next_state(&settle, cmd.attempts);
                let receipt = match &settle {
                    Settle::Receipt(r) => Some(r),
                    Settle::Refused { .. } => None,
                };
                done = finish(&tx, &id, state, receipt, reason.as_deref(), now)?;
            }
            let out = get(&tx, &id)?
                .ok_or_else(|| DeckError::Corrupt("a settled command is gone".to_owned()))?;
            tx.commit()?;
            Ok((out, done))
        })
        .await?;
        self.note_scope_changes(out.1.scopes);
        self.commands_changed();
        if out.1.review {
            self.board_changed();
        }
        Ok(out.0)
    }

    /// Ends every `pending` command whose `retry_until` has passed:
    /// `ambiguous` (`no_receipt`) when it was sent, `failed` (`not_sent`)
    /// when not. Returns the commands it changed.
    ///
    /// # Errors
    /// A database error.
    pub async fn expire_commands(&self) -> Result<Vec<DeckCommand>> {
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let due = list(
                &tx,
                "WHERE state = 'pending' AND retry_until <= ?1 ORDER BY created_at, command_id",
                [now],
            )?;
            let mut done = Finished::default();
            for cmd in &due {
                let (state, reason) = if cmd.attempts > 0 {
                    (DeckCommandState::Ambiguous, "no_receipt")
                } else {
                    (DeckCommandState::Failed, "not_sent")
                };
                let one = finish(&tx, &cmd.command_id, state, None, Some(reason), now)?;
                done.review |= one.review;
                done.scopes.extend(one.scopes);
            }
            let mut changed = Vec::new();
            for cmd in due {
                changed.extend(get(&tx, &cmd.command_id)?);
            }
            tx.commit()?;
            Ok((changed, done))
        })
        .await?;
        self.note_scope_changes(out.1.scopes);
        if !out.0.is_empty() {
            self.commands_changed();
        }
        if out.1.review {
            self.board_changed();
        }
        Ok(out.0)
    }

    /// The `pending` commands of `station` before `retry_until`, oldest
    /// first (the send order).
    ///
    /// # Errors
    /// A database error.
    pub async fn pending_commands(&self, station: StationId) -> Result<Vec<DeckCommand>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            list(
                conn,
                "WHERE station_id = ?1 AND state = 'pending' AND retry_until > ?2
                 ORDER BY created_at, command_id",
                params![station.as_str(), now],
            )
        })
        .await
    }

    /// One command.
    ///
    /// # Errors
    /// A database error.
    pub async fn command(&self, id: CommandId) -> Result<Option<DeckCommand>> {
        call(&self.inner.db, move |conn| get(conn, &id)).await
    }

    /// Every command, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn commands(&self) -> Result<Vec<DeckCommand>> {
        call(&self.inner.db, |conn| {
            list(conn, "ORDER BY created_at, command_id", [])
        })
        .await
    }
}
