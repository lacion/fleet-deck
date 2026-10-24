//! Claude workflows (`local_workflow` tasks): the state of each run and the
//! `workflow.updated` events with only the changes (B12, design
//! `docs/v2/design/workflow-agents.md` §4.1).
//!
//! The CLI sends one task per workflow and `task_progress` frames whose
//! `workflow_progress` list holds the phases and the agents so far. The core
//! keeps a digest per agent, not the entry, so the checkpoint stays small.

use std::collections::{BTreeMap, VecDeque};

use fleetdeck_proto::Event;
use fleetdeck_proto::event::{WORKFLOW_EVENT_BUDGET, WorkflowAgent, WorkflowPhase, WorkflowUpdate};
use fleetdeck_proto::session::MAX_TRACKED_WORKFLOWS;
use serde_json::{Map, Value};

use super::core::{ClaudeCore, Output, ToolNode, truncate};
use super::frames::{MessageFrame, valid_agent_id};

/// The most workflows with a final status that the core keeps.
const MAX_ENDED_WORKFLOWS: usize = 32;
/// The most ids in `untracked_workflows`.
const MAX_UNTRACKED_IDS: usize = 1024;
/// The most agents of one workflow.
const MAX_AGENTS: usize = 1000;
/// The most phases of one workflow.
const MAX_PHASES: usize = 100;

const NAME_CHARS: usize = 80;
const PHASE_CHARS: usize = 120;
const LABEL_CHARS: usize = 120;
const MODEL_CHARS: usize = 64;
const LAST_TOOL_CHARS: usize = 64;
const PREVIEW_CHARS: usize = 200;
const STATE_CHARS: usize = 32;

/// The workflow run of one `local_workflow` task.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct WorkflowEntry {
    pub(super) tool_use_id: Option<String>,
    pub(super) name: Option<String>,
    pub(super) run_id: Option<String>,
    /// "running", "completed", "failed" or "stopped".
    pub(super) status: String,
    /// The value of `next_workflow` at the start (the order for "oldest").
    pub(super) started: u64,
    pub(super) phases: BTreeMap<u32, String>,
    pub(super) agents: BTreeMap<u32, AgentKeep>,
}

/// What the core keeps of one agent.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct AgentKeep {
    /// The normalized state of the last emitted entry (`queued`, `running`, …).
    pub(super) state: String,
    /// FNV-1a 64 of `serde_json::to_vec` of the last emitted `WorkflowAgent` with
    /// `tool_calls`, `tokens` and `last_tool` set to `None`. A different digest is a change.
    pub(super) digest: u64,
}

impl WorkflowEntry {
    fn is_running(&self) -> bool {
        self.status == "running"
    }

    fn running_agents(&self) -> u32 {
        if !self.is_running() {
            return 0;
        }
        let n = self
            .agents
            .values()
            .filter(|a| a.state == "running")
            .count();
        u32::try_from(n).unwrap_or(u32::MAX)
    }
}

/// A task id of `[A-Za-z0-9_-]{1,64}`.
fn valid_task_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// FNV-1a, 64 bits.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn running_entries(core: &ClaudeCore) -> usize {
    core.workflows.values().filter(|w| w.is_running()).count()
}

/// The card node of a tool use, if the core knows it.
fn node_of(core: &ClaudeCore, tool_use_id: Option<&str>) -> Option<String> {
    tool_use_id
        .and_then(|id| core.tools.get(id))
        .map(|n| n.node_id.as_str().to_owned())
}

/// The event of a tracked entry with no phases and no agents.
fn base_update(core: &ClaudeCore, task_id: &str, entry: &WorkflowEntry) -> WorkflowUpdate {
    WorkflowUpdate {
        task_id: task_id.to_owned(),
        node_id: node_of(core, entry.tool_use_id.as_deref()),
        name: entry.name.clone(),
        run_id: entry.run_id.clone(),
        status: entry.status.clone(),
        untracked: false,
        phases: Vec::new(),
        agents: Vec::new(),
        running_agents: entry.running_agents(),
        agents_total: u32::try_from(entry.agents.len()).unwrap_or(u32::MAX),
    }
}

