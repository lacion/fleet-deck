//! The rows of the `!` shell commands (B8, DL-196, design `bang-commands.md`).
//!
//! A row is written in the decision of the command, with its effect, in the
//! state `queued`. A row is **unsettled** while its state is not terminal or
//! `advance_pending = 1`. Every settlement is one guarded transaction that
//! sets a terminal state and `advance_pending = 1`; the advance (under the
//! folder fence) then increments the folder generation and clears the
//! marker. So no checkpoint can be taken at the old generation after a
//! settlement, also across a crash.
//!
//! `shell_admission` in `station_flags` is `open` or `closed`; a missing row
//! means closed. Shutdown closes it with the cancel sweep in one
//! transaction; the station start opens it after the start-up settlement.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::ProviderKind;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Decision, Result, Store, StoreError, call};

/// The states of a shell row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellState {
    Queued,
    Running,
    /// A Codex command whose end is not proven yet.
    AwaitingProof,
    Done,
    Failed,
    Cancelled,
    Timeout,
    Unknown,
}

impl ShellState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::AwaitingProof => "awaiting_proof",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Unknown => "unknown",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "awaiting_proof" => Self::AwaitingProof,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "timeout" => Self::Timeout,
            "unknown" => Self::Unknown,
            other => return Err(StoreError::Corrupt(format!("shell state {other}"))),
        })
    }

    /// True for `done`, `failed`, `cancelled`, `timeout` and `unknown`.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running | Self::AwaitingProof)
    }
}

/// The SQL list of the non-terminal states.
const OPEN: &str = "('queued', 'running', 'awaiting_proof')";
const TERMINAL: &str = "('done', 'failed', 'cancelled', 'timeout', 'unknown')";

/// One stored shell row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellRow {
    pub effect_id: String,
    pub session_id: SessionId,
    pub provider: String,
    pub worktree_root: String,
    pub command: String,
    pub state: ShellState,
    pub exit_code: Option<i32>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub cancel_requested: bool,
    pub advance_pending: bool,
    pub generation: Option<i64>,
    pub included_in: Option<String>,
    pub created_at: i64,
    pub ended_at: Option<i64>,
}

impl ShellRow {
    /// True while the row blocks its folder: not terminal, or its advance
    /// is pending.
    #[must_use]
    pub fn unsettled(&self) -> bool {
        !self.state.is_terminal() || self.advance_pending
    }
}

/// The provider column text.
#[must_use]
pub fn provider_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Claude => "claude",
        ProviderKind::Codex => "codex",
        ProviderKind::Unknown => "unknown",
    }
}

const COLUMNS: &str = "effect_id, session_id, provider, worktree_root, command, state, \
     exit_code, stdout_tail, stderr_tail, cancel_requested, advance_pending, generation, \
     included_in, created_at, ended_at";

fn read(r: &Row<'_>) -> rusqlite::Result<(ShellRow, String)> {
    let state: String = r.get(5)?;
    Ok((
        ShellRow {
            effect_id: r.get(0)?,
            session_id: SessionId::from(r.get::<_, String>(1)?),
            provider: r.get(2)?,
            worktree_root: r.get(3)?,
            command: r.get(4)?,
            state: ShellState::Queued,
            exit_code: r.get(6)?,
            stdout_tail: r.get(7)?,
            stderr_tail: r.get(8)?,
            cancel_requested: r.get::<_, i64>(9)? != 0,
            advance_pending: r.get::<_, i64>(10)? != 0,
            generation: r.get(11)?,
            included_in: r.get(12)?,
            created_at: r.get(13)?,
            ended_at: r.get(14)?,
        },
        state,
    ))
}

fn select(conn: &Connection, filter: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<ShellRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM shell_runs WHERE {filter} ORDER BY created_at, effect_id"
    ))?;
    let rows = stmt
        .query_map(args, read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(mut row, state)| {
            row.state = ShellState::parse(&state)?;
            Ok(row)
        })
        .collect()
}

