# Workflows and agents in the session and the fleet view (B12, B19)

- **Status:** Draft, revision 1 (2026-10-23), for the architect. Risk: **high** (§5): a new station read path of provider files, an additive station migration, a new link method and a new public event kind. Adversary DESIGN REVIEW before the build of T3 and T4; T1, T2, T5, T6 can start after the architect accepts §4.
- **Owner requests:** B12 "Workflows (ultracode) show nothing in the session" and B19 "The fleet view shows the agents too: per session and in total, with ultracode workflows" (`docs/v2/BACKLOG.md:105-118`, `:228-243`).
- **Base:** `v2` at `497b2fbc`. Every `file:line` below is on that commit.
- **Recordings:** `~/fleetdeck-briefs/backlog/workflow-agents/` (§2). Claude Code 2.1.308 on Lighting; one Fleet Deck session on dev1 (Claude 2.1.306).

## 1. Problem and verified facts

### 1.1 The problem

A Claude session that runs the `Workflow` tool starts many agents in phases. The board shows one "Workflow" tool card that is "done" at once, and one background task. The owner cannot see the phases, the agents, their states or their chats. The fleet view counts a workflow as one "background task" and shows no agent total.

### 1.2 Facts that the backlog states, checked on `497b2fbc`

| Backlog fact | Check | Evidence |
| --- | --- | --- |
| B6 reads `task_started`, `task_updated`, `task_notification` into `task.updated` | True | `crates/fleetdeck-providers/src/claude/tasks.rs:24-92`; dispatch `claude/map.rs:178-180` |
| `background_tasks_changed` → `background.updated` | True | `claude/map.rs:156-177` |
| Only `local_agent`, `remote_agent` count as subagents | True | Rust `crates/fleetdeck-proto/src/session.rs:352`; TS `web/src/session/timeline.ts:132`; fold `crates/fleetdeck-proto/src/fold.rs:158-165` |
| The code has no mention of workflows | **Partly wrong** | No provider, proto, station or deck code reads a workflow. The board names the `Workflow` tool: its script name is the card target (`web/src/watch/tooltext.ts:160,193-197`), `agentFile` shortens a script path (`web/src/chat/labels.ts:147`), and the icon is `Workflow` (`web/src/chat/ChatView.tsx:99`). |
| The cards show "Background tasks" with `s.background_tasks.length` | True, but incomplete | `web/src/board/FleetStrip.tsx:289-296`. The strip **also** shows "Subagents running" with `runningSubagents(s)` (`FleetStrip.tsx:166,270-278`; `timeline.ts:580-588`: running subagent tools + `background_agents`). The backlog says the subagent count is not shown in the fleet view: that is **wrong** since the live UI audit. |
| `background_tasks` counts all kinds | True: a `local_agent` and a `local_workflow` are in both badges | fold `fold.rs:158-160` keeps every id |
| The fleet panel is `web/src/fleet/FleetPanel.tsx` | **Wrong subject** | `FleetPanel.tsx:1-40` is the Mail & tasks panel. The fleet header sentence is `web/src/views/FleetSentence.tsx:16-60`, used by `web/src/app/FleetHome.tsx:463` and `web/src/watch/WatchPage.tsx:533`. |
| The fleet view has no agent total anywhere | True | `FleetSentence.tsx` says sessions, stations, needs-you and conflicts only |
| The display route copies `background_agents` | True | `crates/fleetdeck-deck/src/http/display_view.rs:82`. It does **not** copy `running_tools`, so a foreground subagent never counts on a wall. |

### 1.3 More facts (code)

- A `Workflow` call maps to `tool_kind: other` (`claude/map.rs:30-41`). Its `tool.completed` comes at once, because the CLI answers `async_launched` (§2.2).
- `system/task_progress` is not handled: it falls to `_ => Vec::new()` (`claude/map.rs:183`).
- The frame parser keeps only `tool_use_result.agentId` of a structured result (`claude/frames.rs:150-162`). The `runId` of a workflow is dropped.
- A permission request carries the asking agent in `agent_id`; the adapter drops it (`claude/requests.rs:93-150`).
- The core keeps tasks in `ClaudeCore.tasks` (`claude/core.rs:151-161`, `:208-211`). The whole core is the shim checkpoint (`claude/checkpoint.rs:1-8`), so new fields with `#[serde(default)]` survive a shim takeover.
- The board's `taskStates` overwrites the kind of a task with every event (`web/src/session/tasks.ts:40`). An event with `task_kind: "unknown"` (a notification of a task that the current process did not start, §2.4) erases `local_workflow`.
- The Agents switcher lists subagents and `local_bash` tasks only (`web/src/chat/AgentsMenu.tsx:153-250`; `web/src/session/tasks.ts:105-129`).
- DL-191 "Show its work": the station stores a binding row per `Agent`/`Task` tool use (`crates/fleetdeck-station/src/history/subagent.rs:125-185`, table V25 `crates/fleetdeck-station/src/store/subagent_logs.rs:13-26`), opens `projects/<cwd>/<native>/subagents/agent-<id>.jsonl` without links (`history/locate.rs:180-211`), and accepts only records that name the native session and the agent (`history/parse.rs:111-135`).
- Size limits (rule: under 800 lines, `tools/module-size.sh`): `web/src/session/SessionView.tsx` 797, `crates/fleetdeck-deck/src/http/methods_table_rest.rs` 792, `crates/fleetdeck-station/src/link/intake.rs` 768, `claude/map.rs` 743, `web/src/chat/ChatView.tsx` 715, `claude/core.rs` 685, `crates/fleetdeck-station/src/store/mod.rs` 709. The build plan (§6) puts new code in new files.
- The last station migration is V42 (`crates/fleetdeck-station/src/store/migrations.rs:643`).
- Since `0e6c3600` a Claude `usage.updated` is the growth of the process total (`claude/core.rs:224-229`). The growth of the result after a workflow includes the workflow agents (§2.5).

## 2. Recordings (what Claude really sends)

All under `~/fleetdeck-briefs/backlog/workflow-agents/`. Home paths are `/home/me` in the fixtures; no token, e-mail or account id is in the files.

| Folder | Scenario |
| --- | --- |
| `rec1/` | Two phases, one agent each (`bl-tiny`), station mode (`tools/drive.py`: the args of `claude/args.rs:84-120`, `initialize`, a stream-json prompt, every tool allowed) |
| `rec2/` | Two parallel agents with a Bash call each, then one joiner |
| `rec3/` | A workflow agent that needs a permission (Write) |
| `rec4/` | Two slow agents; an `interrupt` after the main turn ended |
| `rec5/` + `rec6/` | The CLI process ends during a workflow; then `--resume` |
| `rec-fleetdeck/dev1-otter-2hw2.txt` | The same workflow in a Fleet Deck session on dev1: what the board gets |
| `disk/` | The workflow files on disk (state JSON, journal, agent meta, one agent transcript, the task output) |
| `fixtures/*.jsonl` | Out-only fixtures for tests (T2 copies them, §6) |
| `tools/` | `drive.py`, `drive2.py` (with `--cwd`), `to_fixture.py`, the CLI strings dump |

### 2.1 The Workflow tool is offered in `-p` mode

The `init` frame of `claude -p --output-format stream-json` lists `Workflow` in `tools` (2.1.308). The model calls it when the prompt opts in ("Use a workflow"). In the default mode the call needs a permission (`can_use_tool`, `tool_name: "Workflow"`).

