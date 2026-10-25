//! The answer of `session.workflows` (B12, design
//! `docs/v2/design/workflow-agents.md` §4.4): the workflow projection of a
//! session in pages of at most `WORKFLOWS_PAGE_BUDGET` serialized bytes.
//!
//! One read transaction gives `as_of` and the page. A run header (the fields
//! of the run, and its phases where the run starts on the page) always goes
//! on a page together with the first agent of the run on that page; a page
//! always holds at least one item. The cursor compares the `idx` column.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::{
    WORKFLOWS_PAGE_BUDGET, WorkflowAgent, WorkflowPhase, WorkflowRun, WorkflowUpdate,
    WorkflowsCursor, WorkflowsPage,
};
use rusqlite::{Connection, OptionalExtension, Rows, params};

use super::{Result, Store, StoreError, call, to_u64};

/// The room of the page envelope (`as_of`, `next`, the array brackets).
const ENVELOPE_BYTES: usize = 256;

/// One row of `workflow_runs`.
struct RunRow {
    run_order: i64,
    task_id: String,
    node_id: Option<String>,
    name: Option<String>,
    run_id: Option<String>,
    status: String,
    untracked: bool,
    running_agents: u32,
    agents_total: u32,
}

fn to_u32(v: i64) -> Result<u32> {
    u32::try_from(v).map_err(|_| StoreError::Corrupt(format!("count {v} out of range")))
}

fn runs_from(conn: &Connection, sid: &str, from_order: i64) -> Result<Vec<RunRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT run_order, task_id, node_id, name, run_id, status, untracked,
                running_agents, agents_total
         FROM workflow_runs WHERE session_id = ?1 AND run_order >= ?2
         ORDER BY run_order",
    )?;
    let raw = stmt
        .query_map(params![sid, from_order], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raw.into_iter()
        .map(
            |(run_order, task_id, node_id, name, run_id, status, untracked, running, total)| {
                Ok(RunRow {
                    run_order,
                    task_id,
                    node_id,
                    name,
                    run_id,
                    status,
                    untracked: untracked != 0,
                    running_agents: to_u32(running)?,
                    agents_total: to_u32(total)?,
                })
            },
        )
        .collect()
}

