//! The durable station coordinator outbox (M3 6-5, ADR-006 §6.5, DL-218).
//!
//! A coordinator tool call that maps to `coordinator.call` commits one row
//! with its complete envelope before any send. The envelope never changes
//! (G3); every transmission sends the same bytes, and the deck deduplicates
//! by `command_id`. The states are the G3 states `pending`, `done`,
//! `failed` and `ambiguous`:
//!
//! - a definitive receipt (accepted, or a stored rejection) → `done`;
//! - `sent_at` commits before each network write ([`Store::coordinator_mark_sent`]),
//!   so a row with a transmission is "possibly sent" from then on;
//! - past `retry_until` without a definitive receipt: never sent →
//!   `failed` `expired`, sent → `ambiguous`;
//! - a non-stored rejection (`incarnation_mismatch`, `expired`,
//!   `forbidden`, `binding_stale`) after an earlier send → `ambiguous`,
//!   else `failed` with that reason (`incarnation_mismatch` → `deck_reset`);
//! - a station-known invalidation of the binding (the fence): never sent →
//!   `failed`, sent → `ambiguous`;
//! - station start recovery: the bindings did not survive the restart, so
//!   every `pending` row is fenced the same way.
//!
//! No terminal row is ever sent again.

use fleetdeck_proto::command::ReceiptState;
use fleetdeck_proto::{CommandId, Receipt, RejectCode};
use rusqlite::{Connection, OptionalExtension, params};

use super::{Result, Store, StoreError, call};

/// The state of an outbox row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxState {
    Pending,
    Done,
    Failed,
    Ambiguous,
}

impl OutboxState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Ambiguous => "ambiguous",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => Self::Pending,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "ambiguous" => Self::Ambiguous,
            other => return Err(StoreError::Corrupt(format!("outbox state {other}"))),
        })
    }
}

/// One coordinator outbox row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRow {
    pub command_id: CommandId,
    pub binding_id: String,
    pub generation: u32,
    /// The stored envelope (JSON), sent unchanged.
    pub envelope: String,
    pub state: OutboxState,
    pub reason: Option<String>,
    /// The definitive receipt (JSON) of a `done` row.
    pub receipt: Option<String>,
    pub retry_until: i64,
    pub sent_at: Option<i64>,
    pub sends: u32,
}

/// A new call for the outbox.
#[derive(Debug, Clone)]
pub struct NewCall {
    pub command_id: CommandId,
    pub binding_id: String,
    pub generation: u32,
    pub idempotency_key: Option<String>,
    pub args_digest: String,
    pub envelope: String,
    pub retry_until: i64,
}

/// The result of an intake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intake {
    /// A new row was stored.
    New(OutboxRow),
    /// The same idempotency key with the same arguments: the first row.
    Existing(OutboxRow),
    /// The same idempotency key with other arguments.
    KeyReused,
}

const COLUMNS: &str = "command_id, binding_id, generation, envelope, state, reason, receipt, \
                       retry_until, sent_at, sends";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(OutboxRow, String)> {
    Ok((
        OutboxRow {
            command_id: CommandId::from(r.get::<_, String>(0)?),
            binding_id: r.get(1)?,
            generation: r.get(2)?,
            envelope: r.get(3)?,
            state: OutboxState::Pending,
            reason: r.get(5)?,
            receipt: r.get(6)?,
            retry_until: r.get(7)?,
            sent_at: r.get(8)?,
            sends: r.get(9)?,
        },
        r.get(4)?,
    ))
}

fn finish((mut row, state): (OutboxRow, String)) -> Result<OutboxRow> {
    row.state = OutboxState::parse(&state)?;
    Ok(row)
}

fn get(conn: &Connection, command_id: &str) -> Result<Option<OutboxRow>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM coordinator_outbox WHERE command_id = ?1"),
        [command_id],
        row,
    )
    .optional()?
    .map(finish)
    .transpose()
}

fn must(conn: &Connection, command_id: &str) -> Result<OutboxRow> {
    get(conn, command_id)?.ok_or_else(|| StoreError::Corrupt(format!("no outbox row {command_id}")))
}

fn set(
    conn: &Connection,
    command_id: &str,
    state: OutboxState,
    reason: Option<&str>,
    receipt: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE coordinator_outbox SET state = ?2, reason = ?3, receipt = ?4, updated_at = ?5
         WHERE command_id = ?1 AND state = 'pending'",
        params![command_id, state.as_str(), reason, receipt, now],
    )?;
    Ok(())
}

/// The terminal state of a row that loses its binding (the fence).
fn fenced(sends: u32) -> OutboxState {
    if sends > 0 {
        OutboxState::Ambiguous
    } else {
        OutboxState::Failed
    }
}

