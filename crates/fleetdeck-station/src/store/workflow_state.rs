//! The workflow projection of the station (B12, design
//! `docs/v2/design/workflow-agents.md` §4.3a and §4.4).
//!
//! [`observe`] runs in the append transaction of every event, after
//! `shell_observe::observe`. It applies the view rules R1 to R6 to the tables
//! `workflow_runs`, `workflow_phases` and `workflow_agents`. The order inside
//! one call is fixed: (1) R1 to R5; (2) for a `workflow.updated`, the binding
//! of its agents ([`super::workflow_agent_logs::bind`]); (3) R6. So R6 removes
//! a run only after its agents are bound. It fails only on a database error.

use fleetdeck_proto::event::{BackgroundTask, Event, SessionState, WorkflowAgent, WorkflowUpdate};
use fleetdeck_proto::{EventBody, EventEnvelope, SessionId};
use rusqlite::{Connection, OptionalExtension, params};

use super::Result;

/// The most phases of one run (R1).
const MAX_PHASES: i64 = 100;
/// The most agents of one run (R1).
const MAX_AGENTS: i64 = 1_000;
/// The most runs of one session with a status that is not `running` (R6).
const MAX_ENDED_RUNS: i64 = 32;

/// Applies the view rules to the tables for one event.
pub(super) fn observe(conn: &Connection, envelope: &EventEnvelope, now: i64) -> Result<()> {
    let EventBody::Known(event) = &envelope.event else {
        return Ok(());
    };
    let session = &envelope.session_id;
    match event {
        Event::WorkflowUpdated(update) => {
            let run_id_was = apply_update(conn, session, update, envelope.station_seq)?;
            super::workflow_agent_logs::bind(conn, session, update, run_id_was.as_deref(), now)?;
            prune(conn, session)
        }
        Event::BackgroundUpdated { tasks } => {
            stop_unlisted(conn, session, tasks)?;
            prune(conn, session)
        }
        Event::SessionEnded { .. } => {
            stop_all(conn, session)?;
            prune(conn, session)
        }
        Event::SessionStateChanged {
            state: SessionState::Lost | SessionState::Exited,
            ..
        } => {
            stop_all(conn, session)?;
            prune(conn, session)
        }
        _ => Ok(()),
    }
}

/// R1, R2 and R5 for one `workflow.updated`. Returns the run id that the run
/// had before the event.
fn apply_update(
    conn: &Connection,
    session: &SessionId,
    update: &WorkflowUpdate,
    station_seq: u64,
) -> Result<Option<String>> {
    let sid = session.as_str();
    let existing: Option<Option<String>> = conn
        .query_row(
            "SELECT run_id FROM workflow_runs WHERE session_id = ?1 AND task_id = ?2",
            params![sid, update.task_id],
            |r| r.get(0),
        )
        .optional()?;
    let untracked = i64::from(update.untracked);
    let running = i64::from(update.running_agents);
    let total = i64::from(update.agents_total);
    let run_id_was = match existing {
        None => {
            conn.execute(
                "INSERT INTO workflow_runs
                   (session_id, task_id, run_order, node_id, name, run_id, status,
                    untracked, running_agents, agents_total)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    sid,
                    update.task_id,
                    i64::try_from(station_seq).unwrap_or(i64::MAX),
                    update.node_id,
                    update.name,
                    update.run_id,
                    update.status,
                    untracked,
                    running,
                    total,
                ],
            )?;
            None
        }
        Some(was) => {
            conn.execute(
                "UPDATE workflow_runs SET
                     node_id = COALESCE(?3, node_id),
                     name = COALESCE(?4, name),
                     run_id = COALESCE(?5, run_id),
                     status = ?6, untracked = ?7, running_agents = ?8, agents_total = ?9
                 WHERE session_id = ?1 AND task_id = ?2",
                params![
                    sid,
                    update.task_id,
                    update.node_id,
                    update.name,
                    update.run_id,
                    update.status,
                    untracked,
                    running,
                    total,
                ],
            )?;
            was
        }
    };
    upsert_phases(conn, sid, update)?;
    upsert_agents(conn, sid, update)?;
    if update.status != "running" {
        stop_agents(conn, sid, &update.task_id)?;
    }
    Ok(run_id_was)
}

fn upsert_phases(conn: &Connection, sid: &str, update: &WorkflowUpdate) -> Result<()> {
    if update.phases.is_empty() {
        return Ok(());
    }
    let mut count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM workflow_phases WHERE session_id = ?1 AND task_id = ?2",
        params![sid, update.task_id],
        |r| r.get(0),
    )?;
    let mut change = conn.prepare_cached(
        "UPDATE workflow_phases SET title = ?4
         WHERE session_id = ?1 AND task_id = ?2 AND idx = ?3",
    )?;
    let mut add = conn.prepare_cached(
        "INSERT INTO workflow_phases (session_id, task_id, idx, title) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for phase in &update.phases {
        let idx = i64::from(phase.index);
        let changed = change.execute(params![sid, update.task_id, idx, phase.title])?;
        // A new index beyond the cap is ignored.
        if changed == 0 && count < MAX_PHASES {
            add.execute(params![sid, update.task_id, idx, phase.title])?;
            count += 1;
        }
    }
    Ok(())
}

