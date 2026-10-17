//! The land runs of the station store (FG-8, design §11.5, §3.5 V30): one row
//! for each `session.land` that the station admitted, with its journal
//! (phase, the `pending_*` operands of an outstanding remote write) and its
//! results. This file has the row, the insert of the decision, and the reads.
//! The engine (T3.2b, T3.2c) writes each phase change in its own transaction,
//! with the event, and uses [`guard_running`] for the fence rule of §11.5.

use fleetdeck_proto::fleet_git::{LandInfo, LandPhase, LandPlan, LandState};
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::{Event, EventEnvelope, SessionId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{NewEvent, Result, Store, StoreError, call, log, session_holds};

/// `pending_action` of a branch push.
pub const PENDING_BRANCH_PUSH: &str = "branch_push";
/// `pending_action` of a pull request creation.
pub const PENDING_PR_CREATE: &str = "pr_create";
/// `pending_action` of the push to the default branch.
pub const PENDING_DEFAULT_PUSH: &str = "default_push";

/// The text of a unit enum value (`LandState::Running` is `running`).
pub fn text_of<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// A unit enum value from its text; an unknown text is `fallback`.
pub fn from_text<T: serde::de::DeserializeOwned>(text: &str, fallback: T) -> T {
    serde_json::from_value(serde_json::Value::String(text.to_owned())).unwrap_or(fallback)
}

/// One row of `land_runs`.
#[derive(Debug, Clone, PartialEq)]
pub struct LandRunRow {
    pub land_id: String,
    pub command_id: String,
    pub session_id: fleetdeck_proto::SessionId,
    pub plan: LandPlan,
    pub attempt: u32,
    pub phase: LandPhase,
    pub state: LandState,
    pub default_ref: Option<String>,
    pub source_oid: Option<String>,
    pub base_oid: Option<String>,
    pub rebased_oid: Option<String>,
    pub branch_lease: Option<String>,
    pub branch_published_oid: Option<String>,
    pub default_published_oid: Option<String>,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub files: Vec<String>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub authority: Option<CommandAuthority>,
    pub pending_action: Option<String>,
    pub pending_attempt: Option<u32>,
    pub pending_url: Option<String>,
    pub pending_ref: Option<String>,
    pub pending_oid: Option<String>,
    pub pending_lease: Option<String>,
    pub pending_expect: Option<String>,
    pub reconciling: bool,
    pub reconcile_attempts: u32,
    pub next_reconcile_at: Option<i64>,
    pub last_write_outcome: Option<String>,
    pub abandon_write: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub ended_at: Option<i64>,
}

/// The values of a new run (design §11.4 step 2).
#[derive(Debug, Clone, PartialEq)]
pub struct NewLandRun {
    pub land_id: String,
    pub command_id: String,
    pub session_id: fleetdeck_proto::SessionId,
    pub plan: LandPlan,
    /// The authority of the envelope (a coordinator land); none for the owner.
    pub authority: Option<CommandAuthority>,
}

const COLUMNS: &str = "land_id, command_id, session_id, plan, attempt, phase, state, default_ref,
     source_oid, base_oid, rebased_oid, branch_lease, branch_published_oid,
     default_published_oid, pr_number, pr_url, files, code, message, authority,
     pending_action, pending_attempt, pending_url, pending_ref, pending_oid, pending_lease,
     pending_expect, reconciling, reconcile_attempts, next_reconcile_at, last_write_outcome,
     abandon_write, created_at, updated_at, ended_at";

fn json_error(column: usize, e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e))
}

fn row_of(r: &Row<'_>) -> rusqlite::Result<LandRunRow> {
    let plan: String = r.get(3)?;
    let files: String = r.get(16)?;
    let authority: Option<String> = r.get(19)?;
    let phase: String = r.get(5)?;
    let state: String = r.get(6)?;
    Ok(LandRunRow {
        land_id: r.get(0)?,
        command_id: r.get(1)?,
        session_id: fleetdeck_proto::SessionId::from(r.get::<_, String>(2)?),
        plan: serde_json::from_str(&plan).map_err(|e| json_error(3, e))?,
        attempt: r.get(4)?,
        phase: from_text(&phase, LandPhase::Unknown),
        state: from_text(&state, LandState::Unknown),
        default_ref: r.get(7)?,
        source_oid: r.get(8)?,
        base_oid: r.get(9)?,
        rebased_oid: r.get(10)?,
        branch_lease: r.get(11)?,
        branch_published_oid: r.get(12)?,
        default_published_oid: r.get(13)?,
        pr_number: r
            .get::<_, Option<i64>>(14)?
            .and_then(|v| u64::try_from(v).ok()),
        pr_url: r.get(15)?,
        files: serde_json::from_str(&files).map_err(|e| json_error(16, e))?,
        code: r.get(17)?,
        message: r.get(18)?,
        authority: authority
            .map(|a| serde_json::from_str(&a).map_err(|e| json_error(19, e)))
            .transpose()?,
        pending_action: r.get(20)?,
        pending_attempt: r.get(21)?,
        pending_url: r.get(22)?,
        pending_ref: r.get(23)?,
        pending_oid: r.get(24)?,
        pending_lease: r.get(25)?,
        pending_expect: r.get(26)?,
        reconciling: r.get::<_, i64>(27)? != 0,
        reconcile_attempts: r.get(28)?,
        next_reconcile_at: r.get(29)?,
        last_write_outcome: r.get(30)?,
        abandon_write: r.get(31)?,
        created_at: r.get(32)?,
        updated_at: r.get(33)?,
        ended_at: r.get(34)?,
    })
}

