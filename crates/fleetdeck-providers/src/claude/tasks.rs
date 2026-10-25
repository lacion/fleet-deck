//! `system/task_started`, `task_updated` and `task_notification` frames →
//! `task.updated` events, and the tools that stay open under a running task
//! (B6, `docs/v2/design/agents-view.md` §2.1, §2.2).

use std::collections::BTreeSet;

use fleetdeck_proto::{Event, TurnId};
use serde_json::{Map, Value};

use super::close_ids;
use crate::claude::core::{ClaudeCore, Output, truncate};
use crate::claude::workflows;

/// The longest `summary` of a `task.updated` event.
const SUMMARY_CHARS: usize = 200;

/// `task_kind` of a task that no `task_started` frame announced.
const UNKNOWN_KIND: &str = "unknown";

/// `task_type` of a Claude workflow (B12).
const WORKFLOW_KIND: &str = "local_workflow";

/// The statuses that end a task; any other text keeps the task running.
const FINAL_STATUSES: &[&str] = &["completed", "failed", "killed", "stopped", "cancelled"];

/// One `system/task_*` frame. A frame without a task id counts as unknown;
/// `task_updated` and `task_notification` without a status change nothing.
pub(super) fn on_task_frame(
    core: &mut ClaudeCore,
    subtype: &str,
    body: &Map<String, Value>,
) -> Vec<Output> {
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
    let Some(task_id) = text(body.get("task_id")) else {
        core.unknown_frames += 1;
        return Vec::new();
    };
    let mut description = None;
    let mut summary = None;
    let status = match subtype {
        "task_started" => {
            let entry = core.tasks.entry(task_id.clone()).or_default();
            entry.tool_use_id = text(body.get("tool_use_id"));
            entry.task_kind = text(body.get("task_type")).unwrap_or_default();
            entry.running = true;
            description = text(body.get("description"));
            Some("running".to_owned())
        }
        "task_updated" => text(body.get("patch").and_then(|p| p.get("status"))),
        _ => {
            // An empty summary is no summary.
            summary = text(body.get("summary"))
                .filter(|s| !s.is_empty())
                .map(|s| truncate(&s, SUMMARY_CHARS));
            text(body.get("status"))
        }
    };
    let Some(status) = status else {
        return Vec::new();
    };
    let entry = core.tasks.entry(task_id.clone()).or_default();
    let ended = FINAL_STATUSES.contains(&status.as_str());
    if ended {
        entry.running = false;
    }
    let key = (status.clone(), summary.clone());
    // Only a change of the status or the summary is an event.
    if entry.last.as_ref() == Some(&key) {
        return Vec::new();
    }
    entry.last = Some(key);
    let task_kind = if entry.task_kind.is_empty() {
        UNKNOWN_KIND.to_owned()
    } else {
        entry.task_kind.clone()
    };
    let tool_use_id = entry.tool_use_id.clone();
    let node_id = tool_use_id
        .as_deref()
        .and_then(|id| core.tools.get(id))
        .map(|n| n.node_id.as_str().to_owned());
    let workflow_start = subtype == "task_started" && task_kind == WORKFLOW_KIND;
    let mut out = vec![Output::Emit(Event::TaskUpdated {
        task_id: task_id.clone(),
        node_id,
        task_kind,
        status: status.clone(),
        description,
        summary,
    })];
    // A workflow run also has its own events (B12), after the task event.
    if workflow_start {
        out.extend(workflows::on_started(core, &task_id, body));
    } else if ended {
        out.extend(workflows::on_final(core, &task_id, &status));
    }
    // A task that ends closes what is still open under it, after its event.
    if ended && let Some(root) = tool_use_id {
        let ids = open_under(core, &root);
        out.extend(close_ids(core, &ids));
    }
    out
}

#[cfg(test)]
#[path = "tests_tasks.rs"]
mod tests;

/// The turn that a new `Agent` or `Task` call belongs to: the running turn,
/// or, with no turn running, the launch turn of the parent call (B6).
pub(super) fn launch_turn(
    core: &ClaudeCore,
    name: &str,
    parent_tool: Option<&str>,
) -> Option<TurnId> {
    if !matches!(name, "Agent" | "Task") {
        return None;
    }
    core.turn.clone().or_else(|| {
        parent_tool
            .and_then(|p| core.tools.get(p))
            .and_then(|n| n.launch_turn.clone())
    })
}

/// The turn of the text of a child message: the launch turn of its subagent
/// call when it has one, else the running turn (B6).
pub(super) fn child_turn(core: &ClaudeCore, parent_tool: Option<&str>) -> Option<TurnId> {
    parent_tool
        .and_then(|p| core.tools.get(p))
        .and_then(|n| n.launch_turn.clone())
        .or_else(|| core.turn.clone())
}

/// The ancestors of a tool, nearest first (a bounded walk).
fn ancestors<'a>(core: &'a ClaudeCore, id: &str) -> impl Iterator<Item = &'a str> {
    let mut next = core.tools.get(id).and_then(|n| n.parent_tool.as_deref());
    let mut left = core.tools.len();
    std::iter::from_fn(move || {
        let current = next?;
        left = left.checked_sub(1)?;
        next = core
            .tools
            .get(current)
            .and_then(|n| n.parent_tool.as_deref());
        Some(current)
    })
}

/// The open tools that run under the tool `root`.
fn open_under(core: &ClaudeCore, root: &str) -> BTreeSet<String> {
    core.tools
        .iter()
        .filter(|(id, n)| !n.done && ancestors(core, id).any(|a| a == root))
        .map(|(id, _)| id.clone())
        .collect()
}

/// The open tools that a `result` frame closes: all of them, except the ones
/// under a call whose task still runs (they complete with their own result,
/// or when the task ends).
pub(super) fn closable_at_result(core: &ClaudeCore) -> BTreeSet<String> {
    let running: BTreeSet<&str> = core
        .tasks
        .values()
        .filter(|t| t.running)
        .filter_map(|t| t.tool_use_id.as_deref())
        .collect();
    core.tools
        .iter()
        .filter(|(id, n)| !n.done && !ancestors(core, id).any(|a| running.contains(a)))
        .map(|(id, _)| id.clone())
        .collect()
}
