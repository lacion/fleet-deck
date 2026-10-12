//! The durable effects of the human shell (PLAN §6.8, DL-405).
//!
//! A shell command (`shell.open`, `shell.resize`, `shell.close`) is decided
//! in the receipt transaction without any PTY I/O: the decision checks the
//! policy and enqueues one `queued` row ([`enqueue`]). After the commit, the
//! dispatcher ([`crate::shell::dispatch`]) claims the oldest row
//! ([`super::Store::claim_shell_effect`]) **before** the I/O and ends it as
//! `confirmed` or `failed` ([`super::Store::finish_shell_effect`]).
//!
//! A shell belongs to one station process (A10). Start recovery
//! ([`recover`]) therefore never dispatches an older row: `queued` becomes
//! `retired_not_dispatched` and `claimed` becomes `outcome_unknown`.

use fleetdeck_proto::CommandId;
use fleetdeck_proto::shell::ShellChange;
use rusqlite::{Connection, OptionalExtension as _, params};

use super::{Result, Store, call};

/// One claimed shell effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellEffect {
    pub effect_id: String,
    pub command_id: CommandId,
    pub change: ShellChange,
}

/// The stored state of one shell effect (tests and diagnostics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellEffectState {
    pub state: String,
    pub detail: Option<String>,
}

/// A shell change that is committed but not yet done: it counts for the
/// policy checks of the next decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingShell {
    pub op: String,
    pub shell_id: String,
}

fn op(change: &ShellChange) -> &'static str {
    match change {
        ShellChange::Open { .. } => "open",
        ShellChange::Resize { .. } => "resize",
        ShellChange::Close { .. } => "close",
    }
}

/// Enqueues the effect of one accepted shell command (decision transaction).
///
/// # Errors
/// A database or JSON error.
pub fn enqueue(
    conn: &Connection,
    effect_id: &str,
    command_id: &CommandId,
    change: &ShellChange,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO shell_effects
           (effect_id, command_id, shell_id, op, change, state, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?6)",
        params![
            effect_id,
            command_id.as_str(),
            change.shell_id(),
            op(change),
            serde_json::to_string(change)?,
            now
        ],
    )?;
    Ok(())
}

/// The committed shell changes that are not done yet, oldest first.
///
/// # Errors
/// A database error.
pub fn pending(conn: &Connection) -> Result<Vec<PendingShell>> {
    let mut stmt = conn.prepare(
        "SELECT op, shell_id FROM shell_effects WHERE state IN ('queued', 'claimed')
         ORDER BY created_at, effect_id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(PendingShell {
                op: r.get(0)?,
                shell_id: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Start recovery: no shell effect of an earlier process is dispatched.
/// Returns (retired, unknown).
///
/// # Errors
/// A database error.
pub fn recover(conn: &Connection, now: i64) -> Result<(usize, usize)> {
    let retired = conn.execute(
        "UPDATE shell_effects SET state = 'retired_not_dispatched', updated_at = ?1,
             detail = 'station_restart'
         WHERE state = 'queued'",
        params![now],
    )?;
    let unknown = conn.execute(
        "UPDATE shell_effects SET state = 'outcome_unknown', updated_at = ?1,
             detail = 'station_restart'
         WHERE state = 'claimed'",
        params![now],
    )?;
    Ok((retired, unknown))
}

impl Store {
    /// Claims the oldest queued shell effect before its PTY I/O.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn claim_shell_effect(&self) -> Result<Option<ShellEffect>> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let row = tx
                .query_row(
                    "SELECT effect_id, command_id, change FROM shell_effects
                     WHERE state = 'queued' ORDER BY created_at, effect_id LIMIT 1",
                    [],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            let Some((effect_id, command_id, change)) = row else {
                return Ok(None);
            };
            tx.execute(
                "UPDATE shell_effects SET state = 'claimed', updated_at = ?2
                 WHERE effect_id = ?1 AND state = 'queued'",
                params![effect_id, now],
            )?;
            tx.commit()?;
            Ok(Some(ShellEffect {
                effect_id,
                command_id: CommandId::from(command_id),
                change: serde_json::from_str(&change)?,
            }))
        })
        .await
    }

    /// Ends a claimed shell effect: `confirmed`, or `failed` with the reason.
    ///
    /// # Errors
    /// A database error.
    pub async fn finish_shell_effect(
        &self,
        effect_id: String,
        outcome: std::result::Result<(), String>,
    ) -> Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let (state, detail) = match outcome {
                Ok(()) => ("confirmed", None),
                Err(e) => ("failed", Some(e)),
            };
            conn.execute(
                "UPDATE shell_effects SET state = ?2, detail = ?3, updated_at = ?4
                 WHERE effect_id = ?1 AND state = 'claimed'",
                params![effect_id, state, detail, now],
            )?;
            Ok(())
        })
        .await
    }

    /// The state of the shell effect of `command_id`.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_effect(&self, command_id: CommandId) -> Result<Option<ShellEffectState>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT state, detail FROM shell_effects WHERE command_id = ?1",
                    params![command_id.as_str()],
                    |r| {
                        Ok(ShellEffectState {
                            state: r.get(0)?,
                            detail: r.get(1)?,
                        })
                    },
                )
                .optional()?)
        })
        .await
    }
}