### 2.2 The frames of one run (`rec1`, `rec2`)

In this order:

1. `assistant` `tool_use` `name: "Workflow"`, `input.script` (or `input.scriptPath`).
2. `system/background_tasks_changed`: `tasks: [{task_id: "wey0ztgn3", run_id: "0mvl5w77o-ad07a0c9", task_type: "local_workflow", description}]`. This `run_id` is a CLI task run id, **not** the workflow run id.
3. `system/task_started`: `task_id`, `tool_use_id` (the Workflow call), `task_type: "local_workflow"`, `workflow_name: "bl-tiny"` (the script `meta.name`), `description`, `prompt` (the script text).
4. `user` `tool_result` "Workflow launched in background. …" with a structured `tool_use_result: {status: "async_launched", taskId, taskType: "local_workflow", workflowName, runId: "wf_0918c8f1-e62", summary, transcriptDir, scriptPath}`.
5. `system/task_progress` frames, many. Each has `task_id`, `tool_use_id`, `description` ("Alpha: alpha-agent"), `usage {total_tokens, tool_uses, duration_ms}`, `last_tool_name`. **Some** of them also have `workflow_progress`: a list of `{type: "workflow_phase", index, title}` and `{type: "workflow_agent", index, label, phaseIndex, phaseTitle, agentId, model, state, startedAt, queuedAt, attempt, promptPreview, promptFramed, lastProgressAt, tokens?, toolCalls?, durationMs?, resultPreview?, lastToolName?, error?}`. In the recordings the list always held every phase and every agent so far (a snapshot); the CLI merges entries by `(type, index)` (CLI strings, `tools/claude-2.1.308.strings`), so the design treats a list as an upsert by `(type, index)`.
6. Agent `state` values seen: `start`, `progress`, `done`. The CLI code also writes `error` (with `error` text, sometimes `blocked: true`), and a `start` entry without `startedAt` is queued (the CLI counts it as not running).
7. `system/task_updated` `patch: {status: "completed", end_time}`; `system/task_notification` `status: "completed"`, `summary: "Dynamic workflow \"…\" completed"`, `output_file` (`/tmp/claude-<uid>/…/tasks/<task_id>.output`, the JSON of the run); `background_tasks_changed` `tasks: []`.
8. The notification starts a provider turn: a second `init`, the answer, a `result`.

What Claude does **not** send: no `assistant` or `user` frame with a `parent_tool_use_id` for a workflow agent, no `stream_event` of an agent, no `task_started` per agent. The agents' chats are only in files (§2.3). `log()` messages are not in the stream.

### 2.3 Where Claude keeps a workflow on disk

For a session `<native>` in project folder `<enc>` (`~/.claude/projects/<enc>/`):

- `<native>/workflows/scripts/<name>-<runId>.js`: the script.
- `<native>/workflows/<runId>.json`: written at the end (completed or killed): `status` (`completed`, `killed`), `result`, `logs`, `phases`, `workflowProgress` (the last snapshot), `agentCount`, `totalTokens`, `error`. A killed run keeps its agents in `progress` (`disk/rec4/wf_b1b2c760-c38.json`). A run that dies with its process has **no** such file (`rec5`).
- `<native>/subagents/workflows/<runId>/journal.jsonl`: `launched`, `started {agentId, label, phase}`, `result {agentId, result}`.
- `<native>/subagents/workflows/<runId>/agent-<agentId>.jsonl` and `.meta.json` (`agentType: "workflow-subagent"`, `workflowPhase`). Every `user`/`assistant` record has `sessionId == <native>`, `agentId == <agentId>`, `isSidechain: true` (checked on all agent files of `rec2`). So the DL-191 parser `parse_line_subagent` reads them without change; only the folder differs.
- The main transcript `<native>.jsonl` has no agent record and no progress.

### 2.4 Stop, process end, resume

- **Interrupt** (`rec4`): an `interrupt` control request, sent after the main turn had ended, killed the running workflow: `task_updated` `killed`, `task_notification` `stopped` (summary = the description), and the agents' own background Bash tasks (`owned_by_subagent: true`, `local_bash`) were killed too. No final `workflow_progress` came: the agents stay `start`/`progress` in the stream.
- **Process end** (`rec5`): the CLI process ended during a run. No `<runId>.json` was written; the journal has only the finished agents.
- **Resume** (`rec6`): the first frame of `claude --resume` is `task_notification` `{task_id: "w9hw2q22t", tool_use_id, status: "stopped", output_file: "", summary: "Background workflow \"bl-orphan\" (run wf_78e3fae0-176) didn't finish before the previous session ended"}`, before `init`, then a `result` with `num_turns: 0` and cost 0, then the real turn. The new process does not restart the workflow.
- **Permission of an agent** (`rec3`): a workflow agent's tool asks with a normal `can_use_tool` that adds `agent_id: "a3f92b5c63d2c47ee"` and the agent's `tool_use_id` (unknown to the main stream).

### 2.5 The gap in Fleet Deck (dev1, `rec-fleetdeck/`)

`read_session` of the dev1 session shows: `tool.started` "Workflow" (`other`), `approval.requested` "Workflow", `background.updated [local_workflow]`, `task.updated local_workflow running`, `tool.completed ok`, `task.updated completed` (twice), `background.updated []`, then a provider turn (`run.started origin provider`, "provider turn") with the result. No phase, no agent, no count. The usage of the provider turn grew by $0.40 for 18 output tokens: the growth of the process total includes the workflow agents (a fact for B14).

## 3. Answers to the questions of the backlog

### 3.1 B12

1. **Which frames?** One task per workflow (`task_type: local_workflow`, `task_started`/`task_updated`/`task_notification`), plus `task_progress` frames whose `workflow_progress` list carries the phases and the agents with their states (§2.2). There is no task per agent and no progress frame per agent.
2. **Are the agents in the session transcript?** No. Each agent has its own transcript in `subagents/workflows/<runId>/agent-<agentId>.jsonl` (§2.3). Its records pass the DL-191 checks unchanged. The station reads it like "Show its work", with the run id as one more folder (§4.4).
3. **A group in the switcher or a separate view?** A group in the Agents switcher: workflow → phase → agent (§4.6). An agent row opens an agent view with the agent's facts and "Show its work".
4. **Shim takeover and resume?** A takeover keeps the CLI process, so the workflow keeps running; the adapter's workflow state is part of the core checkpoint (`#[serde(default)]` fields), so the station continues the diffs (§4.8). A resume starts a new process: the CLI does not continue the workflow; it reports it `stopped` (§2.4). The board shows the workflow stopped from the end of the old process. The agent chats stay readable after both, because the binding rows and the files stay.

### 3.2 B19

