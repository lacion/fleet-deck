//! The rows of the app tool calls (B15, design `mcp-apps.md` §4.4 and §4.6.1,
//! table `mcp_app_calls`).
//!
//! A row is **unsettled** while `state IN ('running', 'uncertain')` or
//! `advance_pending = 1`. The terminal states are `done`, `failed`, `ended`
//! and `released`. A non-read-only call advances the folder generation of its
//! root at its start (in [`Store::app_call_begin`]) and after its settlement
//! ([`Store::app_call_advance`]), with the same `worktree_generations` rows as
//! the `!` shell commands, so the revert barrier meets both. This file does
//! not touch `shell_runs.rs`.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::apps::AppUncertainCause;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Result, Store, StoreError, call, shell_runs};

/// Why an `uncertain` row is open (`cause` column).
pub type UncertainCause = AppUncertainCause;

/// The most rows that [`Store::app_calls_open`] returns.
pub const OPEN_CALLS_LIMIT: usize = 32;

fn cause_str(cause: &UncertainCause) -> &'static str {
    match cause {
        UncertainCause::StationRestart => "station_restart",
        UncertainCause::ProviderError => "provider_error",
    }
}

fn parse_cause(text: &str) -> Result<UncertainCause> {
    match text {
        "station_restart" => Ok(UncertainCause::StationRestart),
        "provider_error" => Ok(UncertainCause::ProviderError),
        other => Err(StoreError::Corrupt(format!("app call cause {other}"))),
    }
}

/// The state of a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Running,
    Uncertain,
    Done,
    Failed,
    Ended,
    Released,
}

impl CallState {
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "running" => Self::Running,
            "uncertain" => Self::Uncertain,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "ended" => Self::Ended,
            "released" => Self::Released,
            other => return Err(StoreError::Corrupt(format!("app call state {other}"))),
        })
    }

    /// True for `done`, `failed`, `ended` and `released`.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running | Self::Uncertain)
    }
}

/// The terminal state that [`Store::app_call_settle`] sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCallEnd {
    Done,
    Failed,
    Ended,
    Released,
}

impl AppCallEnd {
    fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Ended => "ended",
            Self::Released => "released",
        }
    }
}

/// A new row, written in the state `running`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAppCall {
    pub call_id: String,
    pub session_id: SessionId,
    pub app_id: String,
    /// The same text as `shell_runs.worktree_root` and the revert rows.
    pub worktree_root: String,
    pub server: String,
    pub tool: String,
    /// `true`: the tool is read-only and the call advances no generation.
    pub read_only: bool,
    /// The `ProcessIncarnation` text of the session's process.
    pub process: String,
    /// `store.incarnation()` of the station process that starts the call.
    pub incarnation: String,
}

/// One `mcp_app_calls` row.
#[derive(Debug, Clone, PartialEq)]
pub struct AppCallRow {
    pub call_id: String,
    pub session_id: SessionId,
    pub app_id: String,
    pub worktree_root: String,
    pub server: String,
    pub tool: String,
    pub read_only: bool,
    pub process: String,
    pub incarnation: String,
    pub state: CallState,
    pub cause: Option<UncertainCause>,
    pub advance_pending: bool,
    pub generation: Option<i64>,
    pub created_at: i64,
    pub ended_at: Option<i64>,
}

/// Why the app part of the revert barrier refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCallBarrier {
    /// A row of the root is unsettled.
    Running,
    /// A call started in the root after the start checkpoint of the target
    /// turn.
    RanAfterTurn,
}

/// The answer of [`Store::app_call_release`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// The row was `uncertain` and is `released` now.
    Released,
    /// The row is `running`: the station still waits for it.
    Running,
    /// The row is terminal.
    NotOpen,
    /// No row of that session has the id.
    Unknown,
}

const COLUMNS: &str = "call_id, session_id, app_id, worktree_root, server, tool, read_only, \
     process, incarnation, state, cause, advance_pending, generation, created_at, ended_at";

type Raw = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
    String,
    String,
    Option<String>,
    i64,
    Option<i64>,
    i64,
    Option<i64>,
);

