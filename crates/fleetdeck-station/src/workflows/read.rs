//! `session.workflow_agent` (B12, design `docs/v2/design/workflow-agents.md`
//! §4.4): the transcript of one workflow agent. The caller gives a session
//! and an agent id, never a path or a run id. The station finds the file from
//! its own binding row; the file is opened without following a link, read
//! once up to the size captured at open, and every record must name the
//! parent session and the agent.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::check_native_session_id;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::{HistoryState, HistoryUnavailable, code};
use fleetdeck_proto::subagent::{SubagentLog, WorkflowAgentQuery};

use crate::history::subagent::read_log;
use crate::history::{Fail, History, HistoryError, locate};
use crate::store::Store;
use crate::store::workflow_agent_logs::{WorkflowAgentLogRow, agent_id_ok, run_id_ok};
use crate::workspace::PrivatePaths;

fn unavailable(reason: HistoryUnavailable) -> SubagentLog {
    SubagentLog {
        items: Vec::new(),
        state: HistoryState::Unavailable { reason },
    }
}

/// `session.workflow_agent`.
///
/// # Errors
/// [`HistoryError`] with `invalid_params`, `unknown_session`, `busy` or
/// `internal`. `unavailable` and `partial` are answers.
pub async fn answer(
    store: &Store,
    history: &History,
    private: &Arc<PrivatePaths>,
    query: WorkflowAgentQuery,
) -> Result<SubagentLog, HistoryError> {
    if !agent_id_ok(&query.agent_id) {
        return Err(HistoryError::new(
            code::INVALID_PARAMS,
            "agent_id must be 1 to 64 of a-z 0-9",
        ));
    }
    let session_id: SessionId = query.session_id;
    let facts = store
        .history_facts(session_id.clone())
        .await
        .map_err(crate::history::internal)?
        .ok_or_else(|| HistoryError::new(code::UNKNOWN_SESSION, "no such session"))?;
    if facts.provider != ProviderKind::Claude {
        return Ok(unavailable(HistoryUnavailable::UnsupportedFormat));
    }
    let row = store
        .workflow_agent_log(session_id, query.agent_id)
        .await
        .map_err(crate::history::internal)?;
    let Some(row) = row else {
        return Ok(unavailable(HistoryUnavailable::NotFound));
    };
    // The row comes from this station, but the names become path segments:
    // check them again.
    let home = PathBuf::from(&row.provider_home);
    if !home.is_absolute()
        || !agent_id_ok(&row.agent_id)
        || !run_id_ok(&row.run_id)
        || check_native_session_id(&row.native_id).is_err()
    {
        return Ok(unavailable(HistoryUnavailable::NotTrusted));
    }
    let deadline = Instant::now() + history.limits().deadline;
    let this = history.clone();
    let private = Arc::clone(private);
    let worked = history
        .run(deadline, move |deadline| {
            read_agent(&this, &row, home, &private, deadline)
        })
        .await;
    match worked {
        Ok(log) => Ok(log),
        Err(Fail::Busy) => Err(crate::history::busy()),
        Err(Fail::Unavailable(reason)) => Ok(unavailable(reason)),
        Err(fail) => Err(crate::history::internal(format!(
            "workflow agent log: {fail:?}"
        ))),
    }
}

/// The blocking part of [`answer`]: open the file and read it once.
fn read_agent(
    history: &History,
    row: &WorkflowAgentLogRow,
    home: PathBuf,
    private: &PrivatePaths,
    deadline: Instant,
) -> Result<SubagentLog, Fail> {
    let limits = history.limits();
    let trust = locate::Trust {
        private,
        uid: history.uid(),
        limits,
        deadline,
    };
    let opened = locate::locate_workflow_agent(
        &home,
        &row.cwd,
        &row.native_id,
        &row.run_id,
        &row.agent_id,
        &trust,
    )?;
    read_log(
        &opened.file,
        opened.size,
        &row.native_id,
        &row.agent_id,
        limits,
        deadline,
    )
}