fn phases_of(conn: &Connection, sid: &str, task_id: &str) -> Result<Vec<WorkflowPhase>> {
    let mut stmt = conn.prepare_cached(
        "SELECT idx, title FROM workflow_phases
         WHERE session_id = ?1 AND task_id = ?2 ORDER BY idx",
    )?;
    let raw = stmt
        .query_map(params![sid, task_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raw.into_iter()
        .map(|(idx, title)| {
            Ok(WorkflowPhase {
                index: to_u32(idx)?,
                title,
            })
        })
        .collect()
}

/// An agent of a page: its `idx`, the entry and its cost in the page.
struct Item {
    idx: u32,
    agent: WorkflowAgent,
    cost: usize,
}

/// The next agent of `rows` whose `entry` parses; an entry that does not
/// parse is skipped (debug log): it is not an item.
fn next_item(rows: &mut Rows<'_>, session: &str, task_id: &str) -> Result<Option<Item>> {
    while let Some(row) = rows.next()? {
        let idx: i64 = row.get(0)?;
        let entry: String = row.get(1)?;
        let Ok(agent) = serde_json::from_str::<WorkflowAgent>(&entry) else {
            tracing::debug!(
                session,
                task_id,
                idx,
                "workflow page: an entry does not parse"
            );
            continue;
        };
        let cost = serde_json::to_vec(&agent)?.len() + 1;
        return Ok(Some(Item {
            idx: to_u32(idx)?,
            agent,
            cost,
        }));
    }
    Ok(None)
}

fn run_of(row: &RunRow, phases: Vec<WorkflowPhase>) -> Result<WorkflowRun> {
    Ok(WorkflowRun {
        run_order: to_u64(row.run_order)?,
        state: WorkflowUpdate {
            task_id: row.task_id.clone(),
            node_id: row.node_id.clone(),
            name: row.name.clone(),
            run_id: row.run_id.clone(),
            status: row.status.clone(),
            untracked: row.untracked,
            phases,
            agents: Vec::new(),
            running_agents: row.running_agents,
            agents_total: row.agents_total,
        },
    })
}

/// Builds the page from the open transaction.
fn page_of(
    conn: &Connection,
    sid: &str,
    after: Option<WorkflowsCursor>,
) -> Result<Option<WorkflowsPage>> {
    let as_of: Option<i64> = conn
        .query_row(
            "SELECT session_seq FROM sessions WHERE session_id = ?1",
            [sid],
            |r| r.get(0),
        )
        .optional()?;
    let Some(as_of) = as_of else {
        return Ok(None);
    };
    let as_of = to_u64(as_of)?;
    // A cursor beyond the range of the column matches no run.
    let from_order = after.map_or(0, |c| i64::try_from(c.run_order).unwrap_or(i64::MAX));
    let runs = runs_from(conn, sid, from_order)?;
    let mut agents_stmt = conn.prepare_cached(
        "SELECT idx, entry FROM workflow_agents
         WHERE session_id = ?1 AND task_id = ?2 AND idx > ?3 ORDER BY idx",
    )?;
    let mut page: Vec<WorkflowRun> = Vec::new();
    let mut used = ENVELOPE_BYTES;
    let mut last: Option<(u64, u32)> = None;
    let mut more = false;
    for row in &runs {
        // The cursor run continues: no phases, the agents after the cursor.
        let cursor = after.filter(|c| i64::try_from(c.run_order).ok() == Some(row.run_order));
        let after_idx = cursor.map_or(-1, |c| i64::from(c.agent_index));
        let mut rows = agents_stmt.query(params![sid, row.task_id, after_idx])?;
        let mut peeked = next_item(&mut rows, sid, &row.task_id)?;
        if cursor.is_some() && peeked.is_none() {
            // Nothing of this run remains.
            continue;
        }
        let phases = if cursor.is_some() {
            Vec::new()
        } else {
            phases_of(conn, sid, &row.task_id)?
        };
        let mut run = run_of(row, phases)?;
        let header = serde_json::to_vec(&run)?.len() + 1;
        let first = peeked.as_ref().map_or(0, |i| i.cost);
        if !page.is_empty() && used + header + first > WORKFLOWS_PAGE_BUDGET {
            more = true;
            break;
        }
        used += header;
        let run_order = run.run_order;
        // A run without agents has the cursor (its `run_order`, 0).
        last = Some((run_order, 0));
        let mut first_agent = true;
        while let Some(item) = peeked.take() {
            if !first_agent && used + item.cost > WORKFLOWS_PAGE_BUDGET {
                more = true;
                break;
            }
            first_agent = false;
            used += item.cost;
            last = Some((run_order, item.idx));
            run.state.agents.push(item.agent);
            peeked = next_item(&mut rows, sid, &row.task_id)?;
        }
        page.push(run);
        if more {
            break;
        }
    }
    let next = if more {
        last.map(|(run_order, agent_index)| WorkflowsCursor {
            run_order,
            agent_index,
        })
    } else {
        None
    };
    Ok(Some(WorkflowsPage {
        as_of,
        workflows: page,
        next,
    }))
}

impl Store {
    /// One page of the workflow projection of a session, read in one
    /// transaction. `None` for an unknown session.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn workflow_page(
        &self,
        session_id: SessionId,
        after: Option<WorkflowsCursor>,
    ) -> Result<Option<WorkflowsPage>> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            page_of(&tx, session_id.as_str(), after)
        })
        .await
    }

    /// Writes one row of `workflow_agent_logs` as it is, for the tests of
    /// the checks that read a row again.
    #[cfg(test)]
    pub(crate) async fn put_workflow_agent_log_for_tests(
        &self,
        row: super::workflow_agent_logs::WorkflowAgentLogRow,
    ) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(
                "INSERT INTO workflow_agent_logs
                   (session_id, agent_id, run_id, native_id, cwd, provider_home, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    row.session_id.as_str(),
                    row.agent_id,
                    row.run_id,
                    row.native_id,
                    row.cwd,
                    row.provider_home,
                    row.created_at,
                ],
            )?;
            Ok(())
        })
        .await
    }
}