fn raw(r: &Row<'_>) -> rusqlite::Result<Raw> {
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
        r.get(14)?,
    ))
}

fn decode(raw: Raw) -> Result<AppCallRow> {
    let (
        call_id,
        session_id,
        app_id,
        worktree_root,
        server,
        tool,
        read_only,
        process,
        incarnation,
        state,
        cause,
        advance_pending,
        generation,
        created_at,
        ended_at,
    ) = raw;
    Ok(AppCallRow {
        call_id,
        session_id: SessionId::from(session_id),
        app_id,
        worktree_root,
        server,
        tool,
        read_only: read_only != 0,
        process,
        incarnation,
        state: CallState::parse(&state)?,
        cause: cause.as_deref().map(parse_cause).transpose()?,
        advance_pending: advance_pending != 0,
        generation,
        created_at,
        ended_at,
    })
}

fn select(
    conn: &Connection,
    filter: &str,
    args: &[&dyn rusqlite::ToSql],
    limit: Option<usize>,
) -> Result<Vec<AppCallRow>> {
    let limit = limit.map_or(String::new(), |n| format!(" LIMIT {n}"));
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM mcp_app_calls WHERE {filter} ORDER BY created_at, call_id{limit}"
    ))?;
    let raws = stmt
        .query_map(args, raw)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(decode).collect()
}

/// The advance of the folder generation of `root` (the SQL of
/// `shell_runs::advance`). Returns the new generation.
fn bump(conn: &Connection, root: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO worktree_generations (worktree_root, generation) VALUES (?1, 1)
         ON CONFLICT (worktree_root) DO UPDATE SET generation = generation + 1",
        [root],
    )?;
    shell_runs::generation(conn, root)
}

fn begin(conn: &Connection, new: &NewAppCall, now: i64) -> Result<Option<i64>> {
    conn.execute(
        "INSERT INTO mcp_app_calls (call_id, session_id, app_id, worktree_root, server, tool,
             read_only, process, incarnation, state, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'running', ?10)",
        params![
            new.call_id,
            new.session_id.as_str(),
            new.app_id,
            new.worktree_root,
            new.server,
            new.tool,
            i64::from(new.read_only),
            new.process,
            new.incarnation,
            now,
        ],
    )?;
    if new.read_only {
        return Ok(None);
    }
    let generation = bump(conn, &new.worktree_root)?;
    conn.execute(
        "UPDATE mcp_app_calls SET generation = ?2 WHERE call_id = ?1",
        params![new.call_id, generation],
    )?;
    Ok(Some(generation))
}

fn advance(conn: &Connection, call_id: &str, root: &str) -> Result<i64> {
    let pending: bool = conn
        .query_row(
            "SELECT advance_pending = 1 FROM mcp_app_calls WHERE call_id = ?1",
            [call_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false);
    if !pending {
        return shell_runs::generation(conn, root);
    }
    let generation = bump(conn, root)?;
    conn.execute(
        "UPDATE mcp_app_calls SET advance_pending = 0 WHERE call_id = ?1",
        [call_id],
    )?;
    Ok(generation)
}

fn barrier(conn: &Connection, root: &str, app_turn: &str) -> Result<Option<AppCallBarrier>> {
    let running: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM mcp_app_calls WHERE worktree_root = ?1
           AND (state IN ('running', 'uncertain') OR advance_pending = 1))",
        [root],
        |r| r.get(0),
    )?;
    if running {
        return Ok(Some(AppCallBarrier::Running));
    }
    let after: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM mcp_app_calls WHERE worktree_root = ?1
           AND generation > COALESCE(
             (SELECT generation FROM turn_checkpoints WHERE app_turn = ?2), 0))",
        params![root, app_turn],
        |r| r.get(0),
    )?;
    Ok(after.then_some(AppCallBarrier::RanAfterTurn))
}

