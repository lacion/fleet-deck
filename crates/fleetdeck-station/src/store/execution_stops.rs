//! The durable rows of the stop of a lost run (BG-4, design
//! `docs/v2/design/board-gaps.md` §5): the member witnesses of a shim run
//! and one `execution_stops` row for each `session.stop_execution` command.
//!
//! - **Witnesses** record the processes that the station saw as members of
//!   the domain of a run while the leader was alive. A witness authorizes
//!   only itself. A shim keeps at most [`WITNESS_LIMIT`] rows; the oldest
//!   go first.
//! - **A stop row** is `authorized` in the command transaction, `attempted`
//!   before the first signal (a compare-and-set), and then `done`,
//!   `failed` or `unknown`. A settled row never changes. After a restart,
//!   [`Store::recover_execution_stops`] turns `attempted` into `unknown`
//!   and `authorized` into `failed`; it sends no signal.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::execution_stop::{ExecutionStopOutcome, StopOutcome, code};
use rusqlite::{Connection, OptionalExtension as _, params};

use super::{Result, Store, StoreError, call, to_i64};

/// The most witness rows of one shim.
pub const WITNESS_LIMIT: i64 = 512;

pub const AUTHORIZED: &str = "authorized";
pub const ATTEMPTED: &str = "attempted";
pub const DONE: &str = "done";
pub const FAILED: &str = "failed";
pub const UNKNOWN: &str = "unknown";

/// One witness: a process that was a member while the leader lived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Witness {
    pub pid: u32,
    pub start_time: u64,
    pub boot_id: String,
}

/// One `execution_stops` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopRow {
    pub command_id: String,
    pub session_id: SessionId,
    pub shim_id: String,
    pub plan_id: String,
    pub state: String,
    pub code: Option<String>,
    pub term_sent: u32,
    pub kill_sent: u32,
    pub gone: u32,
    pub skipped: u32,
    pub created_at: i64,
    pub updated_at: i64,
}

impl StopRow {
    /// The wire outcome of a settled row (`done`, `failed` or `unknown`);
    /// `None` for `authorized` and `attempted`.
    #[must_use]
    pub fn outcome(&self) -> Option<ExecutionStopOutcome> {
        let outcome = match self.state.as_str() {
            DONE => StopOutcome::Done,
            FAILED => StopOutcome::Failed,
            UNKNOWN => StopOutcome::Unknown,
            _ => return None,
        };
        Some(ExecutionStopOutcome {
            command_id: self.command_id.clone(),
            outcome,
            code: self.code.clone(),
            term_sent: self.term_sent,
            kill_sent: self.kill_sent,
            gone: self.gone,
            skipped: self.skipped,
        })
    }
}

/// What a settled stop records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopResult {
    pub state: StopOutcome,
    pub code: Option<&'static str>,
    pub term_sent: u32,
    pub kill_sent: u32,
    pub gone: u32,
    pub skipped: u32,
}

impl StopResult {
    /// A failure before any signal.
    #[must_use]
    pub fn failed(code: &'static str) -> Self {
        Self {
            state: StopOutcome::Failed,
            code: Some(code),
            term_sent: 0,
            kill_sent: 0,
            gone: 0,
            skipped: 0,
        }
    }
}

const COLUMNS: &str = "command_id, session_id, shim_id, plan_id, state, code, term_sent, kill_sent,
     gone, skipped, created_at, updated_at";

fn count(v: i64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<StopRow> {
    Ok(StopRow {
        command_id: r.get(0)?,
        session_id: SessionId::from(r.get::<_, String>(1)?),
        shim_id: r.get(2)?,
        plan_id: r.get(3)?,
        state: r.get(4)?,
        code: r.get(5)?,
        term_sent: count(r.get(6)?),
        kill_sent: count(r.get(7)?),
        gone: count(r.get(8)?),
        skipped: count(r.get(9)?),
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

/// Inserts the `authorized` row of a command, in the decision transaction.
///
/// # Errors
/// A database error (a duplicate command id too).
pub fn insert_authorized(
    conn: &Connection,
    command_id: &str,
    session: &SessionId,
    shim_id: &str,
    plan_id: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO execution_stops (command_id, session_id, shim_id, plan_id, state,
             created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'authorized', ?5, ?5)",
        params![command_id, session.as_str(), shim_id, plan_id, now],
    )?;
    Ok(())
}

fn get(conn: &Connection, command_id: &str) -> Result<Option<StopRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM execution_stops WHERE command_id = ?1"),
            [command_id],
            row_of,
        )
        .optional()?)
}