/// A new row in the state `queued` (decision transaction).
pub(crate) fn insert(
    conn: &Connection,
    effect_id: &str,
    session: &SessionId,
    provider: ProviderKind,
    root: &str,
    command: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO shell_runs
           (effect_id, session_id, provider, worktree_root, command, state, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6)",
        params![
            effect_id,
            session.as_str(),
            provider_name(provider),
            root,
            command,
            now
        ],
    )?;
    Ok(())
}

pub(crate) fn get(conn: &Connection, effect_id: &str) -> Result<Option<ShellRow>> {
    Ok(select(conn, "effect_id = ?1", &[&effect_id])?.pop())
}

/// True while a row of `root` is unsettled. The roots compare exactly.
pub(crate) fn root_unsettled(conn: &Connection, root: &str) -> Result<bool> {
    Ok(conn.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM shell_runs WHERE worktree_root = ?1
               AND (state IN {OPEN} OR advance_pending = 1))"
        ),
        [root],
        |r| r.get(0),
    )?)
}

/// True when a row of any root is unsettled.
pub(crate) fn any_unsettled(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM shell_runs
               WHERE state IN {OPEN} OR advance_pending = 1)"
        ),
        [],
        |r| r.get(0),
    )?)
}

/// The rows of a session whose state is not terminal, oldest first.
pub(crate) fn open_of_session(conn: &Connection, session: &SessionId) -> Result<Vec<ShellRow>> {
    select(
        conn,
        &format!("session_id = ?1 AND state IN {OPEN}"),
        &[&session.as_str()],
    )
}

pub(crate) fn admission_open(conn: &Connection) -> Result<bool> {
    let value: Option<Option<String>> = conn
        .query_row(
            "SELECT value FROM station_flags WHERE key = 'shell_admission'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(value.flatten().as_deref() == Some("open"))
}

fn set_admission(conn: &Connection, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO station_flags (key, value) VALUES ('shell_admission', ?1)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [value],
    )?;
    Ok(())
}

pub(crate) fn generation(conn: &Connection, root: &str) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT generation FROM worktree_generations WHERE worktree_root = ?1",
            [root],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// Moves a row to `running` unless a cancel is stored or the row left
/// `queued`. One statement, so the check and the move are one step.
pub(crate) fn set_running(conn: &Connection, effect_id: &str) -> Result<bool> {
    let n = conn.execute(
        "UPDATE shell_runs SET state = 'running'
         WHERE effect_id = ?1 AND state = 'queued' AND cancel_requested = 0",
        [effect_id],
    )?;
    Ok(n == 1)
}

/// Settles a row: a terminal state, the result and `advance_pending = 1`.
/// Guarded: only a row in a non-terminal state changes.
pub(crate) fn settle(
    conn: &Connection,
    effect_id: &str,
    state: ShellState,
    exit_code: Option<i32>,
    stdout_tail: Option<&str>,
    stderr_tail: Option<&str>,
    now: i64,
) -> Result<bool> {
    if !state.is_terminal() {
        return Err(StoreError::Corrupt(format!(
            "a shell row settles in a terminal state, not {}",
            state.as_str()
        )));
    }
    let n = conn.execute(
        &format!(
            "UPDATE shell_runs SET state = ?2, exit_code = ?3, stdout_tail = ?4,
               stderr_tail = ?5, advance_pending = 1, ended_at = ?6
             WHERE effect_id = ?1 AND state IN {OPEN}"
        ),
        params![
            effect_id,
            state.as_str(),
            exit_code,
            stdout_tail,
            stderr_tail,
            now
        ],
    )?;
    Ok(n == 1)
}

pub(crate) fn set_awaiting_proof(
    conn: &Connection,
    effect_id: &str,
    from: ShellState,
) -> Result<bool> {
    if !matches!(from, ShellState::Queued | ShellState::Running) {
        return Ok(false);
    }
    let n = conn.execute(
        "UPDATE shell_runs SET state = 'awaiting_proof' WHERE effect_id = ?1 AND state = ?2",
        params![effect_id, from.as_str()],
    )?;
    Ok(n == 1)
}