fn json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| StoreError::Corrupt(format!("land JSON: {e}")))
}

/// Inserts a run in phase `admitted`, state `running`, attempt 1 (design
/// §11.4 step 2). The caller inserts the session hold in the same
/// transaction.
///
/// # Errors
/// A database error (a constraint error for a land id or command id that is
/// stored already).
pub fn insert(conn: &Connection, new: &NewLandRun, now: i64) -> Result<()> {
    let authority = new.authority.as_ref().map(json).transpose()?;
    conn.execute(
        "INSERT INTO land_runs
           (land_id, command_id, session_id, plan, attempt, phase, state, authority,
            created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 1, 'admitted', 'running', ?5, ?6, ?6)",
        params![
            new.land_id,
            new.command_id,
            new.session_id.as_str(),
            json(&new.plan)?,
            authority,
            now,
        ],
    )?;
    Ok(())
}

/// The run of `land_id`.
///
/// # Errors
/// A database error.
pub fn get(conn: &Connection, land_id: &str) -> Result<Option<LandRunRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM land_runs WHERE land_id = ?1"),
            params![land_id],
            row_of,
        )
        .optional()?)
}

/// The run that the command `command_id` made.
///
/// # Errors
/// A database error.
pub fn by_command(conn: &Connection, command_id: &str) -> Result<Option<LandRunRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM land_runs WHERE command_id = ?1"),
            params![command_id],
            row_of,
        )
        .optional()?)
}

/// Every run in state `running`, oldest first.
///
/// # Errors
/// A database error.
pub fn running(conn: &Connection) -> Result<Vec<LandRunRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM land_runs WHERE state = 'running' ORDER BY created_at, land_id"
    ))?;
    let rows = stmt.query_map([], row_of)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The fence rule of design §11.5: the run is `running` and its land id has
/// no fence. A transaction that records the result of a read, or that stores
/// an intent, runs only when this is true (inside the same transaction).
///
/// # Errors
/// A database error.
pub fn guard_running(conn: &Connection, land_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM land_runs
                         WHERE land_id = ?1 AND state = 'running'
                           AND NOT EXISTS (SELECT 1 FROM land_fences WHERE land_id = ?1))",
        params![land_id],
        |r| r.get(0),
    )?)
}

/// The `LandInfo` of the `session.land` event of a run (design §3.6). The
/// text is the end text of a terminal run; `files` is set for a conflict.
#[must_use]
pub fn info_of(row: &LandRunRow) -> LandInfo {
    LandInfo {
        land_id: row.land_id.clone(),
        attempt: row.attempt,
        state: row.state,
        phase: row.phase,
        reconciling: row.reconciling,
        code: row.code.clone(),
        text: row.message.clone(),
        files: row.files.clone(),
        source_oid: row.source_oid.clone(),
        base_oid: row.base_oid.clone(),
        rebased_oid: row.rebased_oid.clone(),
        branch_published_oid: row.branch_published_oid.clone(),
        default_published_oid: row.default_published_oid.clone(),
        pr_url: row.pr_url.clone(),
    }
}

/// The end of a run (design §11.5 step 8): a terminal state, its code and its
/// text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct End {
    pub state: LandState,
    pub code: Option<String>,
    pub text: String,
}

/// One phase change or end of a run: the values that are set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    pub phase: Option<LandPhase>,
    pub default_ref: Option<String>,
    pub source_oid: Option<String>,
    pub base_oid: Option<String>,
    pub rebased_oid: Option<String>,
    pub branch_lease: Option<String>,
    pub files: Option<Vec<String>>,
    pub end: Option<End>,
}