impl Store {
    /// Records the members of the domain of `shim_id` that the station saw
    /// while the leader was alive. A known (PID, start time) pair is
    /// refreshed. The oldest rows beyond [`WITNESS_LIMIT`] are deleted.
    ///
    /// # Errors
    /// A database error.
    pub async fn execution_witnesses_record(
        &self,
        shim_id: String,
        boot_id: String,
        members: Vec<(u32, u64)>,
    ) -> Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            for (pid, start_time) in &members {
                tx.execute(
                    "INSERT INTO execution_witnesses
                         (shim_id, pid, start_time, boot_id, recorded_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (shim_id, pid, start_time)
                     DO UPDATE SET boot_id = excluded.boot_id,
                                   recorded_at = excluded.recorded_at",
                    params![shim_id, i64::from(*pid), to_i64(*start_time)?, boot_id, now],
                )?;
            }
            tx.execute(
                "DELETE FROM execution_witnesses
                 WHERE shim_id = ?1 AND rowid NOT IN
                     (SELECT rowid FROM execution_witnesses WHERE shim_id = ?1
                      ORDER BY recorded_at DESC, rowid DESC LIMIT ?2)",
                params![shim_id, WITNESS_LIMIT],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// The witnesses of `shim_id`.
    ///
    /// # Errors
    /// A database error.
    pub async fn execution_witnesses(&self, shim_id: String) -> Result<Vec<Witness>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT pid, start_time, boot_id FROM execution_witnesses
                 WHERE shim_id = ?1 ORDER BY pid, start_time",
            )?;
            let rows = stmt
                .query_map([shim_id.as_str()], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get(2)?))
                })?
                .collect::<rusqlite::Result<Vec<(i64, i64, String)>>>()?;
            rows.into_iter()
                .map(|(pid, start, boot_id)| {
                    Ok(Witness {
                        pid: u32::try_from(pid)
                            .map_err(|_| StoreError::Corrupt(format!("witness pid {pid}")))?,
                        start_time: u64::try_from(start).map_err(|_| {
                            StoreError::Corrupt(format!("witness start time {start}"))
                        })?,
                        boot_id,
                    })
                })
                .collect()
        })
        .await
    }

    /// One stop row.
    ///
    /// # Errors
    /// A database error.
    pub async fn execution_stop(&self, command_id: String) -> Result<Option<StopRow>> {
        call(&self.inner.db, move |conn| get(conn, &command_id)).await
    }

    /// Sets an `authorized` row to `attempted`, before the first signal.
    /// `false` when the row is not `authorized` any more (it is not
    /// signalled then).
    ///
    /// # Errors
    /// A database error.
    pub async fn execution_stop_attempt(&self, command_id: String) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let n = conn.execute(
                "UPDATE execution_stops SET state = 'attempted', updated_at = ?2
                 WHERE command_id = ?1 AND state = 'authorized'",
                params![command_id, now],
            )?;
            Ok(n == 1)
        })
        .await
    }

    /// Settles a row from `authorized` or `attempted`. A settled row never
    /// changes. `false` when the row was settled already or does not exist.
    ///
    /// # Errors
    /// A database error.
    pub async fn execution_stop_settle(
        &self,
        command_id: String,
        result: StopResult,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let n = conn.execute(
                "UPDATE execution_stops
                 SET state = ?2, code = ?3, term_sent = ?4, kill_sent = ?5, gone = ?6,
                     skipped = ?7, updated_at = ?8
                 WHERE command_id = ?1 AND state IN ('authorized', 'attempted')",
                params![
                    command_id,
                    result.state.as_str(),
                    result.code,
                    i64::from(result.term_sent),
                    i64::from(result.kill_sent),
                    i64::from(result.gone),
                    i64::from(result.skipped),
                    now
                ],
            )?;
            Ok(n == 1)
        })
        .await
    }

    /// The newest settled stop (`done`, `failed` or `unknown`) of
    /// `shim_id`. A row in `authorized` or `attempted` is skipped.
    ///
    /// # Errors
    /// A database error.
    pub async fn execution_stop_last(&self, shim_id: String) -> Result<Option<StopRow>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM execution_stops
                         WHERE shim_id = ?1 AND state IN ('done', 'failed', 'unknown')
                         ORDER BY created_at DESC, rowid DESC LIMIT 1"
                    ),
                    [shim_id.as_str()],
                    row_of,
                )
                .optional()?)
        })
        .await
    }

    /// Station start: an `attempted` row becomes `unknown`
    /// (`outcome_unknown`) and an `authorized` row becomes `failed`
    /// (`plan_changed`). It sends no signal. Returns
    /// `(unknown, failed)` counts.
    ///
    /// # Errors
    /// A database error.
    pub async fn recover_execution_stops(&self) -> Result<(usize, usize)> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let unknown = tx.execute(
                "UPDATE execution_stops SET state = 'unknown', code = ?1, updated_at = ?2
                 WHERE state = 'attempted'",
                params![code::OUTCOME_UNKNOWN, now],
            )?;
            let failed = tx.execute(
                "UPDATE execution_stops SET state = 'failed', code = ?1, updated_at = ?2
                 WHERE state = 'authorized'",
                params![code::PLAN_CHANGED, now],
            )?;
            tx.commit()?;
            Ok((unknown, failed))
        })
        .await
    }
}

#[cfg(test)]
#[path = "execution_stops_tests.rs"]
mod tests;