/// The event of a workflow that the core does not follow.
fn untracked_update(core: &ClaudeCore, task_id: &str, status: &str) -> Output {
    let tool_use_id = core
        .tasks
        .get(task_id)
        .and_then(|t| t.tool_use_id.as_deref());
    Output::Emit(Event::WorkflowUpdated(Box::new(WorkflowUpdate {
        task_id: task_id.to_owned(),
        node_id: node_of(core, tool_use_id),
        name: None,
        run_id: None,
        status: status.to_owned(),
        untracked: true,
        phases: Vec::new(),
        agents: Vec::new(),
        running_agents: 0,
        agents_total: 0,
    })))
}

/// A running workflow that the core does not follow: its id goes to the back
/// of the list, and one event says so.
fn start_untracked(core: &mut ClaudeCore, task_id: &str) -> Vec<Output> {
    if core.untracked_workflows.iter().any(|id| id == task_id) {
        return Vec::new();
    }
    if core.untracked_workflows.len() >= MAX_UNTRACKED_IDS {
        core.untracked_workflows.pop_front();
    }
    core.untracked_workflows.push_back(task_id.to_owned());
    vec![untracked_update(core, task_id, "running")]
}

fn new_entry(core: &mut ClaudeCore) -> WorkflowEntry {
    let entry = WorkflowEntry {
        status: "running".to_owned(),
        started: core.next_workflow,
        ..WorkflowEntry::default()
    };
    core.next_workflow += 1;
    entry
}

/// `task_started` of a `local_workflow` task (called after the `task.updated` event).
pub(super) fn on_started(
    core: &mut ClaudeCore,
    task_id: &str,
    body: &Map<String, Value>,
) -> Vec<Output> {
    if !valid_task_id(task_id) {
        tracing::debug!("workflow task id is not valid; no workflow event");
        return Vec::new();
    }
    let tool_use_id = body
        .get("tool_use_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let name = body
        .get("workflow_name")
        .and_then(Value::as_str)
        .map(|n| truncate(n, NAME_CHARS));
    if !core.workflows.contains_key(task_id) {
        if core.untracked_workflows.iter().any(|id| id == task_id) {
            return Vec::new();
        }
        if running_entries(core) >= MAX_TRACKED_WORKFLOWS {
            return start_untracked(core, task_id);
        }
        let entry = new_entry(core);
        core.workflows.insert(task_id.to_owned(), entry);
    }
    let Some(entry) = core.workflows.get_mut(task_id) else {
        return Vec::new();
    };
    // An entry that a progress frame made before the start gets the facts now.
    if tool_use_id.is_some() {
        entry.tool_use_id = tool_use_id;
    }
    if name.is_some() {
        entry.name = name;
    }
    let update = base_update(core, task_id, &core.workflows[task_id]);
    vec![Output::Emit(Event::WorkflowUpdated(Box::new(update)))]
}

/// The status that a final task status gives a workflow.
fn final_status(task_status: &str) -> &'static str {
    match task_status {
        "completed" => "completed",
        "failed" => "failed",
        _ => "stopped",
    }
}

/// A final status of a workflow task (called after the `task.updated` event).
pub(super) fn on_final(core: &mut ClaudeCore, task_id: &str, task_status: &str) -> Vec<Output> {
    let status = final_status(task_status);
    if let Some(at) = core.untracked_workflows.iter().position(|id| id == task_id) {
        core.untracked_workflows.remove(at);
        return vec![untracked_update(core, task_id, status)];
    }
    let Some(entry) = core.workflows.get_mut(task_id) else {
        return Vec::new();
    };
    if !entry.is_running() {
        return Vec::new();
    }
    status.clone_into(&mut entry.status);
    for agent in entry.agents.values_mut() {
        if agent.state == "queued" || agent.state == "running" {
            "stopped".clone_into(&mut agent.state);
        }
    }
    let update = base_update(core, task_id, &core.workflows[task_id]);
    drop_oldest_ended(core);
    vec![Output::Emit(Event::WorkflowUpdated(Box::new(update)))]
}

