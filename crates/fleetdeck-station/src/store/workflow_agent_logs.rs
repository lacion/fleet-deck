//! The binding of a workflow agent to its transcript (B12, design
//! `docs/v2/design/workflow-agents.md` §4.4 Binding, table
//! `workflow_agent_logs`).
//!
//! [`bind`] runs in the append transaction of a `workflow.updated`, between
//! the projection step and R6 (see `workflow_state`). It reads only
//! station-owned rows and the run id and agent ids of the event, each checked
//! by pattern. `session.workflow_agent` reads only the rows that it writes.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::check_native_session_id;
use fleetdeck_proto::event::WorkflowUpdate;
use rusqlite::{Connection, OptionalExtension, params};

use super::{Result, Store, call};

/// One row of `workflow_agent_logs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowAgentLogRow {
    pub session_id: SessionId,
    pub agent_id: String,
    pub run_id: String,
    pub native_id: String,
    pub cwd: String,
    pub provider_home: String,
    pub created_at: i64,
}

/// True for a workflow run id: `^wf_[a-z0-9-]{1,64}$`.
#[must_use]
pub fn run_id_ok(id: &str) -> bool {
    id.strip_prefix("wf_").is_some_and(|rest| {
        (1..=64).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// True for an agent id of the Claude stream: `[a-z0-9]{1,64}`.
#[must_use]
pub fn agent_id_ok(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// The launch facts that the binding stores: the home and the working
/// directory of the session's newest process, and the session's native id.
struct Facts {
    native_id: String,
    cwd: String,
    provider_home: String,
}

fn facts_of(conn: &Connection, sid: &str) -> Result<Option<Facts>> {
    let process: Option<String> = conn
        .query_row(
            "SELECT process_incarnation FROM processes WHERE session_id = ?1
             ORDER BY started_at DESC, rowid DESC LIMIT 1",
            [sid],
            |r| r.get(0),
        )
        .optional()?;
    let Some(process) = process else {
        tracing::debug!(session = sid, "workflow binding: no process row");
        return Ok(None);
    };
    let launch: Option<(String, String)> = conn
        .query_row(
            "SELECT provider_home, cwd FROM process_launch_facts
             WHERE process_incarnation = ?1 AND session_id = ?2",
            params![process, sid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((provider_home, cwd)) = launch else {
        tracing::debug!(session = sid, "workflow binding: no launch facts");
        return Ok(None);
    };
    if !std::path::Path::new(&provider_home).is_absolute() {
        return Ok(None);
    }
    let native: Option<String> = conn
        .query_row(
            "SELECT native_session_id FROM sessions WHERE session_id = ?1",
            [sid],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(native_id) = native.filter(|n| check_native_session_id(n).is_ok()) else {
        tracing::debug!(session = sid, "workflow binding: no valid native id");
        return Ok(None);
    };
    Ok(Some(Facts {
        native_id,
        cwd,
        provider_home,
    }))
}

/// Binds the agents of one `workflow.updated` (called after R1 to R5).
///
/// Only a run whose run id (after R1) is valid binds. When this event set the
/// run id (`run_id_was` is `None` or another id), every agent of the run in
/// `workflow_agents` with an agent id binds; else only the agents of the
/// event do. No valid run id, process row, facts row, native id or agent id:
/// no row, and no error.
///
/// # Errors
/// A database error.
pub(super) fn bind(
    conn: &Connection,
    session: &SessionId,
    update: &WorkflowUpdate,
    run_id_was: Option<&str>,
    now: i64,
) -> Result<()> {
    let sid = session.as_str();
    let run_id: Option<String> = conn
        .query_row(
            "SELECT run_id FROM workflow_runs WHERE session_id = ?1 AND task_id = ?2",
            params![sid, update.task_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(run_id) = run_id.filter(|id| run_id_ok(id)) else {
        return Ok(());
    };
    let agents: Vec<String> = if run_id_was == Some(run_id.as_str()) {
        update
            .agents
            .iter()
            .filter_map(|a| a.agent_id.clone())
            .collect()
    } else {
        let mut stmt = conn.prepare_cached(
            "SELECT agent_id FROM workflow_agents
             WHERE session_id = ?1 AND task_id = ?2 AND agent_id IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![sid, update.task_id], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let agents: Vec<String> = agents.into_iter().filter(|a| agent_id_ok(a)).collect();
    if agents.is_empty() {
        return Ok(());
    }
    let Some(facts) = facts_of(conn, sid)? else {
        return Ok(());
    };
    let mut insert = conn.prepare_cached(
        "INSERT OR IGNORE INTO workflow_agent_logs
           (session_id, agent_id, run_id, native_id, cwd, provider_home, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for agent_id in agents {
        insert.execute(params![
            sid,
            agent_id,
            run_id,
            facts.native_id,
            facts.cwd,
            facts.provider_home,
            now,
        ])?;
    }
    Ok(())
}

impl Store {
    /// The binding of a workflow agent of a session.
    ///
    /// # Errors
    /// A database error.
    pub async fn workflow_agent_log(
        &self,
        session_id: SessionId,
        agent_id: String,
    ) -> Result<Option<WorkflowAgentLogRow>> {
        call(&self.inner.db, move |conn| {
            let row = conn
                .query_row(
                    "SELECT run_id, native_id, cwd, provider_home, created_at
                     FROM workflow_agent_logs WHERE session_id = ?1 AND agent_id = ?2",
                    params![session_id.as_str(), agent_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()?;
            Ok(row.map(
                |(run_id, native_id, cwd, provider_home, created_at)| WorkflowAgentLogRow {
                    session_id,
                    agent_id,
                    run_id,
                    native_id,
                    cwd,
                    provider_home,
                    created_at,
                },
            ))
        })
        .await
    }
}

#[cfg(test)]
#[path = "tests_workflow_bind.rs"]
mod tests;