1. **What counts per session?** "Running agents" = running subagent tools + background subagents (`background_agents`) + workflow agents in the state `running`. Queued workflow agents and shell tasks do not count (§4.7).
2. **Fleet total:** the sum over the sessions on the board, which is already what the viewer may see (a member's board has only the shared sessions). Sessions of an unreachable station have the card status `unreachable` and are not counted (`web/src/board/fleet.ts:98-99`); the title of the total says so.
3. **Where?** "Agents running" with the count on each strip (it replaces "Subagents running"), and one part of the fleet header sentence: "9 at work" (sessions that work plus their running agents), on Fleet and Watch. The wall gets the number in its card data (`running_agents`) but no new UI in this track (owner question 3).
4. **Tasks separate from agents?** Yes. The strip's "Background tasks" becomes "Background commands" and counts only the tasks that are neither subagents nor workflows.

## 4. Design

### 4.1 Provider (Claude core)

New module `crates/fleetdeck-providers/src/claude/workflows.rs`. State in the core:

```rust
// claude/core.rs, ClaudeCore (new field, after `tasks`):
/// The workflow runs of `local_workflow` tasks, by task id (B12).
#[serde(default)]
pub(super) workflows: BTreeMap<String, WorkflowEntry>,

// claude/workflows.rs
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct WorkflowEntry {
    pub(super) tool_use_id: Option<String>,
    pub(super) name: Option<String>,
    pub(super) run_id: Option<String>,
    /// "running", "completed", "failed" or "stopped".
    pub(super) status: String,
    pub(super) phases: BTreeMap<u32, String>,
    pub(super) agents: BTreeMap<u32, AgentKeep>,
}
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct AgentKeep {
    /// The last emitted entry; a change of any field except the counters is an event.
    pub(super) last: fleetdeck_proto::event::WorkflowAgent,
}
```

Limits: at most 32 entries in `workflows` (when a 33rd starts, remove the oldest entry whose status is not `running`; if all run, remove the oldest); at most 1,000 agents and 100 phases per workflow (more are ignored, the CLI caps a run at 1,000 agents). Texts are cut with `truncate` (`claude/core.rs`): `name` 80 characters, phase `title` 120, `label` 120, `model` 64, `last_tool` 64, `prompt_preview`, `result_preview`, `error` 200, a state text that is not known 32.

Mapping:

1. `task_started` with `task_type == "local_workflow"` (hook in `tasks.rs:37-44`): insert an entry (`tool_use_id`, `name` = `workflow_name`, `status` = `running`) and emit `workflow.updated` with `status: "running"`, no phases, no agents, counts 0. `task.updated` stays as today (older boards read it).
2. `task_progress` (new arm in `map.rs` `on_system`, before `_`): only a frame with a `workflow_progress` array counts; a frame without it emits nothing. An unknown `task_id` gets a new entry with `status: "running"` and no name. For each `workflow_phase` with an integer `index` and a string `title`: a new or changed title goes into `phases`. For each `workflow_agent` with an integer `index`: build a `WorkflowAgent` (state mapping below); when it differs from `last` in any field except `tool_calls`, `tokens`, `last_tool`, it goes into `agents` and replaces `last` (the counters are copied in the same entry). Unknown entry types (`workflow_log`, …) are ignored. Emit one `workflow.updated` when `phases` or `agents` is not empty.
3. State mapping: `start` with a `startedAt` number → `running`; `start` without `startedAt` → `queued`; `progress` → `running`; `done` → `done`; `error` → `failed`; any other string → that string, cut to 32 characters; a missing state → `running`.
4. The Workflow tool result (`map.rs` tool result path, `:351-358`): for a node with `name == "Workflow"` and `msg.workflow_run_id` set, put `"workflow_run_id"` into `ToolCompleted.provider_refs`, set `run_id` on the entry whose `tool_use_id` is the node's tool use, and emit one `workflow.updated` with `run_id` and every agent of the entry that has an `agent_id` (a resend, so the station can bind agents that came before the run id).
5. Every `workflow.updated` after the run id is known carries `run_id`.
6. A final status of a workflow task in `on_task_frame` (`tasks.rs:58`): map `completed` → `completed`, `failed` → `failed`, `killed`, `stopped`, `cancelled` → `stopped`. If the entry's status is already final, emit nothing. Else set it, change every agent whose state is `queued` or `running` to `stopped` when the status is not `completed`, and emit one `workflow.updated` with the status and those agents.
7. `running_agents` = agents of the entry with state `running`; `agents_total` = agents of the entry. Both are absolute in every event.
8. A `task_notification` for a task id that the core does not know (the resume case, §2.4) emits only `task.updated` as today.

Frame parser (`claude/frames.rs`): `MessageFrame` gets `workflow_run_id: Option<String>`: `tool_use_result.runId` (or `toolUseResult.runId`) when it matches `^wf_[a-z0-9-]{1,64}$`, else `None`; nothing is read from text.

Permission requests (`claude/requests.rs:93-150`): `body.agent_id` that matches `[a-z0-9]{1,64}` goes into `ApprovalRequested.provider_refs` as `"agent_id"`. No other change.

Codex has no workflows: no Codex change.

### 4.2 Proto

New file `crates/fleetdeck-proto/src/event_workflow.rs`; `event.rs` gets the variant and re-exports the types.

```rust
/// A Claude workflow run changed (B12). `phases` and `agents` hold only the
/// entries that are new or changed since the last event of this task (merge
/// by `index`); the counts are absolute. Older peers decode this kind as unknown.
#[serde(rename = "workflow.updated")]
WorkflowUpdated(Box<WorkflowUpdate>),

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct WorkflowUpdate {
    pub task_id: String,
    /// The node of the Workflow tool card, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]
    pub node_id: Option<String>,
    /// The `meta.name` of the script.
    #[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]
    pub name: Option<String>,
    /// The workflow run id (`wf_…`), the folder of its agent transcripts.
    #[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]
    pub run_id: Option<String>,
    /// `running`, `completed`, `failed`, `stopped`.
    pub status: String,
    #[serde(default)]
    pub phases: Vec<WorkflowPhase>,
    #[serde(default)]
    pub agents: Vec<WorkflowAgent>,
    pub running_agents: u32,
    pub agents_total: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct WorkflowPhase { pub index: u32, pub title: String }

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct WorkflowAgent {
    pub index: u32,
    pub label: String,
    /// `queued`, `running`, `done`, `failed`, `stopped`, or the provider's text.
    pub state: String,
    // Each of the following: #[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]
    pub phase_index: Option<u32>,
    pub agent_id: Option<String>,
    pub model: Option<String>,
    pub attempt: Option<u32>,
    pub tool_calls: Option<u32>,
    pub tokens: Option<u64>,
    pub last_tool: Option<String>,
    pub prompt_preview: Option<String>,
    pub result_preview: Option<String>,
    pub error: Option<String>,
    /// Unix ms.
    pub started_at: Option<i64>,
}
```

`event_kinds.rs`: `"workflow.updated"` in `KINDS` and `kind_name`.

`SessionSummary` (`session.rs`), three new fields:

```rust
/// How many of `background_tasks` are workflows (`WORKFLOW_TASK_KINDS`, B19).
#[serde(default, skip_serializing_if = "is_zero")] #[ts(optional)]
pub background_workflows: u32,
/// The workflows that run, with their running agents (B12, at most `MAX_RUNNING_WORKFLOWS`).
#[serde(default, skip_serializing_if = "Vec::is_empty")] #[ts(optional)]
pub running_workflows: Vec<RunningWorkflow>,
/// Running subagent tools + `background_agents` + the running agents of
/// `running_workflows` (B19). Derived by the fold after every event.
#[serde(default, skip_serializing_if = "is_zero")] #[ts(optional)]
pub running_agents: u32,

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct RunningWorkflow { pub task_id: String, pub running_agents: u32, pub agents_total: u32 }

pub const WORKFLOW_TASK_KINDS: &[&str] = &["local_workflow"];
pub const MAX_RUNNING_WORKFLOWS: usize = 16;
```

The TS fields are optional, so no web fixture changes (`web/src/testing/summary.ts`).

Link (`version.rs`): `pub const WORKFLOW_AGENT: &str = "workflow_agent.v1";` in the feature list. Method `session.workflow_agent` (deck → station, scope `read`, not mutating). Params and answer in `crates/fleetdeck-proto/src/subagent.rs`:

```rust
/// `session.workflow_agent` params (B12). The station finds the transcript
/// from its own binding row of this agent; the caller never gives a path or a run id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "subagent/")]
pub struct WorkflowAgentQuery {
    pub session_id: SessionId,
    /// `[a-z0-9]{1,64}`.
    pub agent_id: String,
}
// The answer is `SubagentLog` (same limits: 300 items, 512 KiB).
```

### 4.3 Fold (`fold.rs`)

- `Event::WorkflowUpdated(u)`: when `u.status == "running"`, upsert `running_workflows` by `task_id` with `running_agents`, `agents_total` (a new entry goes last; over `MAX_RUNNING_WORKFLOWS` the first entry goes). Any other status removes the entry.
- `Event::BackgroundUpdated`: also `background_workflows` = tasks of `WORKFLOW_TASK_KINDS`; remove every `running_workflows` entry whose `task_id` is not in the new list **and** the list is empty or has no `local_workflow` (a lost final event; the CLI lists the workflow from start to end, §2.2).
- `clear_running` (`fold.rs:343-349`): also `background_workflows = 0`, `running_workflows.clear()`.
- After each fold, next to `derive_activity` (`fold.rs:26-36`): `running_agents` = running tools with `tool_kind == subagent` + `background_agents` + Σ `running_workflows.running_agents` (saturating).

### 4.4 Station: the agent's chat

- **Migration V43** (or the next free number at merge; other tracks may add one), file `crates/fleetdeck-station/src/store/workflow_agent_logs.rs`:

```sql
-- B12: one row for each workflow agent, with the run folder and the launch
-- facts of the process that reported it.
CREATE TABLE workflow_agent_logs (
    session_id    TEXT NOT NULL,
    agent_id      TEXT NOT NULL,
    run_id        TEXT NOT NULL,
    native_id     TEXT NOT NULL,
    cwd           TEXT NOT NULL,
    provider_home TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (session_id, agent_id)
) STRICT;
```

  `put_workflow_agent_log(row) -> Result<bool>` (`INSERT OR IGNORE`: the first binding stays) and `workflow_agent_log(session_id, agent_id) -> Result<Option<WorkflowAgentLogRow>>`, as `subagent_logs.rs:47-100`.
- **Binding** (`history/workflow_agent.rs`, `observe`): called in `manager/pump.rs:68` next to `subagent::observe`, with the same `LaunchFacts`. For each `Event::WorkflowUpdated` with `run_id` that matches `^wf_[a-z0-9-]{1,64}$`, for each agent with an `agent_id` of `[a-z0-9]{1,64}`: store one row with the native id of the store (the same checks as `subagent.rs:139-180`). No launch facts, no native id, an invalid id: no row (debug log).
- **Answer** (`session.workflow_agent`): the same steps as `subagent::answer` (`subagent.rs:197-252`): `agent_id` checked; unknown session → `unknown_session`; not Claude → `unavailable { unsupported_format }`; no row → `unavailable { not_found }`; the row's names checked again (absolute home, agent id, run id pattern, native id) → else `unavailable { not_trusted }`; then the history budget and one read.
- **Path:** `locate::locate_workflow_agent(home, cwd, native, run_id, agent_id, trust)`: segments `projects`, `encode_cwd(cwd)`, `native`, `subagents`, `workflows`, `run_id`, file `agent-<agent_id>.jsonl`; the same `descend` without links, `file_flags`, `check_meta(.., SUBAGENT_FILE_MAX)` as `locate_subagent` (`locate.rs:180-211`).
- **Read:** `subagent::read_log` unchanged (`parse_line_subagent` with the native id and the agent id; a record of another session or agent → `not_trusted`).
- **Intake:** `"session.workflow_agent" => workflow_agent_method(ctx, req.params).await` in `link/intake.rs:505` area; the handler is the twin of `subagent_method` (`intake.rs:382-393`).
- **Method table** (`crates/fleetdeck-link/src/methods_table.rs`, after `session.subagent`): `m("session.workflow_agent", DeckToStation, Scope::Read, Some(feature::WORKFLOW_AGENT), false)`.

### 4.5 Deck

- Route `GET /api/sessions/{station}/{session}/workflow-agents/{agent_id}` → `session_work_routes::workflow_agent`: owner only (403 `owner_only`), `plain_session`, agent id `[a-z0-9]{1,64}` (else 400 `invalid_params` "the agent id is not valid"), the `read` helper (`session_work_routes.rs:160-225`) with a new `Route { method: "session.workflow_agent", feature: feature::WORKFLOW_AGENT, unsupported: "workflow_agent_unsupported", what: … }`, the same audit as `subagent`. Answer `SubagentLog`.
- Board-API method row `sessions.workflow_agent` (GET, `Scope::Read`, not member). `methods_table_rest.rs` is at 792 lines: move its three DL-191 rows (`sessions.subagent`, `sessions.branches`, `sessions.branch_diff`, `:565-585`) with the new row into a new part `methods_table_work.rs` (`API_WORK`), joined by `super::api` like the other parts.
- `display_view.rs` `display_card`: also copy `running_agents` (a count, allowed by R13 §5D: it is the agent count of the card). Not `running_workflows` (task ids) and not `background_workflows`.

### 4.6 Board: B12

**State** (new `web/src/session/workflows.ts`):

```ts
export type WorkflowAgentView = {
  index: number; label: string; state: string; phase_index: number | null;
  agent_id: string | null; model: string | null; attempt: number | null;
  tool_calls: number | null; tokens: number | null; last_tool: string | null;
  prompt_preview: string | null; result_preview: string | null; error: string | null;
  started_at: number | null;
};
export type WorkflowView = {
  task_id: string; node_id: string | null; name: string | null; description: string | null;
  status: string;                 // running | completed | failed | stopped | the provider's text
  phases: Map<number, string>;    // index → title
  agents: Map<number, WorkflowAgentView>;
  running_agents: number; agents_total: number;
  reported: boolean;              // false: no workflow.updated came (an older station)
};
/** In event order: `task.updated` with kind `local_workflow` creates or updates a workflow
 * (description, node, status: running→running, completed→completed, failed/killed→failed,
 * stopped/cancelled→stopped); `workflow.updated` merges phases and agents by index and sets
 * status, counts, name, run node; `session.ended`, or `session.state(_changed)` lost/exited,
 * sets every running workflow to `stopped` and its queued/running agents to `stopped`. */
export function workflowStates(events: readonly EventEnvelope[]): Map<string, WorkflowView>;
/** request_id → agent_id, from `approval.requested.provider_refs.agent_id`. */
export function requestAgents(events: readonly EventEnvelope[]): Map<string, string>;
/** The agent with this id in any workflow, or null. */
export function agentById(workflows: ReadonlyMap<string, WorkflowView>, agentId: string): WorkflowAgentView | null;
/** The status chip of an agent state: running ok; queued info; failed hazard; done, stopped and other text muted. */
export function agentStatus(state: string): ShownStatus;
```

`tasks.ts:40`: keep the kind of an earlier event when the new kind is `unknown`. `hasAgents` is also true with one workflow; `runningCount` adds `running_agents` of the running workflows.

**Agents switcher** (`AgentsMenu.tsx` + new `web/src/chat/WorkflowAgents.tsx`):

- Summary: "Agents" with the pill of subagents + workflow agents (`agents_total` sums); "Commands" as today; "N running" includes running workflow agents.
- Menu order: "Main chat"; "Agents" group (subagents, as today); then one group per workflow, oldest first; then "Background commands".
- Workflow group head (`.fd-label fd-agent-group`): "Workflow · <name or description>" and the `.fd-count` of `agents_total`, then the status chip of the workflow (`running`, `done`, `failed`, `stopped`).
- Phase heads (new `.fd-agent-phase`, 12px `--dim`, indent 8px): "<index>. <title>"; agents without a phase index go under "Other agents" at the end. A phase without agents yet shows its head only.
- Agent rows (`.fd-agent-row fd-selbar`, `data-wfagent="<task_id>:<index>"`): the label, its chip; a dim second line: running → `last_tool` ("Bash"); done → the first line of `result_preview`; failed → the first line of `error`; else none.
- A workflow from an older station (`reported === false`): its head and one dim line "This station does not report the agents of a workflow."
- A choice `{ kind: 'workflow_agent'; task_id: string; index: number }` → URL `agent=wf:<task_id>:<index>` (the route keeps any text, `web/src/app/routes.ts:278-285`; node ids are `node-N`, so `wf:` never collides).

**Agent view** (new `web/src/chat/WorkflowAgentView.tsx`), in place of the chat list:

- Head: `AgentHead` with the title = label and the chip.
- Facts (`.fd-agent-facts`, a definition list): "Workflow" = name; "Phase" = "1. Alpha"; "Model" = `shortModel(model)`; "Tool calls" = `countText`; "Tokens" = `compactCount`; "Attempt" only when > 1.
- "Task": the `prompt_preview` (pre-wrap, `--dim`), absent when null (hidden-script runs withhold it).
- "Result": `result_preview`, or "Error": `error` in the hazard tone.
- Then "Show its work" (`SubagentWork` with `load = () => actions.workflowAgent(agent_id)`) for the owner, when `agent_id` is known and the action exists. No agent id yet: the line "Its work shows after it starts." A member: nothing.
- The composer is hidden, with the line "A workflow agent takes no input. Write in the main chat."
- A URL that names no known agent shows the main chat (as `agentSub` does today, `SessionView.tsx:149-152`).

**Workflow card line** (new `web/src/chat/WorkflowLine.tsx`, rendered by `ChatView` under the tool card whose `node_id` is the workflow's `node_id`): one `--dim` line: "<name> · <done> of <total> agents done · <running> running" (the running part only when > 0; "No agents yet" when total is 0), then the status word when not running: "Stopped", "Failed". No button: the Agents switcher is the way in.

**Approval line** (`ApprovalDock.tsx`): when `requestAgents` maps the request to an agent id, a `--dim` line above the options: "From workflow agent <label>" (the label from `agentById`), or "From a workflow agent" when the label is not known.

**Action** (`web/src/session/commands.ts`): `readonly workflowAgent?: ((agentId: string) => Promise<SubagentLog>) | null;` → `GET ${sessionPath}/workflow-agents/${encodeURIComponent(agentId)}`, like `subagent` (`commands.ts:339-342`).

**SessionView** (797 lines): move the agent choice logic (`SessionView.tsx:145-161`) into a new hook `web/src/session/agentChoice.ts` (`useAgentChoice`) that also resolves `wf:` choices; render the agent view through it. The net change of `SessionView.tsx` must be ≤ 0 lines. The Properties "Agents" row gets `agents` = subagents + workflow agents and `runningAgents` = the switcher's running count (`SessionView.tsx:263,771`).

### 4.7 Board: B19

New `web/src/board/agentCounts.ts`:

```ts
/** Running agents of a card: the summary's `running_agents` (new decks), else the older sum. */
export function runningAgents(s: SessionSummary): number {
  return Math.max(s.running_agents ?? 0, runningSubagents(s));
}
/** Background tasks that are neither subagents nor workflows. */
export function backgroundCommands(s: SessionSummary): number {
  return Math.max(0, s.background_tasks.length - s.background_agents - (s.background_workflows ?? 0));
}
/** The fleet total: live cards whose activity is working or background, plus the running agents of every live card. */
export function atWork(cards: readonly DeckSession[]): { sessions: number; agents: number } {
  const live = cards.filter(isLiveSession);
  return {
    sessions: live.filter((c) => c.summary.activity === 'working' || c.summary.activity === 'background').length,
    agents: live.reduce((n, c) => n + runningAgents(c.summary), 0),
  };
}
```

- **Strip** (`FleetStrip.tsx:166,270-297`): the `subagents` ico becomes `kind="agents"`, title "Agents running", `n = runningAgents(s)`, `data-running-agents`; the `background` ico becomes title "Background commands", `n = backgroundCommands(s)`, hidden at 0.
- **Sentence** (`FleetSentence.tsx`): a new optional prop `atWork?: { sessions: number; agents: number }`. When `sessions + agents > 0`, after the stations part and its period: `<span className="fd-home-work" title={…}>9 at work</span>.` Words: "1 at work" / "N at work" (`countText`). Title: "3 sessions and 6 of their agents run now. Sessions on unreachable stations are not counted." (singular "1 session", "1 of their agents"; with 0 agents: "3 sessions run now. …"). Example: "6 live sessions on 4 stations. 9 at work. 2 need you."
- **FleetHome** (`FleetHome.tsx:323-324,463-468`) and **Watch** (`WatchPage.tsx:529-533`): pass `atWork(live)` and `atWork(matching)`.
- **Properties:** §4.6.

### 4.8 Behavior after stop, takeover and resume

| Case | Claude | Fleet Deck |
| --- | --- | --- |
| Owner Stop (interrupt) | Kills the running workflows and their agents' background commands (§2.4) | `task_updated killed` → `workflow.updated status stopped` with the running/queued agents `stopped`; the fold removes the running workflow |
| Shim takeover | The process lives; frames continue | The core checkpoint holds `workflows`; diffs continue; no event is lost beyond the shim's own rules |
| Process lost or ended | The workflow dies with the process; no state file | `session.state lost/exited` or `session.ended`: the fold clears; the board sets running workflows and agents `stopped` |
| Resume | `task_notification stopped` for the old task before `init` | `task.updated` kind `unknown`; the board keeps `local_workflow` (§4.6) and the workflow stays `stopped`; agent chats stay readable (rows and files stay) |

### 4.9 Compatibility

- New station, older deck: `workflow.updated` is stored and forwarded as an unknown kind (`event_body.rs:56-70`); older boards ignore it. The older deck has no route, so no agent chat.
- Older station, new deck/board: no `workflow.updated`; the board shows the workflow group from `task.updated` with "This station does not report the agents of a workflow."; the route answers 409 `workflow_agent_unsupported` with the older-station text before any forward (feature `workflow_agent.v1` absent). `running_agents` then counts subagents only.
- A summary stored before this build reads the new fields as 0 / empty (`#[serde(default)]`). The board falls back to `runningSubagents`.
- The checkpoint stays version 1: the new core field is `#[serde(default)]`; an older station that restores a newer checkpoint ignores the unknown field (serde default behavior of `ClaudeCore`, no `deny_unknown_fields`).

### 4.10 Errors and edge cases

- A `task_progress` with a `workflow_progress` that is not an array, an entry without an integer `index`, or an agent with a non-string `label`: the entry is skipped; the frame is not an unknown frame.
- Two workflows in one session: each has its own entry, group and count.
- `workflow()` sub-runs (nested named workflows): their agents come in the same `workflow_progress` with their own indexes; they show in the same group (no nesting in this design).
- Hidden-script runs: phase titles "phase N", no prompt preview, "error details withheld for this workflow" as error: shown as they come.
- Retries: `attempt` > 1 shows "Attempt 2"; the same index keeps one row.
- An agent's own background commands (`owned_by_subagent: true`, `local_bash`) stay in "Background commands" with no card, as B6 does today.
- An agent id that the station never bound (a run before this build, or no launch facts): "Show its work" answers `unavailable { not_found }` with the history text that exists.
- A transcript that grows during the read: only the captured extent is read; "Refresh" reads again.
- A very large run (1,000 agents): about 3 events per agent (queued/running/done) with only the changed entries: about 3,000 entry records of ≤ 1 KB; the summary keeps only counts.

### 4.11 Out of scope

- Codex (no workflows). Nested display of sub-workflows. Live streaming of an agent's chat (only on request, as DL-191). Stopping one workflow or one agent from the board (Claude has `stop_task`; a later item). The wall UI for the total (B17). The coordinator tools `list_sessions` / `fleet_view` showing agent counts. Cost per workflow (B14). Reading `<runId>.json`, the journal or the task output file.

## 5. Risk class: high

Reasons: a new station read path to provider files (§4.4), an additive station migration (V43), a new link method and a public event kind (`workflow.updated`) that boards and decks read.

Threat notes:

- **Path choice by a caller.** The caller names only a session and an agent id. The station serves only an agent id that its own stream reported for that session (binding row, `PRIMARY KEY (session_id, agent_id)`); the run id and the home come from the row, never from the request. Run id and agent id are checked by pattern before they become path segments, at bind time and again at read time.
- **Links and swaps.** Every segment is opened with `descend` without following links; the file passes `check_meta` (owner, one hard link, not private, ≤ 64 MiB); one read of the captured extent.
- **Foreign records.** Every record must name the session's native id and the agent id; else the whole answer is `not_trusted`.
- **Who reads.** The route is owner only (as DL-191); a member and a token get 403; a display has no route.
- **Event content.** `prompt_preview`, `result_preview` and `error` are model text (≤ 200 characters each) in the event log, as tool inputs and messages are today. A member of a shared session sees them like the other events of the session (owner question 4). The display card copies only the count.
- **Volume.** Diffs and caps (§4.1) bound the event rate and size; the summary keeps at most 16 running workflows.
- **Migration.** Additive `CREATE TABLE`; no data rewrite; no downgrade issue beyond the usual (an older binary ignores the table).

## 6. Build plan

Every heavy command runs as `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. Every task also runs the guards: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`. UI tasks also run `sh tools/design-check.sh` and follow `docs/v2/DESIGN-SYSTEM.md` (skill `fleet-design-system`).

Order: T1 → (T2 ∥ T3 ∥ T4); T1 → (T5 ∥ T6); T5b after T5 and T4. The adversary DESIGN REVIEW (§5) gates T3 and T4.

### T1 (backend): proto: event, summary, fold, query, feature — about 450 lines

- **Owned files:** `crates/fleetdeck-proto/src/event_workflow.rs` (new), `event.rs`, `event_kinds.rs`, `session.rs`, `fold.rs`, `fold_tests_workflow.rs` (new, `#[cfg(test)] #[path]` from `fold.rs`), `subagent.rs`, `version.rs`, `ts.rs`, `event_tests.rs`, `web/src/gen/**` (regenerated by `make ts-types` via heavy.sh).
- **Changes:** §4.2 types, variant, kinds, summary fields and `RunningWorkflow`, `WORKFLOW_TASK_KINDS`, `MAX_RUNNING_WORKFLOWS`, `WorkflowAgentQuery`, feature `WORKFLOW_AGENT` in the list; §4.3 fold; TS export of the new types.
- **Tests:**
  - `workflow_update_round_trips_and_skips_empty_options` (serde: absent options are not written; an event without `phases`/`agents` reads as empty).
  - `an_older_reader_keeps_workflow_updated_as_unknown` (an `EventBody` from JSON with an unknown extra field still parses; the kind is in `KINDS`).
  - `a_running_workflow_adds_its_running_agents` (two workflows: `running_agents` = sum + `background_agents` + one running subagent tool).
  - `a_final_status_removes_the_workflow` (`completed`, `failed`, `stopped`).
  - `background_updated_counts_workflows_and_drops_a_lost_workflow`.
  - `the_end_of_the_process_clears_the_workflows` (`session.ended`, `session.state lost`).
  - `at_most_16_running_workflows`.
  - `workflow_agent_query_refuses_unknown_fields`.
- **Acceptance:** `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(workflow) | test(fold) | binary(ts_drift) | test(event)'`; `cargo check -p fleetdeck-providers -p fleetdeck-station -p fleetdeck-deck`; the guards.

### T2 (backend): Claude adapter — about 550 lines

- **Depends on:** T1.
- **Owned files:** `crates/fleetdeck-providers/src/claude/workflows.rs` (new), `claude/tests_workflows.rs` (new), `claude/tasks.rs`, `claude/map.rs` (≤ 15 added lines), `claude/frames.rs`, `claude/core.rs` (field + `new`), `claude/requests.rs`, `claude/mod.rs`, `spikes/s1-claude/fixtures/workflow/*.jsonl` (new: copy `~/fleetdeck-briefs/backlog/workflow-agents/fixtures/*.jsonl` verbatim; not under `fixtures/rust`, so the replay harness does not take them).
- **Changes:** §4.1.
- **Tests** (feed the `out` frames of a fixture into `running_core()`-style cores; `tests_tasks.rs:17-40` shows the pattern):
  - `two_phases_give_phases_agents_and_the_run_id` (`workflow_two_phases.jsonl`: first event `running` with name `bl-tiny`; the run id `wf_0918c8f1-e62` in `tool.completed.provider_refs.workflow_run_id` and in later events; phases 1 Alpha, 2 Beta; agent 1 `running` then `done` with result `alpha`; final `completed`; `running_agents` 0 at the end).
  - `fanout_reports_parallel_agents_and_only_changes` (`workflow_fanout.jsonl`: two agents `running` at once, `running_agents == 2`; a frame without `workflow_progress` emits no `workflow.updated`; a frame that repeats a state emits nothing).
  - `an_interrupt_stops_the_running_agents` (`workflow_interrupt.jsonl`: one `workflow.updated status stopped` with both agents `stopped`; `task_notification stopped` after `killed` emits no second one).
  - `an_agent_permission_names_its_agent` (`workflow_agent_approval.jsonl`: `approval.requested.provider_refs.agent_id == "a3f92b5c63d2c47ee"`).
  - `a_resume_notification_of_an_unknown_task_emits_no_workflow_event` (`workflow_resume_orphan.jsonl`).
  - `a_queued_agent_is_not_running` (synthetic `start` without `startedAt`).
  - `bad_entries_are_skipped` (no index, non-string label, non-array list).
  - `the_run_id_is_read_only_from_the_structured_result` (text "Run ID: wf_x" without `tool_use_result` gives none; `runId: "../x"` gives none).
  - `the_checkpoint_keeps_the_workflows` (checkpoint, restore, next progress frame diffs against the restored state).
  - `limits_cut_texts_and_cap_agents` (label > 120 characters cut; agent 1,001 ignored; a 33rd workflow drops the oldest ended one).
- **Acceptance:** `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(workflow) | test(task) | binary(claude_core_replay)'`; the guards.

### T3 (backend): station binding, migration, method — about 500 lines

- **Depends on:** T1; adversary DESIGN REVIEW pass.
- **Owned files:** `crates/fleetdeck-station/src/store/workflow_agent_logs.rs` (new), `store/migrations.rs`, `store/mod.rs` (one `pub mod` line), `history/workflow_agent.rs` (new), `history/tests_workflow_agent.rs` (new), `history/locate.rs`, `history/mod.rs` (one `pub mod`), `manager/pump.rs` (one call), `link/intake.rs` (≤ 16 lines), `crates/fleetdeck-link/src/methods_table.rs`.
- **Changes:** §4.4.
- **Tests** (the `tests_subagent.rs` rig):
  - `a_workflow_update_with_a_run_id_stores_one_row_per_agent`; `no_row_without_run_id_agent_id_native_id_or_launch_facts`; `the_first_binding_of_an_agent_stays`.
  - `it_reads_the_agent_transcript_of_the_run_folder` (a file under `subagents/workflows/wf_…/`; items and tool results paired).
  - `an_unbound_agent_is_not_found`; `a_codex_session_is_unsupported`.
  - `a_link_on_the_run_folder_or_the_file_is_not_trusted`; `a_record_of_another_agent_is_not_trusted`; `a_bad_run_id_in_a_row_is_not_trusted`.
  - `bytes_appended_after_open_are_not_read`.
  - `the_method_is_in_the_table_with_its_feature` (link crate).
  - Migration: the existing `the_migrations_are_valid` (`store/tests.rs:70`) passes with V43; new `a_v42_database_migrates_to_v43_with_the_workflow_agent_table`.
- **Acceptance:** `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(workflow_agent) | test(subagent) | test(migrations) | test(v43)'`; `cargo nextest run -p fleetdeck-link --no-fail-fast -E 'test(method)'`; the guards.

### T4 (backend): deck route and display — about 250 lines

- **Depends on:** T1; adversary DESIGN REVIEW pass.
- **Owned files:** `crates/fleetdeck-deck/src/http/session_work_routes.rs`, `http/session_work_workflow_tests.rs` (new, `#[cfg(test)] #[path]`), `http/mod.rs` (one route), `http/methods_table_rest.rs`, `http/methods_table_work.rs` (new), the file that joins the parts (`super::api`), `http/display_view.rs` and its tests.
- **Changes:** §4.5.
- **Tests:** `a_member_or_token_gets_owner_only`; `a_bad_agent_id_is_invalid_params`; `an_older_station_gets_409_workflow_agent_unsupported_without_a_forward`; `the_answer_of_the_station_is_passed_through`; `the_failure_is_audited_with_the_code_only`; `the_method_table_has_sessions_workflow_agent`; `the_display_card_copies_running_agents_only`.
- **Acceptance:** `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(workflow_agent) | test(display) | test(methods_table)'`; the guards.

### T5 (ux): workflow state, Agents switcher, agent view — about 600 lines

- **Depends on:** T1.
- **Owned files:** `web/src/session/workflows.ts` (new), `web/src/session/workflows.test.ts` (new), `web/src/session/tasks.ts`, `web/src/session/tasks.test.ts`, `web/src/chat/AgentsMenu.tsx`, `web/src/chat/AgentsMenu.test.tsx`, `web/src/chat/WorkflowAgents.tsx` (new), `web/src/chat/WorkflowAgentView.tsx` (new), `web/src/chat/workflowAgents.test.tsx` (new), `web/src/session/agentChoice.ts` (new), `web/src/session/SessionView.tsx` (net ≤ 0 lines), `web/src/styles/chat.css`.
- **Changes:** §4.6 except the card line, the approval line and the action (T5b). In this task the agent view shows the facts and no "Show its work".
- **Tests:** `workflowStates merges phases and agents by index`; `an older station gives a reported:false workflow from task.updated`; `the end of the process stops running workflows and agents`; `an unknown task kind keeps local_workflow`; `the switcher lists workflow → phase → agent with chips and notes`; `queued is info, failed is hazard`; `agents without a phase go under Other agents`; `choosing an agent sets agent=wf:<task>:<index> and shows the facts`; `the composer line says a workflow agent takes no input`; `an unknown wf: choice shows the main chat`; `the running count includes running workflow agents`.
- **Acceptance:** `cd web && bun run test workflows AgentsMenu tasks workflowAgents session` (via heavy.sh); `sh tools/design-check.sh`; the guards.

### T5b (ux): card line, approval line, "Show its work" — about 200 lines

- **Depends on:** T5, T4.
- **Owned files:** `web/src/chat/WorkflowLine.tsx` (new), `web/src/chat/workflowLine.test.tsx` (new), `web/src/chat/ChatView.tsx` (≤ 20 lines), `web/src/session/ApprovalDock.tsx`, `web/src/session/approvalDock.test.tsx`, `web/src/session/commands.ts`, `web/src/chat/WorkflowAgentView.tsx`, `web/src/session/agentChoice.ts`.
- **Changes:** §4.6 card line, approval line, `workflowAgent` action, "Show its work" in the agent view.
- **Tests:** `the card line counts done and running agents`; `a stopped workflow says Stopped`; `an approval of a workflow agent says From workflow agent <label>`; `Show its work calls GET …/workflow-agents/<id>`; `no agent id yet says Its work shows after it starts.`; `a member sees no Show its work`.
- **Acceptance:** `cd web && bun run test workflowLine approvalDock workflowAgents commands` (via heavy.sh); `sh tools/design-check.sh`; the guards.

### T6 (ux): B19 counts — about 300 lines

- **Depends on:** T1.
- **Owned files:** `web/src/board/agentCounts.ts` (new), `web/src/board/agentCounts.test.ts` (new), `web/src/board/FleetStrip.tsx`, the strip tests that name "Subagents running" or "Background tasks", `web/src/views/FleetSentence.tsx`, `web/src/app/FleetHome.tsx`, `web/src/watch/WatchPage.tsx`, `web/src/styles/home.css`, `docs/v2/DESIGN-SYSTEM.md` (§4.1 strip counters: "agents" and "background commands"; §4.4 the "N at work" part; §4.5 the Agents switcher workflow groups; one expansion-log row).
- **Changes:** §4.7.
- **Tests:** `runningAgents prefers running_agents and falls back`; `backgroundCommands leaves out agents and workflows`; `atWork counts working and background sessions and the agents of live cards, not unreachable ones`; `the strip shows Agents running 3 and Background commands 1`; `the sentence says 9 at work with its title`; `no at-work part at 0`; `Watch passes the matching cards`.
- **Acceptance:** `cd web && bun run test agentCounts FleetStrip FleetSentence fleet watch` (via heavy.sh); `sh tools/design-check.sh`; the guards.

## 7. Decisions made in this draft

1. **Source of progress:** `task_progress.workflow_progress` in the stream. Rejected: polling `workflows/<runId>.json` or the journal (written at the end or only with results; no states).
2. **One new event `workflow.updated` with changed entries and absolute counts.** Rejected: one `task.updated` per agent (no phases, and B6 code would treat them as tasks); a full snapshot per event (up to 1,000 agents each time).
3. **Agent states normalized** to `queued`, `running`, `done`, `failed`, `stopped` in the adapter, other text passed through. Rejected: the CLI's `start`/`progress`/`error` words on the board.
4. **Counters ride along, they do not trigger an event** (`tokens`, `tool_calls`, `last_tool`). Rejected: an event per counter change (hundreds per agent).
5. **The summary keeps `running_workflows` (≤ 16), `background_workflows` and a derived `running_agents`.** Rejected: a board-only count from events (the fleet board has summaries only); a session total carried in each event (couples workflows).
6. **New link method `session.workflow_agent` (feature `workflow_agent.v1`) and a new binding table keyed by `(session_id, agent_id)`.** Rejected: a field on `SubagentQuery` (`deny_unknown_fields` and the DL-191 contract); reuse of `subagent_logs` with a synthetic tool use id (no run id column); no binding row (the agent id would be request-chosen).
7. **The run id comes only from `tool_use_result.runId`** with the `wf_` pattern. Rejected: the `run_id` of `task_started` (a CLI task run id, §2.2) and any text.
8. **The binding needs the run id; the core resends the agents with ids once the run id is known.**
9. **Owner only for the agent chat**, as DL-191.
10. **Agents switcher groups workflow → phase → agent; the URL is `agent=wf:<task_id>:<index>`.** Rejected: a separate Workflows tab (the owner asked for "the same switch"); the agent id in the URL (absent while queued).
11. **The agent view shows facts and "Show its work" on request**, no live chat. Rejected: streaming the transcript file (no live frames exist; a file watcher is a new read path).
12. **The Workflow card gets a one-line status, no button.**
13. **An agent's approval is attributed through `provider_refs.agent_id`, read by the board from the timeline.** Rejected: a new `OpenRequest` field (7 struct literals in tests, and the summary does not need it).
14. **`tasks.ts` keeps a known kind over `unknown`.**
15. **B19 count = running subagents + workflow agents in `running`**; queued agents and shell tasks do not count.
16. **Fleet total = working or background live sessions + their running agents, shown as "N at work"** in the header sentence of Fleet and Watch; unreachable stations not counted, said in the title. Rejected: agents only (the owner said "the sessions plus their agents"); a separate stat tile (the header is one sentence, DESIGN-SYSTEM §4.3).
17. **Strip: "Agents running" replaces "Subagents running"; "Background tasks" becomes "Background commands" without agents and workflows.**
18. **The display card copies `running_agents` only;** no wall UI in this track.
19. **Stop keeps Claude's behavior** (an interrupt kills the workflows); the board shows them stopped. Rejected: a different stop path for workflows (Claude decides; `stop_task` per task is a later item).
20. **Fixtures go to `spikes/s1-claude/fixtures/workflow/`, out-only**, not to `fixtures/rust/` (the replay harness needs host commands).
21. **Migration number V43 or the next free one at merge.**

## 8. Owner questions (taste or policy; the build goes on with the default)

1. **The fleet total:** "9 at work" counts the sessions that work plus their running agents. Do you want agents only ("6 agents running")? **Default:** sessions plus agents, "N at work", the split in its title.
2. **Stop and workflows:** Claude's interrupt also ends every running workflow of the session. Should Stop say so when a workflow runs ("Stop also ends 1 running workflow.")? **Default:** no change in this track; note it for a later item with a per-workflow stop.
3. **The wall:** show the "at work" total on the wall? **Default:** not in this track; B17 decides (the number is already in the display card).
4. **Shared sessions:** a member sees the workflow agents' task and result previews (≤ 200 characters), as they see tool inputs today. **Default:** yes, the same as other session events; the agent chat ("Show its work") stays owner only.

## 9. Side findings for other tracks

- **B14 (cost):** the growth of the process total after a workflow includes the workflow agents (`rec1`: modelUsage cache creation 75,487 → 129,006 over a turn of 7,725). A workflow that ends after the last turn is counted only at the next `result`. `subagent_stats` does not count workflow agents.
- **B21 (first turn after resume):** a resumed process sends a `result` with `num_turns: 0` and cost 0 before the first real turn when a background task was cut (`rec6`). Check how the core binds that frame.
- **B2:** the turn that a workflow notification starts is still a provider turn ("provider turn" on the dev1 session).

## Decision log entries (to append)

## DL-420: Workflow progress from the Claude stream (B12, design `docs/v2/design/workflow-agents.md`)

- Context: a Claude `Workflow` run showed only a "done" tool card and one background task. Recordings (Claude 2.1.308) show one `local_workflow` task and `task_progress.workflow_progress` lists of phases and agents; no agent frames in the stream.
- Decision: the Claude adapter keeps a state per workflow (checkpointed) and emits `workflow.updated { task_id, node_id?, name?, run_id?, status, phases[], agents[], running_agents, agents_total }` with only the changed phases and agents; states `queued`, `running`, `done`, `failed`, `stopped`; counters ride along. The run id comes only from `tool_use_result.runId` (`wf_…`). Permission requests of a workflow agent carry `provider_refs.agent_id`.

## DL-421: Agent counts in the session summary (B12, B19)

- Decision: `SessionSummary` gets `running_workflows` (at most 16, `{task_id, running_agents, agents_total}`), `background_workflows` and a derived `running_agents` (running subagent tools + `background_agents` + running workflow agents). All `#[serde(default)]` and optional in TypeScript. The display card copies `running_agents` only.

## DL-422: The chat of a workflow agent (B12)

- Decision: link method `session.workflow_agent` (feature `workflow_agent.v1`, scope `read`) with `{session_id, agent_id}`; the station serves only an agent that its own stream bound (station migration V43 `workflow_agent_logs`, key `(session_id, agent_id)`, with run id and launch facts) and reads `projects/<cwd>/<native>/subagents/workflows/<run_id>/agent-<agent_id>.jsonl` with the DL-191 checks and parser. Deck route `GET /api/sessions/{station}/{session}/workflow-agents/{agent_id}`, owner only. Risk high: adversary DESIGN REVIEW before the station and deck tasks.

## DL-423: Workflows in the Agents switcher (B12)

- Decision: one group per workflow (head with name, count and status), phase heads, agent rows with chips; an agent view with its facts and "Show its work"; the URL `agent=wf:<task_id>:<index>`; a one-line status under the Workflow card; "From workflow agent <label>" on its approvals. An older station shows the workflow with "This station does not report the agents of a workflow."

## DL-424: Agents in the fleet view (B19)

- Decision: the strip shows "Agents running" (subagents + running workflow agents) and "Background commands" (tasks that are neither); the header sentence of Fleet and Watch adds "N at work" (working or background live sessions plus their running agents; unreachable stations not counted, said in the title). Queued agents and shell tasks do not count.

## DL-425: Workflows after stop, takeover and resume (B12)

- Facts: Claude's interrupt kills running workflows; a resumed process reports an unfinished workflow `stopped` and does not continue it; a shim takeover keeps it running.
- Decision: the adapter marks the running and queued agents `stopped` at a final non-completed status; the board stops running workflows at the end of the process and keeps the `local_workflow` kind over an `unknown` notification; agent chats stay readable from their binding rows.
