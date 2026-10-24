//! The workflow events and pages (B12, design `docs/v2/design/workflow-agents.md`
//! §4.2): the `workflow.updated` payload, the projection page of the link
//! method `session.workflows` (feature `workflows.v1`) and their limits.
//! Split from `event.rs` for the module size rule.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::SessionId;

/// The most JSON bytes of one [`WorkflowUpdate`] (§4.1 rule 9).
pub const WORKFLOW_EVENT_BUDGET: usize = 256 * 1024;

/// The most JSON bytes of one [`WorkflowsPage`].
pub const WORKFLOWS_PAGE_BUDGET: usize = 512 * 1024;

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// The payload of `workflow.updated`: a Claude workflow run changed (B12).
/// `phases` and `agents` hold only the entries that are new or changed (merge
/// by `index`; an agent entry replaces the earlier entry of its index); the
/// other fields are absolute. One frame can give several events (§4.1 rule 9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct WorkflowUpdate {
    pub task_id: String,
    /// The node of the Workflow tool card, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub node_id: Option<String>,
    /// The `meta.name` of the script.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// The workflow run id (`wf_…`), the folder of its agent transcripts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub run_id: Option<String>,
    /// `running`, `completed`, `failed`, `stopped`.
    pub status: String,
    /// True: the provider does not follow this workflow (too many run); no
    /// agents come.
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(optional, as = "Option<bool>")]
    pub untracked: bool,
    #[serde(default)]
    pub phases: Vec<WorkflowPhase>,
    #[serde(default)]
    pub agents: Vec<WorkflowAgent>,
    pub running_agents: u32,
    pub agents_total: u32,
}

/// One phase of a workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct WorkflowPhase {
    pub index: u32,
    pub title: String,
}

/// One agent of a workflow.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct WorkflowAgent {
    pub index: u32,
    pub label: String,
    /// `queued`, `running`, `done`, `failed`, `stopped`, or the provider's text.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub phase_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_calls: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prompt_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub result_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    /// Unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub started_at: Option<i64>,
}

/// `session.workflows` params (B12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "event/")]
pub struct WorkflowsQuery {
    pub session_id: SessionId,
    /// The `next` of the previous page; absent for the first page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub after: Option<WorkflowsCursor>,
}

/// Where a page of `session.workflows` stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "event/")]
pub struct WorkflowsCursor {
    /// The `run_order` of the run where the previous page stopped.
    pub run_order: u64,
    /// The last agent index of that run on the previous page.
    pub agent_index: u32,
}

/// One page of the workflow projection of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct WorkflowsPage {
    /// The session's `session_seq` when the station read this page. The page
    /// holds the effect of every event of the session up to it.
    pub as_of: u64,
    /// In `run_order`. Here `phases` and `agents` are complete, not changes.
    /// A run that the budget cuts continues on the next page with the same
    /// fields, no phases, and the agents after `next.agent_index`.
    pub workflows: Vec<WorkflowRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next: Option<WorkflowsCursor>,
}

/// One run of a [`WorkflowsPage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct WorkflowRun {
    /// The station order of the run (the `station_seq` of its first event).
    pub run_order: u64,
    pub state: WorkflowUpdate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagent::WorkflowAgentQuery;
    use serde_json::json;

    fn update() -> WorkflowUpdate {
        WorkflowUpdate {
            task_id: "t1".into(),
            node_id: None,
            name: None,
            run_id: None,
            status: "running".into(),
            untracked: false,
            phases: Vec::new(),
            agents: Vec::new(),
            running_agents: 0,
            agents_total: 0,
        }
    }

    #[test]
    fn workflow_update_round_trips_and_skips_empty_options() {
        let value = serde_json::to_value(update()).unwrap();
        assert_eq!(
            value,
            json!({
                "task_id": "t1",
                "status": "running",
                "phases": [],
                "agents": [],
                "running_agents": 0,
                "agents_total": 0
            })
        );
        // An event without `phases` and `agents` reads as empty.
        let back: WorkflowUpdate = serde_json::from_value(json!({
            "task_id": "t1",
            "status": "running",
            "running_agents": 0,
            "agents_total": 0
        }))
        .unwrap();
        assert_eq!(back, update());

        let mut full = update();
        full.untracked = true;
        full.name = Some("review".into());
        full.agents.push(WorkflowAgent {
            index: 2,
            label: "check".into(),
            state: "running".into(),
            tokens: Some(1_500),
            started_at: Some(1_791_000_000_000),
            ..WorkflowAgent::default()
        });
        let value = serde_json::to_value(&full).unwrap();
        assert_eq!(value["untracked"], true);
        let agent = value["agents"][0].as_object().unwrap();
        assert!(!agent.contains_key("model"));
        assert!(!agent.contains_key("error"));
        let back: WorkflowUpdate = serde_json::from_value(value).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn an_older_reader_keeps_workflow_updated_as_unknown() {
        use crate::event::{Event, EventBody};
        assert!(Event::KINDS.contains(&"workflow.updated"));
        // An extra field of a newer peer is ignored by a known kind.
        let body: EventBody = serde_json::from_value(json!({
            "kind": "workflow.updated",
            "task_id": "t1",
            "status": "running",
            "running_agents": 0,
            "agents_total": 0,
            "future_field": [1, 2]
        }))
        .unwrap();
        assert_eq!(body.kind_name(), "workflow.updated");
        assert_eq!(body.known().map(Event::kind_name), Some("workflow.updated"));
        // A reader that lacks the kind keeps the whole payload verbatim.
        let raw = json!({"kind": "workflow.future", "task_id": "t1"});
        let body: EventBody = serde_json::from_value(raw.clone()).unwrap();
        assert!(body.known().is_none());
        assert_eq!(serde_json::to_value(&body).unwrap(), raw);
    }

    #[test]
    fn workflow_agent_query_and_workflows_query_refuse_unknown_fields() {
        let ok = json!({"session_id": "s1", "agent_id": "a1b2"});
        let q: WorkflowAgentQuery = serde_json::from_value(ok.clone()).unwrap();
        assert_eq!(q.agent_id, "a1b2");
        let mut bad = ok;
        bad["path"] = json!("/etc");
        assert!(serde_json::from_value::<WorkflowAgentQuery>(bad).is_err());

        let ok = json!({"session_id": "s1"});
        let q: WorkflowsQuery = serde_json::from_value(ok.clone()).unwrap();
        assert_eq!(q.after, None);
        let mut bad = ok.clone();
        bad["extra"] = json!(1);
        assert!(serde_json::from_value::<WorkflowsQuery>(bad).is_err());
        let mut bad = ok;
        bad["after"] = json!({"run_order": 1, "agent_index": 2, "x": 3});
        assert!(serde_json::from_value::<WorkflowsQuery>(bad).is_err());
    }
}