fn upsert_agents(conn: &Connection, sid: &str, update: &WorkflowUpdate) -> Result<()> {
    if update.agents.is_empty() {
        return Ok(());
    }
    let mut count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM workflow_agents WHERE session_id = ?1 AND task_id = ?2",
        params![sid, update.task_id],
        |r| r.get(0),
    )?;
    let mut change = conn.prepare_cached(
        "UPDATE workflow_agents SET agent_id = ?4, state = ?5, entry = ?6
         WHERE session_id = ?1 AND task_id = ?2 AND idx = ?3",
    )?;
    let mut add = conn.prepare_cached(
        "INSERT INTO workflow_agents (session_id, task_id, idx, agent_id, state, entry)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for agent in &update.agents {
        let idx = i64::from(agent.index);
        let entry = agent_json(agent)?;
        let values = params![sid, update.task_id, idx, agent.agent_id, agent.state, entry];
        let changed = change.execute(values)?;
        // A new index beyond the cap is ignored.
        if changed == 0 && count < MAX_AGENTS {
            add.execute(values)?;
            count += 1;
        }
    }
    Ok(())
}

fn agent_json(agent: &WorkflowAgent) -> Result<String> {
    Ok(serde_json::to_string(agent)?)
}

/// R2 for the agents of one run: every agent in `queued` or `running`
/// becomes `stopped`, also in its `entry`. The run has no running agent.
fn stop_agents(conn: &Connection, sid: &str, task_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE workflow_runs SET running_agents = 0 WHERE session_id = ?1 AND task_id = ?2",
        params![sid, task_id],
    )?;
    conn.execute(
        "UPDATE workflow_agents SET state = 'stopped', entry = json_set(entry, '$.state', 'stopped')
         WHERE session_id = ?1 AND task_id = ?2 AND state IN ('queued', 'running')
           AND json_valid(entry)",
        params![sid, task_id],
    )?;
    // An entry that does not parse keeps its text: the next entry of its
    // index replaces it.
    conn.execute(
        "UPDATE workflow_agents SET state = 'stopped'
         WHERE session_id = ?1 AND task_id = ?2 AND state IN ('queued', 'running')",
        params![sid, task_id],
    )?;
    Ok(())
}

/// The task ids of the runs of a session with status `running`.
fn running_runs(conn: &Connection, sid: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT task_id FROM workflow_runs WHERE session_id = ?1 AND status = 'running'",
    )?;
    let rows = stmt.query_map([sid], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A run becomes `stopped` (R3, R4): R2 applies.
fn stop_run(conn: &Connection, sid: &str, task_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE workflow_runs SET status = 'stopped' WHERE session_id = ?1 AND task_id = ?2",
        params![sid, task_id],
    )?;
    stop_agents(conn, sid, task_id)
}

/// R3: every running run whose task id the list does not name stops.
fn stop_unlisted(conn: &Connection, session: &SessionId, tasks: &[BackgroundTask]) -> Result<()> {
    let sid = session.as_str();
    for task_id in running_runs(conn, sid)? {
        if !tasks.iter().any(|t| t.task_id == task_id) {
            stop_run(conn, sid, &task_id)?;
        }
    }
    Ok(())
}

/// R4: every running run stops.
fn stop_all(conn: &Connection, session: &SessionId) -> Result<()> {
    let sid = session.as_str();
    for task_id in running_runs(conn, sid)? {
        stop_run(conn, sid, &task_id)?;
    }
    Ok(())
}

/// R6: a session keeps at most 32 runs with a status that is not `running`;
/// the runs with the smallest `run_order` go, with their phases and agents.
/// A running run never goes. The binding rows are not touched.
fn prune(conn: &Connection, session: &SessionId) -> Result<()> {
    let sid = session.as_str();
    let ended: i64 = conn.query_row(
        "SELECT COUNT(*) FROM workflow_runs WHERE session_id = ?1 AND status <> 'running'",
        [sid],
        |r| r.get(0),
    )?;
    if ended <= MAX_ENDED_RUNS {
        return Ok(());
    }
    let mut stmt = conn.prepare_cached(
        "SELECT task_id FROM workflow_runs WHERE session_id = ?1 AND status <> 'running'
         ORDER BY run_order LIMIT ?2",
    )?;
    let gone = stmt
        .query_map(params![sid, ended - MAX_ENDED_RUNS], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for task_id in gone {
        for table in ["workflow_agents", "workflow_phases", "workflow_runs"] {
            conn.execute(
                &format!("DELETE FROM {table} WHERE session_id = ?1 AND task_id = ?2"),
                params![sid, task_id],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests_workflow_state.rs"]
mod tests;
