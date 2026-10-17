//! `task.updated` from the states of Codex sub-agents (B6,
//! `docs/v2/design/agents-view.md` §2.1): the `agentsStates` of a collab item
//! and the turns of a child thread.

use fleetdeck_proto::{Event, NodeId};
use serde_json::Value;

use super::str_at;
use crate::codex::core::{CodexCore, Output};
use crate::codex::truncate;

#[cfg(test)]
#[path = "tests_agents.rs"]
mod tests;

/// The `task_kind` of a Codex sub-agent.
const TASK_KIND: &str = "codex_agent";

/// The longest `summary` of a `task.updated` event.
const SUMMARY_CHARS: usize = 200;

/// The canonical status of a native agent or turn status; other text stays.
fn status_of(native: &str) -> &str {
    match native {
        "pendingInit" | "running" | "inProgress" => "running",
        "completed" => "completed",
        "errored" | "failed" | "error" => "failed",
        "shutdown" | "interrupted" => "stopped",
        other => other,
    }
}

/// A summary text, cut; an empty text is no summary.
fn summary_of(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| truncate(text, SUMMARY_CHARS))
}

/// The text of the last agent message of a completed turn.
fn last_message(turn: &Value) -> Option<String> {
    turn.get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|i| str_at(i, "type") == Some("agentMessage"))
        .filter_map(|i| str_at(i, "text"))
        .rfind(|t| !t.is_empty())
        .and_then(summary_of)
}

impl CodexCore {
    /// One `task.updated` per thread of the item's `agentsStates` whose
    /// status or summary changed. The node is the call that named the thread
    /// first, else the item's own node.
    pub(super) fn agent_states(&mut self, item: &Value, item_node: &NodeId) -> Vec<Output> {
        let Some(states) = item.get("agentsStates").and_then(Value::as_object) else {
            return Vec::new();
        };
        states
            .iter()
            .filter_map(|(thread, state)| {
                let status = status_of(str_at(state, "status")?).to_owned();
                let summary = str_at(state, "message").and_then(summary_of);
                let node = self.child_threads.get(thread).unwrap_or(item_node).clone();
                self.agent_event(thread, &node, status, summary)
            })
            .collect()
    }

    /// `turn/started` and `turn/completed` of a child thread: the agent runs
    /// or ends. A thread that no call named reports nothing.
    pub(super) fn on_child_turn(&mut self, method: &str, p: &Value) -> Vec<Output> {
        let Some(thread) = str_at(p, "threadId") else {
            return Vec::new();
        };
        let Some(node) = self.child_threads.get(thread).cloned() else {
            return Vec::new();
        };
        let turn = p.get("turn").cloned().unwrap_or_default();
        let (status, summary) = if method == "turn/started" {
            ("running", None)
        } else {
            (
                status_of(str_at(&turn, "status").unwrap_or("completed")),
                last_message(&turn),
            )
        };
        self.agent_event(thread, &node, status.to_owned(), summary)
            .into_iter()
            .collect()
    }

    /// The event for a thread, unless its status and summary are the ones of
    /// its last event.
    fn agent_event(
        &mut self,
        thread: &str,
        node: &NodeId,
        status: String,
        summary: Option<String>,
    ) -> Option<Output> {
        let key = (status.clone(), summary.clone());
        if self.agent_last.get(thread) == Some(&key) {
            return None;
        }
        self.agent_last.insert(thread.to_owned(), key);
        Some(Output::Emit(Event::TaskUpdated {
            task_id: thread.to_owned(),
            node_id: Some(node.as_str().to_owned()),
            task_kind: TASK_KIND.to_owned(),
            status,
            description: None,
            summary,
        }))
    }
}