/// The outcome of a receipt for a row that was transmitted `sends` times
/// (this answer belongs to the last transmission).
#[must_use]
pub fn classify(receipt: &Receipt, sends: u32) -> (OutboxState, Option<&'static str>) {
    let earlier = sends > 1;
    if receipt.state == ReceiptState::Accepted {
        return (OutboxState::Done, None);
    }
    let reason = match receipt.reject.as_ref().map(|r| r.code) {
        Some(RejectCode::IncarnationMismatch) => "deck_reset",
        Some(RejectCode::Expired) => "expired",
        Some(RejectCode::Forbidden) => "forbidden",
        Some(RejectCode::BindingStale) => "binding_stale",
        // Every other rejection is a stored, definitive receipt.
        _ => return (OutboxState::Done, None),
    };
    if earlier {
        (OutboxState::Ambiguous, Some(reason))
    } else {
        (OutboxState::Failed, Some(reason))
    }
}

/// Station start recovery: no binding survived the restart.
pub(super) fn recover(conn: &Connection, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE coordinator_outbox
         SET state = CASE WHEN sends > 0 THEN 'ambiguous' ELSE 'failed' END,
             reason = 'station_restart', updated_at = ?1
         WHERE state = 'pending'",
        [now],
    )?;
    Ok(())
}

impl Store {
    /// Stores a new call before any send, or returns the first row of the
    /// same idempotency key (§6.5).
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_intake(&self, new: NewCall) -> Result<Intake> {
        let (intake, _) = self
            .write(move |conn, now, _| {
                if let Some(key) = &new.idempotency_key {
                    let first: Option<(String, String)> = conn
                        .query_row(
                            "SELECT command_id, args_digest FROM coordinator_outbox
                             WHERE binding_id = ?1 AND idempotency_key = ?2",
                            params![new.binding_id, key],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?;
                    if let Some((id, digest)) = first {
                        return Ok(if digest == new.args_digest {
                            Intake::Existing(must(conn, &id)?)
                        } else {
                            Intake::KeyReused
                        });
                    }
                }
                conn.execute(
                    "INSERT INTO coordinator_outbox (command_id, binding_id, generation,
                         idempotency_key, args_digest, envelope, state, retry_until,
                         created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?8)",
                    params![
                        new.command_id.as_str(),
                        new.binding_id,
                        new.generation,
                        new.idempotency_key,
                        new.args_digest,
                        new.envelope,
                        new.retry_until,
                        now
                    ],
                )?;
                Ok(Intake::New(must(conn, new.command_id.as_str())?))
            })
            .await?;
        Ok(intake)
    }

    /// Commits a transmission (`sent_at`, `sends + 1`) before the network
    /// write. `None` when the row is no longer `pending`.
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_mark_sent(&self, command_id: CommandId) -> Result<Option<OutboxRow>> {
        let (r, _) = self
            .write(move |conn, now, _| {
                let n = conn.execute(
                    "UPDATE coordinator_outbox SET sent_at = ?2, sends = sends + 1, updated_at = ?2
                     WHERE command_id = ?1 AND state = 'pending'",
                    params![command_id.as_str(), now],
                )?;
                if n == 0 {
                    return Ok(None);
                }
                get(conn, command_id.as_str())
            })
            .await?;
        Ok(r)
    }

    /// Records the deck's answer to the last transmission.
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_settle(&self, receipt: Receipt) -> Result<OutboxRow> {
        let (r, _) = self
            .write(move |conn, now, _| {
                let id = receipt.command_id.as_str().to_owned();
                let current = must(conn, &id)?;
                if current.state == OutboxState::Pending {
                    let (state, reason) = classify(&receipt, current.sends);
                    let json = serde_json::to_string(&receipt)?;
                    let stored = (state == OutboxState::Done).then_some(json.as_str());
                    set(conn, &id, state, reason, stored, now)?;
                }
                must(conn, &id)
            })
            .await?;
        Ok(r)
    }

    /// Ends every `pending` row whose `retry_until` passed (§6.5).
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_expire(&self) -> Result<()> {
        self.write(move |conn, now, _| {
            conn.execute(
                "UPDATE coordinator_outbox
                 SET state = CASE WHEN sends > 0 THEN 'ambiguous' ELSE 'failed' END,
                     reason = 'expired', updated_at = ?1
                 WHERE state = 'pending' AND retry_until < ?1",
                [now],
            )?;
            Ok(())
        })
        .await
        .map(|_| ())
    }

    /// The fence: the binding of `command_id` is no longer valid for it.
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_fence(
        &self,
        command_id: CommandId,
        reason: &'static str,
    ) -> Result<OutboxRow> {
        let (r, _) = self
            .write(move |conn, now, _| {
                let id = command_id.as_str();
                let current = must(conn, id)?;
                set(conn, id, fenced(current.sends), Some(reason), None, now)?;
                must(conn, id)
            })
            .await?;
        Ok(r)
    }

    /// The `pending` rows, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_pending(&self) -> Result<Vec<OutboxRow>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM coordinator_outbox WHERE state = 'pending'
                 ORDER BY created_at, command_id"
            ))?;
            let rows = stmt
                .query_map([], row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().map(finish).collect()
        })
        .await
    }

    /// One row.
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_row(&self, command_id: CommandId) -> Result<Option<OutboxRow>> {
        call(&self.inner.db, move |conn| get(conn, command_id.as_str())).await
    }
}