/// At most `MAX_ENDED_WORKFLOWS` entries with a final status stay: the one
/// with the smallest `started` goes. A running entry never goes.
fn drop_oldest_ended(core: &mut ClaudeCore) {
    let ended: Vec<(&String, u64)> = core
        .workflows
        .iter()
        .filter(|(_, w)| !w.is_running())
        .map(|(id, w)| (id, w.started))
        .collect();
    if ended.len() <= MAX_ENDED_WORKFLOWS {
        return;
    }
    if let Some(oldest) = ended
        .into_iter()
        .min_by_key(|(_, started)| *started)
        .map(|(id, _)| id.clone())
    {
        core.workflows.remove(&oldest);
    }
}

/// The run id in the structured result of the `Workflow` call: it goes into
/// the `provider_refs` of `tool.completed` and into one `workflow.updated`
/// without phases and agents.
pub(super) fn on_run_id(
    core: &mut ClaudeCore,
    tool_use_id: &str,
    node: &ToolNode,
    msg: &MessageFrame,
    provider_refs: &mut Map<String, Value>,
) -> Vec<Output> {
    let (true, Some(run_id)) = (node.name == "Workflow", &msg.workflow_run_id) else {
        return Vec::new();
    };
    provider_refs.insert("workflow_run_id".to_owned(), Value::from(run_id.as_str()));
    let Some(task_id) = core
        .workflows
        .iter()
        .find(|(_, w)| w.tool_use_id.as_deref() == Some(tool_use_id))
        .map(|(id, _)| id.clone())
    else {
        return Vec::new();
    };
    core.workflows
        .entry(task_id.clone())
        .and_modify(|w| w.run_id = Some(run_id.clone()));
    let update = base_update(core, &task_id, &core.workflows[&task_id]);
    vec![Output::Emit(Event::WorkflowUpdated(Box::new(update)))]
}

/// The normalized state of an agent entry (§4.1 rule 3).
fn agent_state(entry: &Value) -> String {
    let started = entry.get("startedAt").is_some_and(Value::is_number);
    match entry.get("state").and_then(Value::as_str) {
        Some("start") if started => "running".to_owned(),
        Some("start") => "queued".to_owned(),
        Some("progress") | None => "running".to_owned(),
        Some("done") => "done".to_owned(),
        Some("error") => "failed".to_owned(),
        Some(other) => truncate(other, STATE_CHARS),
    }
}

fn text(entry: &Value, key: &str, max: usize) -> Option<String> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .map(|t| truncate(t, max))
}

fn number<T: TryFrom<u64>>(entry: &Value, key: &str) -> Option<T> {
    entry
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| T::try_from(n).ok())
}

/// The agent of a `workflow_agent` entry, or `None` for an entry without an
/// integer index or a string label.
fn build_agent(entry: &Value) -> Option<WorkflowAgent> {
    let index = number::<u32>(entry, "index")?;
    let label = truncate(entry.get("label")?.as_str()?, LABEL_CHARS);
    Some(WorkflowAgent {
        index,
        label,
        state: agent_state(entry),
        phase_index: number(entry, "phaseIndex"),
        agent_id: entry
            .get("agentId")
            .and_then(Value::as_str)
            .filter(|id| valid_agent_id(id))
            .map(str::to_owned),
        model: text(entry, "model", MODEL_CHARS),
        attempt: number(entry, "attempt"),
        tool_calls: number(entry, "toolCalls"),
        tokens: number(entry, "tokens"),
        last_tool: text(entry, "lastToolName", LAST_TOOL_CHARS),
        prompt_preview: text(entry, "promptPreview", PREVIEW_CHARS),
        result_preview: text(entry, "resultPreview", PREVIEW_CHARS),
        error: text(entry, "error", PREVIEW_CHARS),
        started_at: entry.get("startedAt").and_then(Value::as_i64),
    })
}

/// The digest of an agent: the entry without the counters that ride along.
fn digest(agent: &WorkflowAgent) -> u64 {
    let mut stable = agent.clone();
    stable.tool_calls = None;
    stable.tokens = None;
    stable.last_tool = None;
    fnv1a(&serde_json::to_vec(&stable).unwrap_or_default())
}