/// One store transaction of a run (design §11.5, journal rule): it requires
/// the fence rule of [`guard_running`] (zero rows: the run is fenced or
/// ended, nothing is written), sets the values of `change`, and appends one
/// `session.land` event. An end also needs `pending_action = NULL`; it sets
/// the phase `done` and `ended_at`, and deletes the session hold (design
/// §11.5 step 8, one transaction). Returns the event, or `None` when nothing
/// changed.
///
/// # Errors
/// A database error.
pub fn advance(
    conn: &Connection,
    land_id: &str,
    change: &Change,
    now: i64,
) -> Result<Option<EventEnvelope>> {
    let files = change.files.as_ref().map(json).transpose()?;
    let (phase, state, code, message, ended) = match &change.end {
        Some(end) => (
            Some(text_of(&LandPhase::Done)),
            Some(text_of(&end.state)),
            end.code.clone(),
            Some(end.text.clone()),
            true,
        ),
        None => (change.phase.as_ref().map(text_of), None, None, None, false),
    };
    let changed = conn.execute(
        "UPDATE land_runs SET
            phase = COALESCE(?2, phase), state = COALESCE(?3, state),
            default_ref = COALESCE(?4, default_ref), source_oid = COALESCE(?5, source_oid),
            base_oid = COALESCE(?6, base_oid), rebased_oid = COALESCE(?7, rebased_oid),
            branch_lease = COALESCE(?8, branch_lease), files = COALESCE(?9, files),
            code = COALESCE(?10, code), message = COALESCE(?11, message),
            ended_at = CASE WHEN ?12 THEN ?13 ELSE ended_at END, updated_at = ?13
         WHERE land_id = ?1 AND state = 'running'
           AND NOT EXISTS (SELECT 1 FROM land_fences WHERE land_id = ?1)
           AND (NOT ?12 OR pending_action IS NULL)",
        params![
            land_id,
            phase,
            state,
            change.default_ref,
            change.source_oid,
            change.base_oid,
            change.rebased_oid,
            change.branch_lease,
            files,
            code,
            message,
            ended,
            now,
        ],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    let Some(row) = get(conn, land_id)? else {
        return Ok(None);
    };
    if ended {
        session_holds::delete(conn, &row.session_id, land_id)?;
    }
    let event = NewEvent::from(Event::SessionLand(Box::new(info_of(&row))));
    Ok(Some(log::append_one(conn, &row.session_id, event, now)?))
}

/// What the land decision reads of a session's Fleet Git rows (design §11.1,
/// §11.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFacts {
    pub project_id: String,
    pub kind: String,
    pub slug: Option<String>,
    pub branch: Option<String>,
    pub publish_ref: Option<String>,
    pub created_branch: bool,
    /// `git_pushes.target_dest`, when the session has a push row.
    pub push_dest: Option<String>,
}

/// The Fleet Git facts of a session, or `None` without a `session_git` row.
///
/// # Errors
/// A database error.
pub fn session_facts(conn: &Connection, session_id: &SessionId) -> Result<Option<SessionFacts>> {
    Ok(conn
        .query_row(
            "SELECT g.project_id, g.kind, g.slug, g.branch, g.publish_ref, g.created_branch,
                    (SELECT p.target_dest FROM git_pushes p WHERE p.session_id = g.session_id)
             FROM session_git g WHERE g.session_id = ?1",
            params![session_id.as_str()],
            |r| {
                Ok(SessionFacts {
                    project_id: r.get(0)?,
                    kind: r.get(1)?,
                    slug: r.get(2)?,
                    branch: r.get(3)?,
                    publish_ref: r.get(4)?,
                    created_branch: r.get::<_, i64>(5)? != 0,
                    push_dest: r.get(6)?,
                })
            },
        )
        .optional()?)
}

impl Store {
    /// One phase change or end of a run in one transaction, with its
    /// `session.land` event. `false`: the run is fenced or ended, and nothing
    /// was written.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_advance(&self, land_id: String, change: Change) -> Result<bool> {
        let now = self.now();
        let event = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let event = advance(&tx, &land_id, &change, now)?;
            tx.commit()?;
            Ok(event)
        })
        .await?;
        if let Some(event) = &event {
            self.publish(event.station_seq);
        }
        Ok(event.is_some())
    }

    /// The `git_pushes.last_pushed_oid` of a session (the branch lease of
    /// attempt 1).
    ///
    /// # Errors
    /// A database error.
    pub async fn land_last_pushed(&self, session_id: SessionId) -> Result<Option<String>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT last_pushed_oid FROM git_pushes WHERE session_id = ?1",
                    params![session_id.as_str()],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
        })
        .await
    }

    /// The run of `land_id`.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_run(&self, land_id: String) -> Result<Option<LandRunRow>> {
        call(&self.inner.db, move |conn| get(conn, &land_id)).await
    }

    /// The run that the command `command_id` made.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_run_by_command(&self, command_id: String) -> Result<Option<LandRunRow>> {
        call(&self.inner.db, move |conn| by_command(conn, &command_id)).await
    }

    /// Every run in state `running`, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_runs_running(&self) -> Result<Vec<LandRunRow>> {
        call(&self.inner.db, move |conn| running(conn)).await
    }
}

#[cfg(test)]
#[path = "land_runs_tests.rs"]
mod tests;