fn release(conn: &Connection, session: &SessionId, call_id: &str, now: i64) -> Result<Release> {
    let state: Option<String> = conn
        .query_row(
            "SELECT state FROM mcp_app_calls WHERE call_id = ?1 AND session_id = ?2",
            params![call_id, session.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(state) = state else {
        return Ok(Release::Unknown);
    };
    match CallState::parse(&state)? {
        CallState::Running => Ok(Release::Running),
        CallState::Uncertain => {
            let n = conn.execute(
                "UPDATE mcp_app_calls SET state = 'released', ended_at = ?3,
                     advance_pending = 1 - read_only
                 WHERE call_id = ?1 AND session_id = ?2 AND state = 'uncertain'",
                params![call_id, session.as_str(), now],
            )?;
            Ok(if n == 1 {
                Release::Released
            } else {
                Release::NotOpen
            })
        }
        _ => Ok(Release::NotOpen),
    }
}

impl Store {
    /// The `(running, uncertain)` rows of a session.
    ///
    /// # Errors
    /// A database error.
    pub async fn app_call_open_count(&self, session: SessionId) -> Result<(u32, u32)> {
        call(&self.inner.db, move |conn| {
            let count = |state: &str| -> Result<u32> {
                let n: i64 = conn.query_row(
                    "SELECT count(*) FROM mcp_app_calls WHERE session_id = ?1 AND state = ?2",
                    params![session.as_str(), state],
                    |r| r.get(0),
                )?;
                u32::try_from(n).map_err(|_| StoreError::Corrupt("app call count".to_owned()))
            };
            Ok((count("running")?, count("uncertain")?))
        })
        .await
    }

    /// Inserts a row in `running`. A call that is not read-only also advances
    /// the folder generation of its root in the same transaction and stores
    /// the new value in `generation`. Returns that generation, or `None` for
    /// a read-only call.
    ///
    /// # Errors
    /// A database error (a call id that exists).
    pub async fn app_call_begin(&self, new: NewAppCall) -> Result<Option<i64>> {
        let (generation, _) = self
            .write(move |conn, now, _| begin(conn, &new, now))
            .await?;
        Ok(generation)
    }

    /// Moves a row in `running` or `uncertain` to `end`, sets `ended_at` and
    /// `advance_pending = 1 - read_only`, in one guarded statement. `false`
    /// when the row was terminal or does not exist.
    ///
    /// # Errors
    /// A database error.
    pub async fn app_call_settle(&self, call_id: String, end: AppCallEnd) -> Result<bool> {
        let (moved, _) = self
            .write(move |conn, now, _| {
                let n = conn.execute(
                    "UPDATE mcp_app_calls SET state = ?2, ended_at = ?3,
                         advance_pending = 1 - read_only
                     WHERE call_id = ?1 AND state IN ('running', 'uncertain')",
                    params![call_id, end.as_str(), now],
                )?;
                Ok(n == 1)
            })
            .await?;
        Ok(moved)
    }

    /// When the advance of the row is pending, advances the generation of
    /// `root` and clears the marker, in one transaction. Returns the
    /// generation of `root`. The caller holds the fence of `root`.
    ///
    /// # Errors
    /// A database error.
    pub async fn app_call_advance(&self, call_id: String, root: String) -> Result<i64> {
        let (generation, _) = self
            .write(move |conn, _, _| advance(conn, &call_id, &root))
            .await?;
        Ok(generation)
    }

    /// `(call_id, worktree_root)` of the rows with `advance_pending = 1`.
    ///
    /// # Errors
    /// A database error.
    pub async fn app_call_pending_advances(&self) -> Result<Vec<(String, String)>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT call_id, worktree_root FROM mcp_app_calls WHERE advance_pending = 1
                 ORDER BY ended_at, call_id",
            )?;
            Ok(stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// The rows in `running` whose `incarnation` is not this store's.
    ///
    /// # Errors
    /// A database error.
    pub async fn app_calls_of_older_incarnations(&self) -> Result<Vec<AppCallRow>> {
        let current = self.inner.incarnation.as_str().to_owned();
        call(&self.inner.db, move |conn| {
            select(
                conn,
                "state = 'running' AND incarnation <> ?1",
                &[&current],
                None,
            )
        })
        .await
    }

    /// Moves a row from `running` to `uncertain` with `cause`, in one guarded
    /// statement. `false` when the row was in another state.
    ///
    /// # Errors
    /// A database error.
    pub async fn app_call_mark_uncertain(
        &self,
        call_id: String,
        cause: UncertainCause,
    ) -> Result<bool> {
        let (moved, _) = self
            .write(move |conn, _, _| {
                let n = conn.execute(
                    "UPDATE mcp_app_calls SET state = 'uncertain', cause = ?2
                     WHERE call_id = ?1 AND state = 'running'",
                    params![call_id, cause_str(&cause)],
                )?;
                Ok(n == 1)
            })
            .await?;
        Ok(moved)
    }

    /// The rows in `uncertain`, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn app_calls_uncertain(&self) -> Result<Vec<AppCallRow>> {
        call(&self.inner.db, move |conn| {
            select(conn, "state = 'uncertain'", &[], None)
        })
        .await
    }

    /// The `uncertain` rows of `session` whose `process` is `process`.
    ///
    /// # Errors
    /// A database error.
    pub async fn app_calls_uncertain_of_process(
        &self,
        session: SessionId,
        process: String,
    ) -> Result<Vec<AppCallRow>> {
        call(&self.inner.db, move |conn| {
            select(
                conn,
                "state = 'uncertain' AND session_id = ?1 AND process = ?2",
                &[&session.as_str(), &process],
                None,
            )
        })
        .await
    }

    /// The rows in `running` or `uncertain` (oldest first, at most
    /// [`OPEN_CALLS_LIMIT`]) whose session is `session`, or whose
    /// `worktree_root` is the root of an open row of `session`, the root of
    /// the newest `reverts` row of `session`, or `extra_root`.
    ///
    /// # Errors
    /// A database error.
    pub async fn app_calls_open(
        &self,
        session: SessionId,
        extra_root: Option<String>,
    ) -> Result<Vec<AppCallRow>> {
        call(&self.inner.db, move |conn| {
            select(
                conn,
                "state IN ('running', 'uncertain') AND (session_id = ?1 OR worktree_root IN (
                   SELECT worktree_root FROM mcp_app_calls
                     WHERE session_id = ?1 AND state IN ('running', 'uncertain')
                   UNION
                   SELECT worktree_root FROM (SELECT worktree_root FROM reverts
                     WHERE session_id = ?1 ORDER BY created_at DESC, revert_id DESC LIMIT 1)
                   UNION
                   SELECT ?2 WHERE ?2 IS NOT NULL))",
                &[&session.as_str(), &extra_root],
                Some(OPEN_CALLS_LIMIT),
            )
        })
        .await
    }

    /// The app part of the revert barrier for the turn `app_turn` in `root`:
    /// `Running` when a row of `root` is unsettled; else `RanAfterTurn` when a
    /// row of `root` has a `generation` above the one of the turn row (NULL
    /// counts as 0); else `None`.
    ///
    /// # Errors
    /// A database error.
    pub async fn app_call_barrier(
        &self,
        root: String,
        app_turn: String,
    ) -> Result<Option<AppCallBarrier>> {
        call(&self.inner.db, move |conn| barrier(conn, &root, &app_turn)).await
    }

    /// The owner's release of an `uncertain` call. It reads only
    /// `mcp_app_calls`: neither the `mcp_apps` row nor a live session is
    /// needed.
    ///
    /// # Errors
    /// A database error.
    pub async fn app_call_release(&self, session: SessionId, call_id: String) -> Result<Release> {
        let (answer, _) = self
            .write(move |conn, now, _| release(conn, &session, &call_id, now))
            .await?;
        Ok(answer)
    }

    /// One row, by call id (tests and the station wiring).
    ///
    /// # Errors
    /// A database error.
    pub async fn app_call(&self, call_id: String) -> Result<Option<AppCallRow>> {
        call(&self.inner.db, move |conn| {
            Ok(select(conn, "call_id = ?1", &[&call_id], None)?.pop())
        })
        .await
    }
}

#[cfg(test)]
#[path = "app_calls_tests.rs"]
mod tests;