/// Stores a cancel request on a row whose state is not terminal.
pub(crate) fn request_cancel(conn: &Connection, effect_id: &str) -> Result<bool> {
    let n = conn.execute(
        &format!(
            "UPDATE shell_runs SET cancel_requested = 1
             WHERE effect_id = ?1 AND state IN {OPEN}"
        ),
        [effect_id],
    )?;
    Ok(n == 1)
}

/// The advance of a folder generation (call it in one transaction).
/// At the start of a command it stores the new value on the row. At the
/// settlement it clears `advance_pending`; a row whose advance is not
/// pending changes nothing and the current generation returns.
pub(crate) fn advance(
    conn: &Connection,
    effect_id: &str,
    root: &str,
    at_start: bool,
) -> Result<i64> {
    if !at_start {
        let pending: bool = conn
            .query_row(
                "SELECT advance_pending = 1 FROM shell_runs WHERE effect_id = ?1",
                [effect_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if !pending {
            return generation(conn, root);
        }
    }
    conn.execute(
        "INSERT INTO worktree_generations (worktree_root, generation) VALUES (?1, 1)
         ON CONFLICT (worktree_root) DO UPDATE SET generation = generation + 1",
        [root],
    )?;
    let new = generation(conn, root)?;
    if at_start {
        conn.execute(
            "UPDATE shell_runs SET generation = ?2 WHERE effect_id = ?1",
            params![effect_id, new],
        )?;
    } else {
        conn.execute(
            "UPDATE shell_runs SET advance_pending = 0 WHERE effect_id = ?1",
            [effect_id],
        )?;
    }
    Ok(new)
}

pub(crate) fn pending_advances(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT effect_id, worktree_root FROM shell_runs WHERE advance_pending = 1
         ORDER BY ended_at, effect_id",
    )?;
    Ok(stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The Claude rows of a session in a terminal state that no input carried
/// yet, oldest first.
pub(crate) fn attachable(conn: &Connection, session: &SessionId) -> Result<Vec<ShellRow>> {
    select(
        conn,
        &format!(
            "session_id = ?1 AND provider = 'claude' AND state IN {TERMINAL}
             AND included_in IS NULL"
        ),
        &[&session.as_str()],
    )
}

/// Station start: settles `cancelled` every non-terminal row whose effect is
/// retired as not dispatched or missing. A row whose effect is still queued
/// or was claimed stays as it is. Each update is guarded by the row state.
pub(crate) fn cancel_unclaimed(conn: &Connection, now: i64) -> Result<usize> {
    let mut stmt = conn.prepare(&format!(
        "SELECT r.effect_id FROM shell_runs r
         LEFT JOIN effects e ON e.effect_id = r.effect_id
         WHERE r.state IN {OPEN}
           AND (e.effect_id IS NULL OR e.state = 'retired_not_dispatched')"
    ))?;
    let ids: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut changed = 0;
    for id in ids {
        if settle(conn, &id, ShellState::Cancelled, None, None, None, now)? {
            changed += 1;
        }
    }
    Ok(changed)
}

/// Marks rows as carried by `input_id`. Only a row that no input carried
/// changes. Returns how many rows changed.
pub(crate) fn include(conn: &Connection, effect_ids: &[String], input_id: &str) -> Result<usize> {
    let mut changed = 0;
    for id in effect_ids {
        changed += conn.execute(
            "UPDATE shell_runs SET included_in = ?2
             WHERE effect_id = ?1 AND included_in IS NULL",
            params![id, input_id],
        )?;
    }
    Ok(changed)
}

/// Why the revert barrier refuses (B8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Barrier {
    /// A shell row of the root is unsettled.
    Running,
    /// The folder generation is higher than the one of the target turn's
    /// start checkpoint.
    RanAfterTurn,
}

/// The revert barrier for the turn `app_turn` in `root`: a start checkpoint
/// without a stored generation counts as 0.
pub(crate) fn barrier(conn: &Connection, root: &str, app_turn: &str) -> Result<Option<Barrier>> {
    if root_unsettled(conn, root)? {
        return Ok(Some(Barrier::Running));
    }
    let stored: Option<i64> = conn
        .query_row(
            "SELECT generation FROM turn_checkpoints WHERE app_turn = ?1",
            [app_turn],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok((stored.unwrap_or(0) < generation(conn, root)?).then_some(Barrier::RanAfterTurn))
}

impl Decision<'_> {
    /// The revert barrier for the turn `app_turn` in `root` (B8).
    ///
    /// # Errors
    /// A database error.
    pub fn shell_barrier(&self, root: &str, app_turn: &str) -> Result<Option<Barrier>> {
        barrier(self.connection(), root, app_turn)
    }

    /// True while a shell row of `root` is unsettled (B8).
    ///
    /// # Errors
    /// A database error.
    pub fn shell_root_unsettled(&self, root: &str) -> Result<bool> {
        root_unsettled(self.connection(), root)
    }

    /// True while `shell_admission` is `open` (B8).
    ///
    /// # Errors
    /// A database error.
    pub fn shell_admission_open(&self) -> Result<bool> {
        admission_open(self.connection())
    }

    /// The folder generation of `root` (0 when it never advanced).
    ///
    /// # Errors
    /// A database error.
    pub fn root_generation(&self, root: &str) -> Result<i64> {
        generation(self.connection(), root)
    }

    /// One shell row.
    ///
    /// # Errors
    /// A database error.
    pub fn shell_run(&self, effect_id: &str) -> Result<Option<ShellRow>> {
        get(self.connection(), effect_id)
    }

    /// The non-terminal rows of a session.
    ///
    /// # Errors
    /// A database error.
    pub fn shell_open_rows(&self, session: &SessionId) -> Result<Vec<ShellRow>> {
        open_of_session(self.connection(), session)
    }

    /// The row of a new command, written with its effect (state `queued`).
    ///
    /// # Errors
    /// A database error.
    pub fn shell_insert(
        &mut self,
        effect_id: &str,
        session: &SessionId,
        provider: ProviderKind,
        root: &str,
        command: &str,
    ) -> Result<()> {
        let now = self.now();
        insert(
            self.connection(),
            effect_id,
            session,
            provider,
            root,
            command,
            now,
        )
    }

    /// Settles a row (see [`settle`]); `false` when it was terminal.
    ///
    /// # Errors
    /// A database error.
    pub fn shell_settle(&mut self, effect_id: &str, state: ShellState) -> Result<bool> {
        let now = self.now();
        settle(self.connection(), effect_id, state, None, None, None, now)
    }

    /// Stores a cancel request on a row that is not terminal.
    ///
    /// # Errors
    /// A database error.
    pub fn shell_request_cancel(&mut self, effect_id: &str) -> Result<bool> {
        request_cancel(self.connection(), effect_id)
    }
}

impl Store {
    /// One shell row.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_run(&self, effect_id: String) -> Result<Option<ShellRow>> {
        call(&self.inner.db, move |conn| get(conn, &effect_id)).await
    }

    /// The rows that are unsettled: not terminal, or an advance pending.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_runs_unsettled(&self) -> Result<Vec<ShellRow>> {
        call(&self.inner.db, move |conn| {
            select(
                conn,
                &format!("state IN {OPEN} OR advance_pending = 1"),
                &[],
            )
        })
        .await
    }

    /// True while a row of `root` is unsettled.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_root_unsettled(&self, root: String) -> Result<bool> {
        call(&self.inner.db, move |conn| root_unsettled(conn, &root)).await
    }

    /// True while any row of any root is unsettled.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_any_unsettled(&self) -> Result<bool> {
        call(&self.inner.db, move |conn| any_unsettled(conn)).await
    }

    /// Moves a `queued` row to `running` in one transaction; `false` when a
    /// cancel is stored (or the row is not `queued`).
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_set_running(&self, effect_id: String) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let moved = set_running(&tx, &effect_id)?;
            tx.commit()?;
            Ok(moved)
        })
        .await
    }

    /// Settles a row in one transaction (terminal state, result,
    /// `advance_pending = 1`). `false` when the row was terminal already.
    ///
    /// # Errors
    /// A database error, or a state that is not terminal.
    pub async fn shell_settle(
        &self,
        effect_id: String,
        state: ShellState,
        exit_code: Option<i32>,
        stdout_tail: Option<String>,
        stderr_tail: Option<String>,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let moved = settle(
                &tx,
                &effect_id,
                state,
                exit_code,
                stdout_tail.as_deref(),
                stderr_tail.as_deref(),
                now,
            )?;
            tx.commit()?;
            Ok(moved)
        })
        .await
    }

    /// Moves a row from `from` to `awaiting_proof`.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_set_awaiting_proof(
        &self,
        effect_id: String,
        from: ShellState,
    ) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            set_awaiting_proof(conn, &effect_id, from)
        })
        .await
    }

    /// Stores a cancel request on a row that is not terminal.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_request_cancel(&self, effect_id: String) -> Result<bool> {
        call(&self.inner.db, move |conn| request_cancel(conn, &effect_id)).await
    }

    /// Shutdown: closes the shell admission and stores a cancel request on
    /// every row whose state is not terminal, in one transaction. Returns
    /// their effect ids.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_close_admission_and_cancel_all(&self) -> Result<Vec<String>> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            set_admission(&tx, "closed")?;
            let ids = select(&tx, &format!("state IN {OPEN}"), &[])?
                .into_iter()
                .map(|r| r.effect_id)
                .collect::<Vec<_>>();
            tx.execute(
                &format!("UPDATE shell_runs SET cancel_requested = 1 WHERE state IN {OPEN}"),
                [],
            )?;
            tx.commit()?;
            Ok(ids)
        })
        .await
    }

    /// Opens the shell admission (the station start, after the start-up
    /// settlement).
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_open_admission(&self) -> Result<()> {
        call(&self.inner.db, move |conn| set_admission(conn, "open")).await
    }

    /// True while the shell admission is open.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_admission_open(&self) -> Result<bool> {
        call(&self.inner.db, move |conn| admission_open(conn)).await
    }

    /// The revert barrier for the turn `app_turn` in `root`.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_barrier(&self, root: String, app_turn: String) -> Result<Option<Barrier>> {
        call(&self.inner.db, move |conn| barrier(conn, &root, &app_turn)).await
    }

    /// The folder generation of `root`.
    ///
    /// # Errors
    /// A database error.
    pub async fn root_generation(&self, root: String) -> Result<i64> {
        call(&self.inner.db, move |conn| generation(conn, &root)).await
    }

    /// The advance of the folder generation of `root` for the row
    /// `effect_id`, in one transaction (see [`advance`]). Returns the
    /// generation.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_advance(
        &self,
        effect_id: String,
        root: String,
        at_start: bool,
    ) -> Result<i64> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let new = advance(&tx, &effect_id, &root, at_start)?;
            tx.commit()?;
            Ok(new)
        })
        .await
    }

    /// The rows that settled and still wait for their advance, with their
    /// roots, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_pending_advances(&self) -> Result<Vec<(String, String)>> {
        call(&self.inner.db, move |conn| pending_advances(conn)).await
    }

    /// The Claude rows of a session that a later input can carry.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_attachable(&self, session: SessionId) -> Result<Vec<ShellRow>> {
        call(&self.inner.db, move |conn| attachable(conn, &session)).await
    }

    /// Station start: settles `cancelled` the rows whose effect is retired or
    /// missing.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_cancel_unclaimed(&self) -> Result<usize> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let n = cancel_unclaimed(&tx, now)?;
            tx.commit()?;
            Ok(n)
        })
        .await
    }

    /// Marks rows as carried by `input_id`; returns how many changed.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_include(&self, effect_ids: Vec<String>, input_id: String) -> Result<usize> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let n = include(&tx, &effect_ids, &input_id)?;
            tx.commit()?;
            Ok(n)
        })
        .await
    }
}

#[path = "shell_runs_ctx.rs"]
mod ctx;

#[cfg(test)]
#[path = "shell_runs_tests.rs"]
mod tests;