/// `system/task_progress`: only a frame with a `workflow_progress` array counts.
pub(super) fn on_task_progress(core: &mut ClaudeCore, body: &Map<String, Value>) -> Vec<Output> {
    let Some(list) = body.get("workflow_progress").and_then(Value::as_array) else {
        return Vec::new();
    };
    let Some(task_id) = body.get("task_id").and_then(Value::as_str) else {
        return Vec::new();
    };
    if !valid_task_id(task_id) {
        tracing::debug!("workflow task id is not valid; no workflow event");
        return Vec::new();
    }
    if core.untracked_workflows.iter().any(|id| id == task_id) {
        return Vec::new();
    }
    let mut created = false;
    if !core.workflows.contains_key(task_id) {
        if running_entries(core) >= MAX_TRACKED_WORKFLOWS {
            return start_untracked(core, task_id);
        }
        let entry = new_entry(core);
        core.workflows.insert(task_id.to_owned(), entry);
        created = true;
    }
    let Some(entry) = core.workflows.get_mut(task_id) else {
        return Vec::new();
    };
    // No running event after the end.
    if !entry.is_running() {
        return Vec::new();
    }
    let mut phases: BTreeMap<u32, String> = BTreeMap::new();
    let mut agents: BTreeMap<u32, WorkflowAgent> = BTreeMap::new();
    for item in list {
        match item.get("type").and_then(Value::as_str) {
            Some("workflow_phase") => {
                let (Some(index), Some(title)) = (
                    number::<u32>(item, "index"),
                    item.get("title").and_then(Value::as_str),
                ) else {
                    continue;
                };
                let title = truncate(title, PHASE_CHARS);
                let known = entry.phases.get(&index);
                if known.is_none() && entry.phases.len() >= MAX_PHASES {
                    continue;
                }
                if known != Some(&title) {
                    entry.phases.insert(index, title.clone());
                    phases.insert(index, title);
                }
            }
            Some("workflow_agent") => {
                let Some(agent) = build_agent(item) else {
                    continue;
                };
                let hash = digest(&agent);
                let known = entry.agents.get(&agent.index);
                if known.is_none() && entry.agents.len() >= MAX_AGENTS {
                    continue;
                }
                if known.is_none_or(|k| k.digest != hash) {
                    entry.agents.insert(
                        agent.index,
                        AgentKeep {
                            state: agent.state.clone(),
                            digest: hash,
                        },
                    );
                    agents.insert(agent.index, agent);
                }
            }
            _ => {}
        }
    }
    if !created && phases.is_empty() && agents.is_empty() {
        return Vec::new();
    }
    let base = base_update(core, task_id, &core.workflows[task_id]);
    let phases = phases
        .into_iter()
        .map(|(index, title)| WorkflowPhase { index, title })
        .collect();
    chunks(base, phases, agents.into_values().collect())
}

/// The events of one frame (§4.1 rule 9): the first carries every changed
/// phase; the agents follow in index order, each event at most
/// `WORKFLOW_EVENT_BUDGET` bytes of JSON.
fn chunks(
    base: WorkflowUpdate,
    phases: Vec<WorkflowPhase>,
    agents: Vec<WorkflowAgent>,
) -> Vec<Output> {
    let size = |u: &WorkflowUpdate| serde_json::to_vec(u).map_or(0, |v| v.len());
    let mut first = base.clone();
    first.phases = phases;
    let mut events: VecDeque<WorkflowUpdate> = VecDeque::new();
    let mut current = first;
    let mut current_size = size(&current);
    let empty_size = size(&base);
    for agent in agents {
        let agent_size = serde_json::to_vec(&agent).map_or(0, |v| v.len()) + 1;
        if !current.agents.is_empty() && current_size + agent_size > WORKFLOW_EVENT_BUDGET {
            let next = base.clone();
            events.push_back(std::mem::replace(&mut current, next));
            current_size = empty_size;
        }
        current_size += agent_size;
        current.agents.push(agent);
    }
    events.push_back(current);
    events
        .into_iter()
        .map(|u| Output::Emit(Event::WorkflowUpdated(Box::new(u))))
        .collect()
}
