# Workflows and agents in the session and the fleet view (B12, B19)

- **Status:** Draft, revision 2 (2026-10-23), after the adversary DESIGN REVIEW round 1 (`~/fleetdeck-briefs/backlog/workflow-agents/adv-design-r1.md`; changes in §Revision 2). Risk: **high** (§5): a new station read path of provider files, an additive station migration, two new link methods and a new public event kind. Adversary DESIGN REVIEW before the build of T3a, T3b and T4; T1, T2, T5, T6 can start after the architect accepts §4.
- **Owner requests:** B12 "Workflows (ultracode) show nothing in the session" and B19 "The fleet view shows the agents too: per session and in total, with ultracode workflows" (`docs/v2/BACKLOG.md:105-118`, `:228-243`).
- **Base:** `v2` at `497b2fbc`. Every `file:line` below is on that commit (revision 2 facts checked on `00055138`; the lines are the same).
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
- The core keeps tasks in `ClaudeCore.tasks` (`claude/core.rs:151-161`, `:208-211`). The whole core is the shim checkpoint (`claude/checkpoint.rs:1-8`), so new fields with `#[serde(default)]` survive a shim takeover. The checkpoint is written with each committed output batch (`station/src/store/shims.rs:652-664`), so its size counts.
- The board's `taskStates` overwrites the kind of a task with every event (`web/src/session/tasks.ts:40`). An event with `task_kind: "unknown"` (a notification of a task that the current process did not start, §2.4) erases `local_workflow`.
- The Agents switcher lists subagents and `local_bash` tasks only (`web/src/chat/AgentsMenu.tsx:153-250`; `web/src/session/tasks.ts:105-129`).
- DL-191 "Show its work": the station stores a binding row per `Agent`/`Task` tool use (`crates/fleetdeck-station/src/history/subagent.rs:125-185`, table V25 `crates/fleetdeck-station/src/store/subagent_logs.rs:13-26`), opens `projects/<cwd>/<native>/subagents/agent-<id>.jsonl` without links (`history/locate.rs:180-211`), and accepts only records that name the native session and the agent (`history/parse.rs:111-135`).
- **Two hosts call the binding hook.** A direct session calls `history::subagent::observe` in `manager/pump.rs:68`. A shim session calls it in its own observer, `manager/shim_target.rs:694-744` (hook at `:732`). Linux uses shims by default (`crates/fleetdeck-core/src/config.rs:96-105`).
- **An adopted shim session has no launch facts.** After a station restart, `SessionTarget::adopt` passes `launch: None` to the observer (`shim_target.rs:167-171`). `LaunchFacts` (`history/subagent.rs:47-69`) is the absolute provider home and the cwd, computed from the policy at launch (`manager/target.rs:285-289`). Today an adopted session binds no subagent.
- **The station keeps projections in the event transaction.** `store/log.rs:94-193` (`append_at`) folds the summary and calls `requests::observe`, `effects::observe` and `shell_observe::observe` in the same transaction as the event row. Every event of both hosts goes through it.
- **One event is one control frame.** The station stream sends each event whole (`station/src/link/stream.rs:273-298`) and returns the error of a frame that does not encode. A control frame has at most 1,048,576 bytes (`crates/fleetdeck-proto/src/link.rs:17`; check in `crates/fleetdeck-link/src/control.rs:232-243`). One event over the limit stops the replication of the whole station.
- **Timelines are short.** The deck keeps 500 timeline rows per session (`crates/fleetdeck-deck/src/store/read_model.rs:31`, deletion `:129-137`). The browser keeps the last 1,000 events of a timeline (`web/src/runtime/state.ts:225`). A view that is computed from timeline events alone loses the start of a long workflow.
- **Interrupt needs a turn.** The board enables Interrupt only with a `current_run` (`web/src/session/controller.ts:33,45`). The station refuses an interrupt without a turn or a run, "no turn runs" (`station/src/manager/decide.rs:188-205`). The Claude core refuses it when the core is quiet (`claude/core.rs:432-441`, `ProviderError::NoTurn`).
- **The schema is forward only.** A binary opens a newer database with `DbError::TooNew` and stops (`crates/fleetdeck-core/src/db.rs:220-229`). Before a migration, the station writes `<data>/backups/station-v<from>-to-v<to>-<ms>.db` (`crates/fleetdeck-core/src/db_backup.rs:1-7,70`; the newest 3 stay).
- Size limits (rule: fewer than 800 lines, `tools/module-size.sh`): `web/src/session/SessionView.tsx` 797, `crates/fleetdeck-station/src/history/mod.rs` 798, `crates/fleetdeck-station/src/manager/mod.rs` 797, `crates/fleetdeck-deck/src/http/methods_table_rest.rs` 792, `crates/fleetdeck-station/src/manager/shim_target.rs` 791, `crates/fleetdeck-station/src/link/intake.rs` 768, `claude/map.rs` 743, `web/src/chat/ChatView.tsx` 715, `crates/fleetdeck-station/src/store/mod.rs` 709, `claude/core.rs` 685. The build plan (§6) puts new code in new files and adds no module line to `history/mod.rs` or `manager/mod.rs`.
- The last station migration on the base is V43 (`store/migrations.rs:643`, the DL-402 fix). This track uses **V45** (architect numbering, §Architect decisions).
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

- **Interrupt** (`rec4`): an `interrupt` control request, sent after the main turn had ended, killed the running workflow: `task_updated` `killed`, `task_notification` `stopped` (summary = the description), and the agents' own background Bash tasks (`owned_by_subagent: true`, `local_bash`) were killed too. No final `workflow_progress` came: the agents stay `start`/`progress` in the stream. **The recorder wrote this request straight to the CLI.** Fleet Deck does not send an interrupt between turns: three guards refuse it (§1.3). §4.8 gives the Fleet Deck behavior.
- **Process end** (`rec5`): the CLI process ended during a run. No `<runId>.json` was written; the journal has only the finished agents.
- **Resume** (`rec6`): the first frame of `claude --resume` is `task_notification` `{task_id: "w9hw2q22t", tool_use_id, status: "stopped", output_file: "", summary: "Background workflow \"bl-orphan\" (run wf_78e3fae0-176) didn't finish before the previous session ended"}`, before `init`, then a `result` with `num_turns: 0` and cost 0, then the real turn. The new process does not restart the workflow.
- **Permission of an agent** (`rec3`): a workflow agent's tool asks with a normal `can_use_tool` that adds `agent_id: "a3f92b5c63d2c47ee"` and the agent's `tool_use_id` (unknown to the main stream).

### 2.5 The gap in Fleet Deck (dev1, `rec-fleetdeck/`)

`read_session` of the dev1 session shows: `tool.started` "Workflow" (`other`), `approval.requested` "Workflow", `background.updated [local_workflow]`, `task.updated local_workflow running`, `tool.completed ok`, `task.updated completed` (twice), `background.updated []`, then a provider turn (`run.started origin provider`, "provider turn") with the result. No phase, no agent, no count. The usage of the provider turn grew by $0.40 for 18 output tokens: the growth of the process total includes the workflow agents (a fact for B14).

## 3. Answers to the questions of the backlog

### 3.1 B12

1. **Which frames?** One task per workflow (`task_type: local_workflow`, `task_started`/`task_updated`/`task_notification`), plus `task_progress` frames whose `workflow_progress` list carries the phases and the agents with their states (§2.2). There is no task per agent and no progress frame per agent.
2. **Are the agents in the session transcript?** No. Each agent has its own transcript in `subagents/workflows/<runId>/agent-<agentId>.jsonl` (§2.3). Its records pass the DL-191 checks unchanged. The station reads it like "Show its work", with the run id as one more folder (§4.4).
3. **A group in the switcher or a separate view?** A group in the Agents switcher: workflow → phase → agent (§4.6). An agent row opens an agent view with the agent's facts and "Show its work". The board gets the list from a station projection (§4.3a, §4.4), not only from the timeline, because the timelines are short (§1.3).
4. **Shim takeover and resume?** A takeover keeps the CLI process, so the workflow keeps running; the adapter's workflow state is part of the core checkpoint (`#[serde(default)]` fields), so the station continues the diffs (§4.8). The station also stores the launch facts of each shim process and reads them again at the takeover, so agents that start after a station restart get binding rows (§4.4). A resume starts a new process: the CLI does not continue the workflow; it reports it `stopped` (§2.4). The board shows the workflow stopped from the end of the old process. The agent chats stay readable after both, because the binding rows and the files stay.

### 3.2 B19

1. **What counts per session?** "Running agents" = running subagent tools + background subagents (`background_agents`) + workflow agents in the state `running`. Queued workflow agents and shell tasks do not count (§4.7). When a session runs more workflows than Fleet Deck follows (§4.1), the count is a lower bound and the board shows "N+" (§4.7).
2. **Fleet total:** the sum over the sessions on the board, which is already what the viewer may see (a member's board has only the shared sessions). Sessions of an unreachable station have the card status `unreachable` and are not counted (`web/src/board/fleet.ts:98-99`); the title of the total says so.
3. **Where?** "Agents running" with the count on each strip (it replaces "Subagents running"), and one part of the fleet header sentence: "9 at work" (sessions that work plus their running agents), on Fleet and Watch. The wall gets the number in its card data (`running_agents`) but no new UI in this track (owner question 3).
4. **Tasks separate from agents?** Yes. The strip's "Background tasks" becomes "Background commands" and counts only the tasks that are neither subagents nor workflows.

## 4. Design

### 4.1 Provider (Claude core)

New module `crates/fleetdeck-providers/src/claude/workflows.rs`. State in the core:

```rust
// claude/core.rs, ClaudeCore (new fields, after `tasks`):
/// The workflow runs of `local_workflow` tasks, by task id (B12).
#[serde(default)]
pub(super) workflows: BTreeMap<String, WorkflowEntry>,
/// Running workflows that the core does not follow (§4.1 limits), oldest first.
#[serde(default)]
pub(super) untracked_workflows: VecDeque<String>,
/// The start counter of `WorkflowEntry::started`.
#[serde(default)]
pub(super) next_workflow: u64,

// claude/workflows.rs
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
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct AgentKeep {
    /// The normalized state of the last emitted entry (`queued`, `running`, …).
    pub(super) state: String,
    /// FNV-1a 64 of `serde_json::to_vec` of the last emitted `WorkflowAgent` with
    /// `tool_calls`, `tokens` and `last_tool` set to `None`. A different digest is a change.
    pub(super) digest: u64,
}
```

The core keeps a digest, not the entry, so the checkpoint stays small (about 50 bytes per agent).

Limits:

- **Followed workflows.** At most `MAX_TRACKED_WORKFLOWS` (64, proto) entries with status `running`. The core never removes a running entry; only its final status or the end of the process ends it. At most `MAX_ENDED_WORKFLOWS` (32, `workflows.rs`) entries with a final status: when a 33rd ends, the ended entry with the smallest `started` goes.
- **Untracked workflows.** A `local_workflow` task that starts while 64 entries run gets no entry. Its id goes to the back of `untracked_workflows` (at most 1,024 ids; at the cap the front id goes), and the core emits one `workflow.updated { status: "running", untracked: true }` with no phases, no agents and counts 0. Its progress frames emit nothing. Its final status (rule 6) removes its id and emits one `workflow.updated { status: <final>, untracked: true }`. It stays untracked when a slot becomes free.
- **Ids.** A task id that does not match `^[A-Za-z0-9_-]{1,64}$` gets no entry and no `workflow.updated` (debug log). An `agentId` that does not match `^[a-z0-9]{1,64}$` becomes `None`.
- **Per workflow:** at most 1,000 agents and 100 phases (a new index beyond is ignored; the CLI caps a run at 1,000 agents).
- **Texts** are cut with `truncate` (`claude/core.rs`): `name` 80 characters, phase `title` 120, `label` 120, `model` 64, `last_tool` 64, `prompt_preview`, `result_preview`, `error` 200, a state text that is not known 32.
- **Event size.** One `workflow.updated` has at most `WORKFLOW_EVENT_BUDGET` (256 KiB) of JSON (rule 9). With the text caps, one agent entry has at most about 7 KiB of JSON (944 characters, each at most 6 bytes as a JSON escape, plus the keys), and 100 phases at most about 75 KiB. The envelope and the `event` control frame add less than 2 KiB. So every event stays far below the 1 MiB frame limit (§1.3).

Mapping:

1. `task_started` with `task_type == "local_workflow"` (hook in `tasks.rs:37-44`): with fewer than 64 running entries, insert an entry (`tool_use_id`, `name` = `workflow_name`, `status` = `running`, `started` = `next_workflow`, then `next_workflow += 1`) and emit `workflow.updated` with `status: "running"`, no phases, no agents, counts 0. Else the untracked path (Limits). `task.updated` stays as today (older boards read it).
2. `task_progress` (new arm in `map.rs` `on_system`, before `_`): only a frame with a `workflow_progress` array counts; a frame without it emits nothing. A frame for an entry whose status is final emits nothing (no running event after the end). A frame for an id in `untracked_workflows` emits nothing. An unknown `task_id` gets a new entry with `status: "running"` and no name when fewer than 64 run; else it takes the untracked path. For each `workflow_phase` with an integer `index` and a string `title`: a new or changed title goes into `phases`. For each `workflow_agent` with an integer `index`: build the full `WorkflowAgent` (state mapping below, with the current counters); when its digest differs from `AgentKeep.digest` (or the index is new), it is a changed agent, and `AgentKeep` takes its state and digest. Unknown entry types (`workflow_log`, …) are ignored. When phases or agents changed, emit them with rule 9.
3. State mapping: `start` with a `startedAt` number → `running`; `start` without `startedAt` → `queued`; `progress` → `running`; `done` → `done`; `error` → `failed`; any other string → that string, cut to 32 characters; a missing state → `running`.
4. The Workflow tool result (`map.rs` tool result path, `:351-358`): for a node with `name == "Workflow"` and `msg.workflow_run_id` set, put `"workflow_run_id"` into `ToolCompleted.provider_refs`, set `run_id` on the entry whose `tool_use_id` is the node's tool use, and emit one `workflow.updated` with `run_id`, no phases and no agents. The station binds the agents that it already has from its projection (§4.4); the core sends no agent again.
5. Every `workflow.updated` after the run id is known carries `run_id`.
6. A final status of a workflow task in `on_task_frame` (`tasks.rs:58`): map `completed` → `completed`, `failed` → `failed`, `killed`, `stopped`, `cancelled` → `stopped`. An untracked id: see Limits. If the entry's status is already final, emit nothing. Else set it, set the `AgentKeep.state` of every `queued` or `running` agent to `stopped`, and emit one `workflow.updated` with the status, `running_agents: 0`, `agents_total`, no phases and no agents. The receivers stop the queued and running agents themselves (§4.3a rule R2), so the event stays small.
7. `running_agents` = agents of the entry with state `running` (0 at a final status); `agents_total` = agents of the entry. Both are absolute in every event.
8. A `task_notification` for a task id that the core does not know (the resume case, §2.4) emits only `task.updated` as today.
9. **Chunks.** The changes of one frame go out as one or more `workflow.updated` events. Every event of the frame carries the same `task_id`, `node_id`, `name`, `run_id`, `status`, `untracked`, `running_agents` and `agents_total` (the state after the whole frame). The first event carries every changed phase. The changed agents follow in index order: an agent goes into the current event while the size stays at most `WORKFLOW_EVENT_BUDGET`; else a new event starts. The size is measured once for the base (the update with its phases and no agents) plus `serde_json::to_vec(agent).len() + 1` per agent. The events of one frame have disjoint agent indexes and the same absolute fields, so a receiver can apply each one alone and in any order.

Frame parser (`claude/frames.rs`): `MessageFrame` gets `workflow_run_id: Option<String>`: `tool_use_result.runId` (or `toolUseResult.runId`) when it matches `^wf_[a-z0-9-]{1,64}$`, else `None`; nothing is read from text.

Permission requests (`claude/requests.rs:93-150`): `body.agent_id` that matches `[a-z0-9]{1,64}` goes into `ApprovalRequested.provider_refs` as `"agent_id"`. No other change.

Interrupt: no change. `ClaudeCore::interrupt` still refuses a quiet core (`NoTurn`), also while a workflow runs (§4.8).

Codex has no workflows: no Codex change.

### 4.2 Proto

New file `crates/fleetdeck-proto/src/event_workflow.rs`; `event.rs` gets the variant and re-exports the types.

```rust
/// A Claude workflow run changed (B12). `phases` and `agents` hold only the
/// entries that are new or changed (merge by `index`; an agent entry replaces
/// the earlier entry of its index); the other fields are absolute. One frame
/// can give several events (§4.1 rule 9). Older peers decode this kind as unknown.
#[serde(rename = "workflow.updated")]
WorkflowUpdated(Box<WorkflowUpdate>),

/// The most JSON bytes of one `WorkflowUpdate` (§4.1 rule 9).
pub const WORKFLOW_EVENT_BUDGET: usize = 256 * 1024;

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
    /// True: the provider does not follow this workflow (too many run); no agents come.
    #[serde(default, skip_serializing_if = "is_false")] #[ts(optional)]
    pub untracked: bool,
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

The projection page (`session.workflows`, §4.4), in the same file:

```rust
/// `session.workflows` params (B12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "event/")]
pub struct WorkflowsQuery {
    pub session_id: SessionId,
    /// The `next` of the previous page; absent for the first page.
    #[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]
    pub after: Option<WorkflowsCursor>,
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]
    pub next: Option<WorkflowsCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct WorkflowRun {
    /// The station order of the run (the `station_seq` of its first event).
    pub run_order: u64,
    pub state: WorkflowUpdate,
}

/// The most JSON bytes of one `WorkflowsPage`.
pub const WORKFLOWS_PAGE_BUDGET: usize = 512 * 1024;
```

`event_kinds.rs`: `"workflow.updated"` in `KINDS` and `kind_name`.

`SessionSummary` (`session.rs`), four new fields:

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
/// True: `running_agents` is a lower bound (an untracked workflow runs, or the
/// fold dropped one at its cap). False again when `running_workflows` is empty.
#[serde(default, skip_serializing_if = "is_false")] #[ts(optional)]
pub agents_partial: bool,

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct RunningWorkflow {
    pub task_id: String,
    pub running_agents: u32,
    pub agents_total: u32,
    #[serde(default, skip_serializing_if = "is_false")] #[ts(optional)]
    pub untracked: bool,
}

pub const WORKFLOW_TASK_KINDS: &[&str] = &["local_workflow"];
/// Running workflows that the Claude core follows with their agents, per session.
pub const MAX_TRACKED_WORKFLOWS: usize = 64;
/// Entries of `running_workflows` (tracked and untracked).
pub const MAX_RUNNING_WORKFLOWS: usize = 256;
```

The TS fields are optional, so no web fixture changes (`web/src/testing/summary.ts`).

Link (`version.rs`): `pub const WORKFLOWS: &str = "workflows.v1";` in the feature list. It gates two methods (deck → station, scope `read`, not mutating): `session.workflows` (`WorkflowsQuery` → `WorkflowsPage`) and `session.workflow_agent`. The agent query is in `crates/fleetdeck-proto/src/subagent.rs`:

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

- `Event::WorkflowUpdated(u)` with `u.status == "running"`: upsert `running_workflows` by `task_id` with `running_agents`, `agents_total`, `untracked`. A new entry goes last. When the list already has `MAX_RUNNING_WORKFLOWS` entries, a new entry is not added and `agents_partial = true`. An entry with `untracked` sets `agents_partial = true`.
- `Event::WorkflowUpdated(u)` with any other status: remove the entry of `u.task_id`.
- `Event::BackgroundUpdated`: also `background_workflows` = tasks of `WORKFLOW_TASK_KINDS`. The list is the complete current task set (as `fold.rs:158` treats it), so **remove every `running_workflows` entry whose `task_id` is not in the new list**, also when other workflows stay in it. Ordering protection, separately: (1) the CLI lists a workflow before its `task_started` (§2.2 steps 2 and 3), so a start is never removed by its own list; (2) the core emits no running event for a task after its final status (§4.1 rule 2); (3) if a list comes early without a task that still runs, the next running event of that task adds it again (upsert).
- `clear_running` (`fold.rs:343-349`): also `background_workflows = 0`, `running_workflows.clear()`.
- After every change of `running_workflows`: when the list is empty, `agents_partial = false`.
- After each fold, next to `derive_activity` (`fold.rs:26-36`): `running_agents` = running tools with `tool_kind == subagent` + `background_agents` + Σ `running_workflows.running_agents` (saturating).

The summary has no detail of a workflow. The detail lives in the station projection (§4.3a, §4.4).

### 4.3a The workflow view rules (station projection and board)

The station projection (§4.4) and the board's `workflowStates` (§4.6) apply the same rules, in event order:

- **R1** `workflow.updated`: the first event of a task creates its run (`run_order` = the `station_seq` of that event). `status`, `untracked`, `running_agents`, `agents_total` are replaced; `node_id`, `name`, `run_id` are replaced when present. Phases are upserted by index. An agent entry replaces the entry of its index. A run keeps at most 100 phases and 1,000 agents (a new index beyond is ignored).
- **R2** A status that is not `running`: every agent of the run in `queued` or `running` becomes `stopped`; `running_agents = 0`.
- **R3** `background.updated`: every run with status `running` whose task id is not in the list becomes `stopped` (R2 applies).
- **R4** `session.ended`, or `session.state(_changed)` with `lost` or `exited`: every running run becomes `stopped` (R2 applies).
- **R5** A later `workflow.updated` with status `running` makes a stopped run `running` again (R3 with an early list); its agents change with their later events.
- **R6** (station only) A session keeps at most 32 runs with a status that is not `running`: when a 33rd ends, the run with the smallest `run_order` among them goes, with its phases and agents. A running run never goes. The binding rows (§4.4) stay.

The rules are idempotent for a replay of the same event, and an agent entry is always complete. So the board can start from a page with `as_of` and apply every later event (§4.6).

### 4.4 Station: projection, binding and the agent's chat

- **Migration V45** (architect numbering; renumbered at merge if V44 is not merged first), file `crates/fleetdeck-station/src/store/schema_v45.rs`, registered in `store/migrations.rs`:

```sql
-- B12: the workflow projection (§4.3a). Written in the event transaction.
CREATE TABLE workflow_runs (
    session_id     TEXT    NOT NULL,
    task_id        TEXT    NOT NULL,
    run_order      INTEGER NOT NULL,
    node_id        TEXT,
    name           TEXT,
    run_id         TEXT,
    status         TEXT    NOT NULL,
    untracked      INTEGER NOT NULL DEFAULT 0,
    running_agents INTEGER NOT NULL DEFAULT 0,
    agents_total   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, task_id)
) STRICT;
CREATE INDEX workflow_runs_order ON workflow_runs(session_id, run_order);
CREATE TABLE workflow_phases (
    session_id TEXT    NOT NULL,
    task_id    TEXT    NOT NULL,
    idx        INTEGER NOT NULL,
    title      TEXT    NOT NULL,
    PRIMARY KEY (session_id, task_id, idx)
) STRICT;
CREATE TABLE workflow_agents (
    session_id TEXT    NOT NULL,
    task_id    TEXT    NOT NULL,
    idx        INTEGER NOT NULL,
    agent_id   TEXT,
    state      TEXT    NOT NULL,
    entry      TEXT    NOT NULL,  -- the JSON of the WorkflowAgent
    PRIMARY KEY (session_id, task_id, idx)
) STRICT;
-- B12: one row for each bound workflow agent, with the run folder and the
-- launch facts of the process that reported it.
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
-- B12 (also DL-191): the launch facts of a shim process, read again when a
-- station start adopts it.
CREATE TABLE process_launch_facts (
    process_incarnation TEXT PRIMARY KEY,
    session_id          TEXT    NOT NULL,
    provider_home       TEXT    NOT NULL,
    cwd                 TEXT    NOT NULL,
    created_at          INTEGER NOT NULL
) STRICT;
```

- **Projection** (`store/workflow_state.rs`, `observe(conn, &envelope) -> Result<()>`): one new line in `store/log.rs` `append_at`, after `shell_observe::observe`. It applies R1 to R6 (§4.3a) to the tables in the event transaction, for both hosts. R2 rewrites the `state` column and the `state` of the `entry` JSON. It fails only on a database error (then the append fails, as for the other projections). An `entry` that does not parse is replaced by the next entry of its index.
- **Shared hook** (`crates/fleetdeck-station/src/workflows/mod.rs`, new top-level module, one `pub mod workflows;` line in `lib.rs`): `pub async fn observe(store, session, launch: Option<&LaunchFacts>, observed: &[Event])` calls `history::subagent::observe` and then `workflows::bind::observe`. It **replaces** the `crate::history::subagent::observe(…)` line in `manager/pump.rs:68` and in `manager/shim_target.rs:732`, so both hosts bind subagents and workflow agents in the same way.
- **Launch facts of a shim process** (`workflows/launch.rs`, `settle(store, session, process, launch) -> Option<LaunchFacts>`): the first line of the shim `observe` task (`shim_target.rs:694`) is `let launch = crate::workflows::launch::settle(&store, &session_id, &process, launch).await;`. With `Some(facts)` (a launch), it stores a `process_launch_facts` row (`INSERT OR IGNORE`, key: the text of the process incarnation) and returns the facts. With `None` (an adoption after a station restart, or a drain of an ended session), it reads the row of that process incarnation **and** that session and returns its facts; no row (a process launched before this build) gives `None`, as today. A store error gives `None` (warn log). The comment at `shim_target.rs:168-169` becomes "An adopted process gets the launch facts that its launch stored (B12)." The facts come only from the station's own policy at launch; no event and no request writes them.
- **Binding** (`workflows/bind.rs`, `observe`): for each `Event::WorkflowUpdated` with a `run_id` that matches `^wf_[a-z0-9-]{1,64}$`, the station reads from the projection every agent of `(session, task_id)` with an `agent_id` and no `workflow_agent_logs` row (one query, at most 1,000 rows), and stores one row for each agent id that matches `[a-z0-9]{1,64}`, with the native id of the store (the same checks as `subagent.rs:139-180`). So an agent that came before the run id is bound when the run id comes, and no event must repeat it. `put_workflow_agent_log(row) -> Result<bool>` (`INSERT OR IGNORE`: the first binding stays) and `workflow_agent_log(session_id, agent_id) -> Result<Option<WorkflowAgentLogRow>>` in `store/workflow_agent_logs.rs`, as `subagent_logs.rs:47-100`. No launch facts, no native id, an invalid id: no row (debug log). A store error is logged and never stops the pump or the observer.
- **Answer of `session.workflows`** (`store/workflow_pages.rs`, `workflow_page(session_id, after) -> Result<Option<WorkflowsPage>>`): unknown session → `None` (`unknown_session`). One read transaction: `as_of` = the session's `session_seq`; the runs in `run_order` from the cursor; for each run its fields, its phases (only where the run starts on this page) and its agents in index order (after `agent_index` for the cursor run). The page stops before the JSON size would pass `WORKFLOWS_PAGE_BUDGET`, and `next` names the run and the last agent index on the page. A session without workflows (or a Codex session) gets `{ as_of, workflows: [] }`.
- **Answer of `session.workflow_agent`** (`workflows/read.rs`): the same steps as `subagent::answer` (`subagent.rs:197-252`): `agent_id` checked; unknown session → `unknown_session`; not Claude → `unavailable { unsupported_format }`; no row → `unavailable { not_found }`; the row's names checked again (absolute home, agent id, run id pattern, native id) → else `unavailable { not_trusted }`; then the history budget and one read.
- **Path:** `locate::locate_workflow_agent(home, cwd, native, run_id, agent_id, trust)` in `history/locate.rs`: segments `projects`, `encode_cwd(cwd)`, `native`, `subagents`, `workflows`, `run_id`, file `agent-<agent_id>.jsonl`; the same `descend` without links, `file_flags`, `check_meta(.., SUBAGENT_FILE_MAX)` as `locate_subagent` (`locate.rs:180-211`).
- **Read:** `subagent::read_log` unchanged (`parse_line_subagent` with the native id and the agent id; a record of another session or agent → `not_trusted`).
- **Intake:** the handlers `workflows_method` and `workflow_agent_method` are in the new `link/intake_workflows.rs` (declared with `#[path]` from `intake.rs`, as `intake_native.rs`); `intake.rs` gets the declaration and two match arms near `:505`: `"session.workflows"` and `"session.workflow_agent"`. Both handlers are twins of `subagent_method` (`intake.rs:382-393`).
- **Method table** (`crates/fleetdeck-link/src/methods_table.rs`, after `session.subagent`): `m("session.workflows", DeckToStation, Scope::Read, Some(feature::WORKFLOWS), false)` and `m("session.workflow_agent", DeckToStation, Scope::Read, Some(feature::WORKFLOWS), false)`.

### 4.5 Deck

- Route `GET /api/sessions/{station}/{session}/workflows` with the optional query `after_order` (u64) and `after_index` (u32) → `session_work_routes::workflows`: **owner and members** of the session (`session_of(.., MemberAction::Read, ..)` in the `read` helper, no `owner_only`: the page holds what the session's events hold, owner question 4), `plain_session`, both query values or none (else 400 `invalid_params` "the page cursor is not valid"), `Route { method: "session.workflows", feature: feature::WORKFLOWS, unsupported: "workflows_unsupported", … }`. Answer `WorkflowsPage`. A failure is audited with the code only, as `subagent`.
- Route `GET /api/sessions/{station}/{session}/workflow-agents/{agent_id}` → `session_work_routes::workflow_agent`: owner only (403 `owner_only`), `plain_session`, agent id `[a-z0-9]{1,64}` (else 400 `invalid_params` "the agent id is not valid"), the `read` helper (`session_work_routes.rs:160-225`) with `Route { method: "session.workflow_agent", feature: feature::WORKFLOWS, unsupported: "workflows_unsupported", what: … }`, the same audit as `subagent`. Answer `SubagentLog`.
- Board-API method rows `sessions.workflows` (GET, `Scope::Read`, member `true`) and `sessions.workflow_agent` (GET, `Scope::Read`, member `false`). `methods_table_rest.rs` is at 792 lines: move its three DL-191 rows (`sessions.subagent`, `sessions.branches`, `sessions.branch_diff`, `:565-585`) with the two new rows into a new part `methods_table_work.rs` (`API_WORK`), joined by `super::api` like the other parts.
- `display_view.rs` `display_card`: also copy `running_agents` (a count, allowed by R13 §5D: it is the agent count of the card). Not `running_workflows` (task ids), not `background_workflows`, not `agents_partial` (no wall UI in this track).

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
  untracked: boolean;
  phases: Map<number, string>;    // index → title
  agents: Map<number, WorkflowAgentView>;
  running_agents: number; agents_total: number;
  /** 'station': from the page or a workflow.updated; 'task_only': only task.updated events. */
  source: 'station' | 'task_only';
};
/** The merged pages of `session.workflows`: `as_of` is the smallest `as_of` of the pages. */
export type WorkflowsSnapshot = { as_of: number; runs: WorkflowRun[] };
/** Starts from `snapshot` (null: an older station or no answer) and applies, in order, every
 * event with `session_seq > snapshot.as_of` (every event when null) with the rules R1 to R5
 * (design §4.3a). `task.updated` with kind `local_workflow` creates a `task_only` workflow or
 * updates its description and node (status: running→running, completed→completed,
 * failed/killed→failed, stopped/cancelled→stopped, for a `task_only` workflow only). */
export function workflowStates(
  snapshot: WorkflowsSnapshot | null, events: readonly EventEnvelope[],
): Map<string, WorkflowView>;
/** request_id → agent_id, from `approval.requested.provider_refs.agent_id`. */
export function requestAgents(events: readonly EventEnvelope[]): Map<string, string>;
/** The agent with this id in any workflow, or null. */
export function agentById(workflows: ReadonlyMap<string, WorkflowView>, agentId: string): WorkflowAgentView | null;
/** The status chip of an agent state: running ok; queued info; failed hazard; done, stopped and other text muted. */
export function agentStatus(state: string): ShownStatus;
```

**Loading** (new hook `web/src/session/useWorkflows.ts`, `useWorkflows(session, events, actions) → { workflows, snapshot }`):

- When the session view of a Claude session opens and `actions.workflows` exists, read the pages: the first without a cursor, then with `next` until `next` is absent, at most 64 pages. Merge the pages by `run_order` (R1 with the complete entries); `as_of` = the smallest `as_of` of the pages. One load runs at a time.
- A 409 `workflows_unsupported`, a 404, or another error: `snapshot = null` (the timeline-only view of an older station).
- **Reload** when the oldest retained event has `session_seq > snapshot.as_of + 1`: the browser window (1,000 events, §1.3) has dropped events after the page, so the page is read again. After a reload, `as_of` is at least the newest retained event, so a reload happens at most once per 1,000 events.
- `workflows = workflowStates(snapshot, events)` (memoized on both).

`tasks.ts:40`: keep the kind of an earlier event when the new kind is `unknown`. `hasAgents` is also true with one workflow; `runningCount` adds `running_agents` of the running workflows.

**Agents switcher** (`AgentsMenu.tsx` + new `web/src/chat/WorkflowAgents.tsx`):

- Summary: "Agents" with the pill of subagents + workflow agents (`agents_total` sums); "Commands" as today; "N running" includes running workflow agents.
- Menu order: "Main chat"; "Agents" group (subagents, as today); then one group per workflow, oldest first; then "Background commands".
- Workflow group head (`.fd-label fd-agent-group`): "Workflow · <name or description>" and the `.fd-count` of `agents_total`, then the status chip of the workflow (`running`, `done`, `failed`, `stopped`).
- Phase heads (new `.fd-agent-phase`, 12px `--dim`, indent 8px): "<index>. <title>"; agents without a phase index go under "Other agents" at the end. A phase without agents yet shows its head only.
- Agent rows (`.fd-agent-row fd-selbar`, `data-wfagent="<task_id>:<index>"`): the label, its chip; a dim second line: running → `last_tool` ("Bash"); done → the first line of `result_preview`; failed → the first line of `error`; else none.
- A `task_only` workflow: its head and one dim line. With `snapshot === null`: "This station does not report the agents of a workflow." With a snapshot: "Fleet Deck no longer keeps the agents of this workflow." (R6 removed it).
- An `untracked` workflow: its head and one dim line "This workflow started while 64 others ran. Fleet Deck does not show or count its agents."
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

**Actions** (`web/src/session/commands.ts`):

- `readonly workflows?: ((after: WorkflowsCursor | null) => Promise<WorkflowsPage>) | null;` → `GET ${sessionPath}/workflows` (with `?after_order=<n>&after_index=<n>` for a cursor). Present for the owner and for members.
- `readonly workflowAgent?: ((agentId: string) => Promise<SubagentLog>) | null;` → `GET ${sessionPath}/workflow-agents/${encodeURIComponent(agentId)}`, like `subagent` (`commands.ts:339-342`). Owner only.

**SessionView** (797 lines): move the agent choice logic (`SessionView.tsx:145-161`) into a new hook `web/src/session/agentChoice.ts` (`useAgentChoice`) that calls `useWorkflows`, also resolves `wf:` choices, and renders the agent view. The net change of `SessionView.tsx` must be ≤ 0 lines. The Properties "Agents" row gets `agents` = subagents + workflow agents and `runningAgents` = the switcher's running count (`SessionView.tsx:263,771`).

### 4.7 Board: B19

New `web/src/board/agentCounts.ts`:

```ts
/** Running agents of a card: the summary's `running_agents` (new decks), else the older sum. */
export function runningAgents(s: SessionSummary): number {
  return Math.max(s.running_agents ?? 0, runningSubagents(s));
}
/** True when the count of the card is a lower bound (design §4.3). */
export function agentsPartial(s: SessionSummary): boolean {
  return s.agents_partial === true;
}
/** Background tasks that are neither subagents nor workflows. */
export function backgroundCommands(s: SessionSummary): number {
  return Math.max(0, s.background_tasks.length - s.background_agents - (s.background_workflows ?? 0));
}
/** The fleet total: live cards whose activity is working or background, plus the running agents of every live card. */
export function atWork(cards: readonly DeckSession[]): { sessions: number; agents: number; partial: boolean } {
  const live = cards.filter(isLiveSession);
  return {
    sessions: live.filter((c) => c.summary.activity === 'working' || c.summary.activity === 'background').length,
    agents: live.reduce((n, c) => n + runningAgents(c.summary), 0),
    partial: live.some((c) => agentsPartial(c.summary)),
  };
}
```

- **Strip** (`FleetStrip.tsx:166,270-297`): the `subagents` ico becomes `kind="agents"`, title "Agents running", `n = runningAgents(s)`, `data-running-agents`; with `agentsPartial(s)` the number shows as "3+" and the title is "Agents running. Some workflow agents are not counted." The `background` ico becomes title "Background commands", `n = backgroundCommands(s)`, hidden at 0.
- **Sentence** (`FleetSentence.tsx`): a new optional prop `atWork?: { sessions: number; agents: number; partial: boolean }`. When `sessions + agents > 0`, after the stations part and its period: `<span className="fd-home-work" title={…}>9 at work</span>.` Words: "1 at work" / "N at work" (`countText`), "N+ at work" when `partial`. Title: "3 sessions and 6 of their agents run now. Sessions on unreachable stations are not counted." (singular "1 session", "1 of their agents"; with 0 agents: "3 sessions run now. …"; with `partial` the title ends with " Some workflow agents are not counted."). Example: "6 live sessions on 4 stations. 9 at work. 2 need you."
- **FleetHome** (`FleetHome.tsx:323-324,463-468`) and **Watch** (`WatchPage.tsx:529-533`): pass `atWork(live)` and `atWork(matching)`.
- **Properties:** §4.6.

### 4.8 Behavior after stop, takeover and resume

| Case | Claude | Fleet Deck |
| --- | --- | --- |
| Interrupt during a main turn (the board's Interrupt, enabled only with a `current_run`) | Ends the turn. `rec4` shows that Claude's interrupt also kills the running workflows (sent between turns, outside Fleet Deck); during a turn it is expected to do the same, not recorded | When Claude kills a workflow: `task_updated killed` → `workflow.updated status stopped`; the fold removes it, R2 stops its agents. When Claude does not kill it: the workflow keeps running and the board shows it running |
| Workflow runs, no main turn (the normal case after the launch) | — | The board offers no Interrupt (`controller.ts:33,45`); the station refuses one with `invalid_state` "no turn runs" (`decide.rs:197-203`); the core refuses it too (`core.rs:436`). The owner ends the workflow with **Stop**, which ends the session process (next row). A per-workflow stop (`stop_task`) is a later item (§4.11, owner question 2) |
| Stop, process lost or ended | The workflow dies with the process; no state file | `session.state lost/exited` or `session.ended`: the fold clears; R4 stops the runs and their agents in the projection and on the board |
| Shim takeover (station restart) | The process lives; frames continue | The core checkpoint holds `workflows` and `untracked_workflows`; diffs continue; no event is lost beyond the shim's own rules. The observer reads the stored launch facts of the process (§4.4), so the agents of later phases get binding rows |
| Resume | `task_notification stopped` for the old task before `init` | `task.updated` kind `unknown`; the board keeps `local_workflow` (§4.6); the run stays `stopped` (R4 at the end of the old process); agent chats stay readable (rows and files stay) |

### 4.9 Compatibility

- New station, older deck: `workflow.updated` is stored and forwarded as an unknown kind (`event_body.rs:56-70`); older boards ignore it. The older deck has no routes, so no workflow list from the station and no agent chat.
- Older station, new deck/board: no `workflow.updated`, feature `workflows.v1` absent: both routes answer 409 `workflows_unsupported` with the older-station text before any forward. The board uses `snapshot = null` and shows the workflow group from `task.updated` with "This station does not report the agents of a workflow." `running_agents` then counts subagents only.
- A summary stored before this build reads the new fields as 0 / empty / false (`#[serde(default)]`). The board falls back to `runningSubagents`.
- The checkpoint stays version 1: the new core fields are `#[serde(default)]`; an older station that restores a newer checkpoint ignores the unknown fields (serde default behavior of `ClaudeCore`, no `deny_unknown_fields`).
- A shim process that was launched before this build has no `process_launch_facts` row: after a takeover it binds nothing, as today.
- The station database is forward only (§5 Migration).

### 4.10 Errors and edge cases

- A `task_progress` with a `workflow_progress` that is not an array, an entry without an integer `index`, or an agent with a non-string `label`: the entry is skipped; the frame is not an unknown frame.
- Two workflows in one session: each has its own entry, run, group and count. A workflow that ends while another runs leaves the fold at its final event, or at the next `background.updated` that does not list it (§4.3).
- More than 64 running workflows in one session: the next ones are untracked (§4.1); the strip and the sentence show "N+" (§4.7). More than 1,024 untracked ids at the same time: the oldest id is forgotten, and its end gives no event; the fold removes it at the next `background.updated` without it.
- `workflow()` sub-runs (nested named workflows): their agents come in the same `workflow_progress` with their own indexes; they show in the same group (no nesting in this design).
- Hidden-script runs: phase titles "phase N", no prompt preview, "error details withheld for this workflow" as error: shown as they come.
- Retries: `attempt` > 1 shows "Attempt 2"; the same index keeps one row.
- An agent's own background commands (`owned_by_subagent: true`, `local_bash`) stay in "Background commands" with no card, as B6 does today.
- An agent id that the station never bound (a run before this build, or no launch facts): "Show its work" answers `unavailable { not_found }` with the history text that exists.
- A transcript that grows during the read: only the captured extent is read; "Refresh" reads again.
- A very large run (1,000 agents): about 3 changes per agent (queued/running/done). Each frame emits only the changed agents, in events of at most 256 KiB. The station projection keeps one row per agent; the summary keeps only counts; the board reads the projection in pages of at most 512 KiB.
- The checkpoint holds about 50 bytes per followed agent: at most about 3.2 MB at 64 workflows of 1,000 agents; usually a few KB.

### 4.11 Out of scope

- Codex (no workflows). Nested display of sub-workflows. Live streaming of an agent's chat (only on request, as DL-191). Stopping one workflow or one agent from the board, or an interrupt between turns (Claude has `stop_task`; a later item). The wall UI for the total (B17). The coordinator tools `list_sessions` / `fleet_view` showing agent counts. Cost per workflow (B14). Reading `<runId>.json`, the journal or the task output file. Removing `process_launch_facts` rows of dead processes (small rows; a later maintenance item).

## 5. Risk class: high

Reasons: a new station read path to provider files (§4.4), an additive station migration (V45), two new link methods, a new member-readable deck route, and a public event kind (`workflow.updated`) that boards and decks read.

Threat notes:

- **Path choice by a caller.** The caller names only a session and an agent id. The station serves only an agent id that its own stream reported for that session (binding row, `PRIMARY KEY (session_id, agent_id)`); the run id and the home come from the row, never from the request. Run id and agent id are checked by pattern before they become path segments, at bind time and again at read time.
- **Launch facts.** The station writes a `process_launch_facts` row only from its own policy at launch (`LaunchFacts::of`), never from an event or a request. It reads the row only for the same process incarnation and the same session. A missing row gives no binding.
- **Links and swaps.** Every segment is opened with `descend` without following links; the file passes `check_meta` (owner, one hard link, not private, ≤ 64 MiB); one read of the captured extent.
- **Foreign records.** Every record must name the session's native id and the agent id; else the whole answer is `not_trusted`.
- **Who reads.** The agent chat route is owner only (as DL-191); a member and a token get 403; a display has no route. The workflow page route has the access of the session's events (`MemberAction::Read`): it holds only what `workflow.updated` events hold (owner question 4).
- **Event content.** `prompt_preview`, `result_preview` and `error` are model text (≤ 200 characters each) in the event log and the projection, as tool inputs and messages are today. A member of a shared session sees them like the other events of the session (owner question 4). The display card copies only the count.
- **Volume.** One `workflow.updated` is at most 256 KiB and one page at most 512 KiB, both far below the 1 MiB control frame (§1.3), so a workflow never blocks the station stream. The provider emits only changed agents; the summary keeps at most 256 workflow entries of four fields; the projection keeps every running run and 32 ended runs per session, each with at most 1,000 agents and 100 phases.
- **Projection in the event transaction.** A failure of the projection fails the append, as the other projections do; it fails only on a database error, never on event content.
- **Migration.** Additive `CREATE TABLE` and `CREATE INDEX`; no data rewrite. The schema is **forward only**: an older station binary refuses the migrated database with `DbError::TooNew` (`fleetdeck-core/src/db.rs:220-229`) and does not start. Rollback procedure: stop the station service, put the older binary in place, replace `<data>/station/station.db` with the pre-upgrade backup `<data>/backups/station-v<from>-to-v<to>-<ms>.db` that the upgrade wrote (`db_backup.rs:70`), then start the service. The events, bindings and other rows written after the upgrade are lost with this procedure.

## 6. Build plan

Every heavy command runs as `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. Every task also runs the guards: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`. UI tasks also run `sh tools/design-check.sh` and follow `docs/v2/DESIGN-SYSTEM.md` (skill `fleet-design-system`).

Order: T1 → (T2 ∥ T3a ∥ T4 ∥ T5 ∥ T6), at most 2 workers at the same time; T3b after T3a; T5b after T5 and T4. The adversary DESIGN REVIEW (§5) gates T3a, T3b and T4.

### T1 (backend): proto: event, page, summary, fold, query, feature — about 550 lines

- **Owned files:** `crates/fleetdeck-proto/src/event_workflow.rs` (new), `event.rs`, `event_kinds.rs`, `session.rs`, `fold.rs`, `fold_tests_workflow.rs` (new, `#[cfg(test)] #[path]` from `fold.rs`), `subagent.rs`, `version.rs`, `ts.rs`, `event_tests.rs`, `web/src/gen/**` (regenerated by `make ts-types` via heavy.sh).
- **Changes:** §4.2 types, variant, kinds, `WORKFLOW_EVENT_BUDGET`, `WorkflowsQuery`, `WorkflowsCursor`, `WorkflowsPage`, `WorkflowRun`, `WORKFLOWS_PAGE_BUDGET`, summary fields and `RunningWorkflow`, `WORKFLOW_TASK_KINDS`, `MAX_TRACKED_WORKFLOWS`, `MAX_RUNNING_WORKFLOWS`, `WorkflowAgentQuery`, feature `WORKFLOWS` in the list; §4.3 fold; TS export of the new types.
- **Tests:**
  - `workflow_update_round_trips_and_skips_empty_options` (serde: absent options and `untracked: false` are not written; an event without `phases`/`agents` reads as empty).
  - `an_older_reader_keeps_workflow_updated_as_unknown` (an `EventBody` from JSON with an unknown extra field still parses; the kind is in `KINDS`).
  - `a_running_workflow_adds_its_running_agents` (two workflows: `running_agents` = sum + `background_agents` + one running subagent tool).
  - `a_final_status_removes_the_workflow` (`completed`, `failed`, `stopped`).
  - `background_updated_removes_a_missing_workflow_while_another_runs` (A and B run; a list with only B removes A; `running_agents` = B's agents).
  - `a_running_event_after_an_early_list_adds_the_workflow_again`.
  - `the_end_of_the_process_clears_the_workflows` (`session.ended`, `session.state lost`).
  - `an_untracked_workflow_sets_agents_partial_until_the_list_is_empty`.
  - `at_most_256_running_workflows_then_partial` (the 257th is not added; `agents_partial` is true; it is false after `clear_running`).
  - `workflow_agent_query_and_workflows_query_refuse_unknown_fields`.
- **Acceptance:** `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(workflow) | test(fold) | binary(ts_drift) | test(event)'`; `cargo check -p fleetdeck-providers -p fleetdeck-station -p fleetdeck-deck`; the guards.

### T2 (backend): Claude adapter — about 650 lines

- **Depends on:** T1.
- **Owned files:** `crates/fleetdeck-providers/src/claude/workflows.rs` (new), `claude/tests_workflows.rs` (new), `claude/tasks.rs`, `claude/map.rs` (≤ 15 added lines), `claude/frames.rs`, `claude/core.rs` (fields + `new`), `claude/requests.rs`, `claude/mod.rs`, `spikes/s1-claude/fixtures/workflow/*.jsonl` (new: copy `~/fleetdeck-briefs/backlog/workflow-agents/fixtures/*.jsonl` verbatim; not under `fixtures/rust`, so the replay harness does not take them).
- **Changes:** §4.1.
- **Tests** (feed the `out` frames of a fixture into `running_core()`-style cores; `tests_tasks.rs:17-40` shows the pattern):
  - `two_phases_give_phases_agents_and_the_run_id` (`workflow_two_phases.jsonl`: first event `running` with name `bl-tiny`; the run id `wf_0918c8f1-e62` in `tool.completed.provider_refs.workflow_run_id`, in one `workflow.updated` without agents, and in later events; phases 1 Alpha, 2 Beta; agent 1 `running` then `done` with result `alpha`; final `completed` without agents; `running_agents` 0 at the end).
  - `fanout_reports_parallel_agents_and_only_changes` (`workflow_fanout.jsonl`: two agents `running` at once, `running_agents == 2`; a frame without `workflow_progress` emits no `workflow.updated`; a frame that repeats a state emits nothing; a change of only `tokens` emits nothing).
  - `an_interrupt_stops_the_workflow_without_agent_entries` (`workflow_interrupt.jsonl`: one `workflow.updated status stopped`, `running_agents 0`, no agents; `task_notification stopped` after `killed` emits no second one; a later progress frame of the task emits nothing).
  - `an_agent_permission_names_its_agent` (`workflow_agent_approval.jsonl`: `approval.requested.provider_refs.agent_id == "a3f92b5c63d2c47ee"`).
  - `a_resume_notification_of_an_unknown_task_emits_no_workflow_event` (`workflow_resume_orphan.jsonl`).
  - `a_queued_agent_is_not_running` (synthetic `start` without `startedAt`).
  - `bad_entries_are_skipped` (no index, non-string label, non-array list, a task id with `/`, an agent id with `.`).
  - `the_run_id_is_read_only_from_the_structured_result` (text "Run ID: wf_x" without `tool_use_result` gives none; `runId: "../x"` gives none).
  - `the_checkpoint_keeps_the_workflows` (checkpoint, restore, next progress frame diffs against the restored digests; `untracked_workflows` and `next_workflow` survive).
  - `a_maximum_frame_splits_under_the_frame_limit` (synthetic: 100 phases and 1,000 agents, every text at its cap and made of `\u{1}` characters, so JSON escapes are 6 bytes: every emitted `WorkflowUpdate` is ≤ `WORKFLOW_EVENT_BUDGET`; each one, put in an `EventEnvelope` with a 128-character session id and in `ControlFrame::Event` with a 64-character incarnation, gives `serde_json::to_string(..).len() <= MAX_CONTROL_FRAME_BYTES`; the union of the agents is the 1,000 indexes once each; every event has the same status and counts).
  - `limits_cut_texts_and_cap_agents` (label > 120 characters cut; agent 1,001 ignored; a 33rd ended workflow drops the oldest ended one; a running one is never dropped).
  - `the_65th_running_workflow_is_untracked` (64 started: the 65th gives one `untracked: true` running event; its progress emits nothing; its `task_updated completed` gives one `untracked: true` final event; the other 64 keep their run ids).
  - `an_interrupt_without_a_turn_is_refused_while_a_workflow_runs` (after the main `result` with a running workflow: `core.interrupt(None)` is `Err(ProviderError::NoTurn)`).
- **Acceptance:** `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(workflow) | test(task) | binary(claude_core_replay)'`; the guards.

### T3a (backend): station projection, launch facts, binding on both hosts — about 650 lines

- **Depends on:** T1; adversary DESIGN REVIEW pass.
- **Owned files:** `crates/fleetdeck-station/src/store/schema_v45.rs` (new), `store/workflow_state.rs` (new), `store/workflow_agent_logs.rs` (new), `store/launch_facts.rs` (new), `store/migrations.rs` (one line), `store/mod.rs` (`pub mod` lines only), `store/log.rs` (one line), `store/tests_workflow_state.rs` (new, `#[cfg(test)] #[path]` from `workflow_state.rs`), `crates/fleetdeck-station/src/lib.rs` (one `pub mod workflows;` line), `crates/fleetdeck-station/src/workflows/mod.rs` (new), `workflows/launch.rs` (new), `workflows/bind.rs` (new), `workflows/tests_bind.rs` (new), `manager/pump.rs` (the replaced line), `manager/shim_target.rs` (the replaced line, the `settle` line, the comment, the test module declaration; the file must stay below 800 lines), `manager/shim_workflow_tests.rs` (new, declared from `shim_target.rs`, so it can use `super::shim_resume_rig`). No line in `history/mod.rs` (798) or `manager/mod.rs` (797).
- **Changes:** §4.3a in the station (R1 to R6), §4.4 migration, projection, shared hook, launch facts, binding.
- **Tests:**
  - Projection (`tests_workflow_state.rs`, store level): `the_projection_merges_changes_by_index`; `a_final_status_stops_queued_and_running_agents`; `background_updated_stops_a_missing_run_while_another_runs`; `the_end_of_the_session_stops_running_runs`; `a_running_event_revives_a_run_stopped_by_an_early_list`; `at_most_32_ended_runs_and_no_running_run_is_removed`; `three_thousand_events_keep_every_agent` (1,000 agents, 3 changes each, through `Store::append`; the projection has all 1,000 agents in their last state, far beyond the 500-row deck timeline).
  - Binding (`tests_bind.rs`, the `tests_subagent.rs` rig): `the_run_id_binds_every_agent_that_came_before_it`; `a_later_agent_is_bound_by_its_own_event`; `no_row_without_run_id_agent_id_native_id_or_launch_facts`; `the_first_binding_of_an_agent_stays`.
  - Launch facts: `settle_stores_the_facts_of_a_launch_and_reads_them_for_an_adoption`; `settle_reads_no_facts_of_another_session_or_process`.
  - Shim host (`shim_workflow_tests.rs`, the rig of `manager/shim_resume_rig.rs`, the provider home set as in `history/tests_subagent.rs`): `a_shim_hosted_session_binds_workflow_agents` (a shim session of the rig; `Event::WorkflowUpdated` values of the two-phase run appended to the session with `Store::append`; the shim observer stores a row for agent 1); `a_restart_between_phases_binds_the_later_agent` (after phase 1, `as_shim_station()` restarts the station and adopts the session with `launch: None`; an event of phase 2 with agent 2 and the run id is appended; a row for agent 2 exists with the launch's home and cwd); `an_interrupt_between_turns_is_refused_while_a_workflow_runs` (the session has a running workflow and no run; `SessionCommand::Interrupt` through the manager gives a rejected receipt `invalid_state` "no turn runs").
  - Migration: the existing `the_migrations_are_valid` (`store/tests.rs:70`) passes with V45; new `a_v44_database_migrates_to_v45_with_the_workflow_tables` (use the number before V45 at the time of the build).
- **Acceptance:** `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(workflow) | test(subagent) | test(settle) | test(migrations) | test(v45) | test(shim_workflow)'`; the guards (module size: `shim_target.rs` < 800).

### T3b (backend): station methods `session.workflows` and `session.workflow_agent` — about 450 lines

- **Depends on:** T3a; adversary DESIGN REVIEW pass.
- **Owned files:** `crates/fleetdeck-station/src/store/workflow_pages.rs` (new), `store/mod.rs` (one `pub mod` line), `workflows/read.rs` (new), `workflows/mod.rs` (one `pub mod read;` line), `workflows/tests_read.rs` (new), `history/locate.rs`, `link/intake_workflows.rs` (new), `link/intake.rs` (the `#[path]` declaration and two match arms, ≤ 8 lines), `crates/fleetdeck-link/src/methods_table.rs`.
- **Changes:** §4.4 answers, path, intake, method table.
- **Tests:**
  - `it_reads_the_agent_transcript_of_the_run_folder` (a file under `subagents/workflows/wf_…/`; items and tool results paired).
  - `an_unbound_agent_is_not_found`; `a_codex_session_is_unsupported`.
  - `a_link_on_the_run_folder_or_the_file_is_not_trusted`; `a_record_of_another_agent_is_not_trusted`; `a_bad_run_id_in_a_row_is_not_trusted`.
  - `bytes_appended_after_open_are_not_read`.
  - `the_first_page_has_as_of_and_every_run_in_order`; `an_unknown_session_is_unknown_session`; `a_session_without_workflows_gets_an_empty_page`.
  - `a_maximum_run_is_paged_under_the_budget` (1,000 agents with texts at their caps and 6-byte escapes: each page ≤ `WORKFLOWS_PAGE_BUDGET`; each page in `ControlFrame::Response` is ≤ `MAX_CONTROL_FRAME_BYTES`; the pages give every agent once; the phases come only on the first page of the run).
  - `the_methods_are_in_the_table_with_their_feature` (link crate).
- **Acceptance:** `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(workflow) | test(subagent)'`; `cargo nextest run -p fleetdeck-link --no-fail-fast -E 'test(method)'`; the guards.

### T4 (backend): deck routes and display — about 300 lines

- **Depends on:** T1; adversary DESIGN REVIEW pass.
- **Owned files:** `crates/fleetdeck-deck/src/http/session_work_routes.rs`, `http/session_work_workflow_tests.rs` (new, `#[cfg(test)] #[path]`), `http/mod.rs` (two routes), `http/methods_table_rest.rs`, `http/methods_table_work.rs` (new), the file that joins the parts (`super::api`), `http/display_view.rs` and its tests.
- **Changes:** §4.5.
- **Tests:** `a_member_or_token_gets_owner_only_for_the_agent_chat`; `a_member_of_the_shared_session_reads_the_workflow_page`; `a_member_without_the_session_does_not_read_the_page` (the answer of `session_of`); `a_bad_agent_id_is_invalid_params`; `a_half_cursor_is_invalid_params`; `an_older_station_gets_409_workflows_unsupported_without_a_forward` (both routes); `the_answer_of_the_station_is_passed_through` (both routes; the cursor goes to `WorkflowsQuery.after`); `the_failure_is_audited_with_the_code_only`; `the_method_table_has_both_rows_with_their_member_flags`; `the_display_card_copies_running_agents_only`.
- **Acceptance:** `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(workflow) | test(display) | test(methods_table)'`; the guards.

### T5 (ux): workflow state, loading, Agents switcher, agent view — about 750 lines

- **Depends on:** T1.
- **Owned files:** `web/src/session/workflows.ts` (new), `web/src/session/workflows.test.ts` (new), `web/src/session/useWorkflows.ts` (new), `web/src/session/useWorkflows.test.tsx` (new), `web/src/session/tasks.ts`, `web/src/session/tasks.test.ts`, `web/src/session/commands.ts` (both actions), `web/src/session/controller.test.ts` (new), `web/src/chat/AgentsMenu.tsx`, `web/src/chat/AgentsMenu.test.tsx`, `web/src/chat/WorkflowAgents.tsx` (new), `web/src/chat/WorkflowAgentView.tsx` (new), `web/src/chat/workflowAgents.test.tsx` (new), `web/src/session/agentChoice.ts` (new), `web/src/session/SessionView.tsx` (net ≤ 0 lines), `web/src/styles/chat.css`.
- **Changes:** §4.6 except the card line, the approval line and "Show its work" (T5b). Both actions in `commands.ts`. In this task the agent view shows the facts and no "Show its work".
- **Tests:** `workflowStates merges phases and agents by index`; `workflowStates applies only events after as_of to a snapshot`; `a final status stops queued and running agents (R2)`; `background.updated stops a missing run while another runs (R3)`; `an older station gives a task_only workflow from task.updated`; `a run removed by the station says Fleet Deck no longer keeps its agents`; `an untracked workflow shows its line`; `the end of the process stops running workflows and agents`; `an unknown task kind keeps local_workflow`; `useWorkflows reads every page and merges them` (a run cut over two pages); `a cold load beyond the timeline shows the snapshot` (snapshot with 1,000 agents, an event window that holds none of their events); `continued viewing past the window reads the pages again once` (the oldest retained event has `session_seq > as_of + 1`: one more load, then none); `a 409 gives the timeline-only view`; `the switcher lists workflow → phase → agent with chips and notes`; `queued is info, failed is hazard`; `agents without a phase go under Other agents`; `choosing an agent sets agent=wf:<task>:<index> and shows the facts`; `the composer line says a workflow agent takes no input`; `an unknown wf: choice shows the main chat`; `the running count includes running workflow agents`; `the workflows action sends the cursor as after_order and after_index`; `controller: with only a running workflow, Interrupt is off and Stop is on`.
- **Acceptance:** `cd web && bun run test workflows useWorkflows AgentsMenu tasks workflowAgents controller commands session` (via heavy.sh); `sh tools/design-check.sh`; the guards.

### T5b (ux): card line, approval line, "Show its work" — about 200 lines

- **Depends on:** T5, T4.
- **Owned files:** `web/src/chat/WorkflowLine.tsx` (new), `web/src/chat/workflowLine.test.tsx` (new), `web/src/chat/ChatView.tsx` (≤ 20 lines), `web/src/session/ApprovalDock.tsx`, `web/src/session/approvalDock.test.tsx`, `web/src/chat/WorkflowAgentView.tsx`, `web/src/session/agentChoice.ts`.
- **Changes:** §4.6 card line, approval line, "Show its work" in the agent view (with the `workflowAgent` action of T5).
- **Tests:** `the card line counts done and running agents`; `a stopped workflow says Stopped`; `an approval of a workflow agent says From workflow agent <label>`; `Show its work calls GET …/workflow-agents/<id>`; `no agent id yet says Its work shows after it starts.`; `a member sees no Show its work`.
- **Acceptance:** `cd web && bun run test workflowLine approvalDock workflowAgents` (via heavy.sh); `sh tools/design-check.sh`; the guards.

### T6 (ux): B19 counts — about 320 lines

- **Depends on:** T1.
- **Owned files:** `web/src/board/agentCounts.ts` (new), `web/src/board/agentCounts.test.ts` (new), `web/src/board/FleetStrip.tsx`, the strip tests that name "Subagents running" or "Background tasks", `web/src/views/FleetSentence.tsx`, `web/src/app/FleetHome.tsx`, `web/src/watch/WatchPage.tsx`, `web/src/styles/home.css`, `docs/v2/DESIGN-SYSTEM.md` (§4.1 strip counters: "agents" with the "N+" form and "background commands"; §4.4 the "N at work" part; §4.5 the Agents switcher workflow groups; one expansion-log row).
- **Changes:** §4.7.
- **Tests:** `runningAgents prefers running_agents and falls back`; `agentsPartial reads agents_partial`; `backgroundCommands leaves out agents and workflows`; `atWork counts working and background sessions and the agents of live cards, not unreachable ones`; `atWork is partial when one live card is partial`; `the strip shows Agents running 3 and Background commands 1`; `a partial card shows 3+ with the note in the title`; `the sentence says 9 at work with its title`; `a partial total says 9+ at work`; `no at-work part at 0`; `Watch passes the matching cards`.
- **Acceptance:** `cd web && bun run test agentCounts FleetStrip FleetSentence fleet watch` (via heavy.sh); `sh tools/design-check.sh`; the guards.

## 7. Decisions made in this draft

1. **Source of progress:** `task_progress.workflow_progress` in the stream. Rejected: polling `workflows/<runId>.json` or the journal (written at the end or only with results; no states).
2. **One new event `workflow.updated` with changed entries and absolute counts, in chunks of at most 256 KiB.** Rejected: one `task.updated` per agent (no phases, and B6 code would treat them as tasks); a full snapshot per event (up to 1,000 agents each time); one event per frame without a size limit (can pass the 1 MiB control frame and stop the station stream).
3. **Agent states normalized** to `queued`, `running`, `done`, `failed`, `stopped` in the adapter, other text passed through. Rejected: the CLI's `start`/`progress`/`error` words on the board.
4. **Counters ride along, they do not trigger an event** (`tokens`, `tool_calls`, `last_tool`). Rejected: an event per counter change (hundreds per agent).
5. **The summary keeps `running_workflows` (≤ 256, tracked and untracked), `background_workflows`, a derived `running_agents` and `agents_partial`.** Rejected: a board-only count from events (the fleet board has summaries only); a session total carried in each event (couples workflows); a cap of 16 (a normal session could pass it and lose agents from the total silently).
6. **New link methods `session.workflows` and `session.workflow_agent` (feature `workflows.v1`) and a new binding table keyed by `(session_id, agent_id)`.** Rejected: a field on `SubagentQuery` (`deny_unknown_fields` and the DL-191 contract); reuse of `subagent_logs` with a synthetic tool use id (no run id column); no binding row (the agent id would be request-chosen).
7. **The run id comes only from `tool_use_result.runId`** with the `wf_` pattern. Rejected: the `run_id` of `task_started` (a CLI task run id, §2.2) and any text.
8. **The binding needs the run id; the station binds from its projection when the run id comes.** Rejected: a resend of every agent by the core (large events, and the binding then depends on an event that a size limit can split).
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
19. **Fleet Deck keeps its interrupt guards.** Interrupt works during a main turn; between turns, Stop (the end of the process) ends a workflow. Rejected in this track: a session-wide interrupt between turns (it changes three guards on a command path, and a per-workflow `stop_task` is the better later item).
20. **Fixtures go to `spikes/s1-claude/fixtures/workflow/`, out-only**, not to `fixtures/rust/` (the replay harness needs host commands).
21. **Migration number V45** (architect numbering).
22. **A station projection of the workflows, written in the event transaction, read in pages by `session.workflows`.** Rejected: a view computed from the timeline only (500 rows on the deck, 1,000 in the browser); the detail in the summary (up to megabytes per session); a deck-side projection (needs a deck migration, and this track has no deck number).
23. **One set of view rules (R1 to R6) for the station and the board**, so a page plus the later events give the same view as all events.
24. **The station stores the launch facts of each shim process and reads them at the takeover.** Rejected: no binding after a takeover (the agents of later phases would never be readable).
25. **One shared binding hook for the direct and the shim host** (`workflows::observe`). Rejected: a second call only in `pump.rs` (the shim host is the Linux default).
26. **The core follows at most 64 running workflows and never drops a running one; more are untracked and make the count "N+".** Rejected: dropping the oldest running entry (loses the run id and later bindings); no limit (the checkpoint grows without bound).
27. **The core keeps a digest per agent, not the entry**, so the checkpoint stays small.

## 8. Owner questions (taste or policy; the build goes on with the default)

1. **The fleet total:** "9 at work" counts the sessions that work plus their running agents. Do you want agents only ("6 agents running")? **Default:** sessions plus agents, "N at work", the split in its title.
2. **Stop and workflows:** Claude's interrupt also ends every running workflow of the session. Should Stop say so when a workflow runs ("Stop also ends 1 running workflow.")? **Default:** no change in this track; note it for a later item with a per-workflow stop. *Revision 2 correction:* Fleet Deck's Interrupt works only during a main turn (§4.8). Between turns, only Stop ends a workflow, because it ends the session process. The default stays.
3. **The wall:** show the "at work" total on the wall? **Default:** not in this track; B17 decides (the number is already in the display card).
4. **Shared sessions:** a member sees the workflow agents' task and result previews (≤ 200 characters), as they see tool inputs today. **Default:** yes, the same as other session events; the agent chat ("Show its work") stays owner only. In revision 2 this also covers the workflow page route (§4.5).

## 9. Side findings for other tracks

- **B14 (cost):** the growth of the process total after a workflow includes the workflow agents (`rec1`: modelUsage cache creation 75,487 → 129,006 over a turn of 7,725). A workflow that ends after the last turn is counted only at the next `result`. `subagent_stats` does not count workflow agents.
- **B21 (first turn after resume):** a resumed process sends a `result` with `num_turns: 0` and cost 0 before the first real turn when a background task was cut (`rec6`). Check how the core binds that frame.
- **B2:** the turn that a workflow notification starts is still a provider turn ("provider turn" on the dev1 session).
- **DL-191:** with `process_launch_facts` (§4.4), subagents of an adopted shim session get binding rows too (today they get none). A positive side effect; no DL-191 code changes except the shared hook.

## Decision log entries (to append)

## DL-420: Workflow progress from the Claude stream (B12, design `docs/v2/design/workflow-agents.md`)

- Context: a Claude `Workflow` run showed only a "done" tool card and one background task. Recordings (Claude 2.1.308) show one `local_workflow` task and `task_progress.workflow_progress` lists of phases and agents; no agent frames in the stream.
- Decision: the Claude adapter keeps a state per workflow (checkpointed, a digest per agent) and emits `workflow.updated { task_id, node_id?, name?, run_id?, status, untracked?, phases[], agents[], running_agents, agents_total }` with only the changed phases and agents, in events of at most 256 KiB; states `queued`, `running`, `done`, `failed`, `stopped`; counters ride along. A final status carries no agents; the receivers stop the queued and running ones. The core follows at most 64 running workflows and never drops a running one; more are `untracked`. The run id comes only from `tool_use_result.runId` (`wf_…`). Permission requests of a workflow agent carry `provider_refs.agent_id`.

## DL-421: Agent counts in the session summary (B12, B19)

- Decision: `SessionSummary` gets `running_workflows` (at most 256, `{task_id, running_agents, agents_total, untracked?}`), `background_workflows`, a derived `running_agents` (running subagent tools + `background_agents` + running workflow agents) and `agents_partial`. A `background.updated` removes every workflow that it does not list. All `#[serde(default)]` and optional in TypeScript. The display card copies `running_agents` only.

## DL-422: The chat of a workflow agent (B12)

- Decision: link method `session.workflow_agent` (feature `workflows.v1`, scope `read`) with `{session_id, agent_id}`; the station serves only an agent that it bound from its own projection when the run id came (station migration V45 `workflow_agent_logs`, key `(session_id, agent_id)`, with run id and launch facts) and reads `projects/<cwd>/<native>/subagents/workflows/<run_id>/agent-<agent_id>.jsonl` with the DL-191 checks and parser. Both hosts bind through one hook; a shim process keeps its launch facts in `process_launch_facts` for the takeover. Deck route `GET /api/sessions/{station}/{session}/workflow-agents/{agent_id}`, owner only. Risk high: adversary DESIGN REVIEW before the station and deck tasks.

## DL-423: Workflows in the Agents switcher (B12)

- Decision: one group per workflow (head with name, count and status), phase heads, agent rows with chips; an agent view with its facts and "Show its work"; the URL `agent=wf:<task_id>:<index>`; a one-line status under the Workflow card; "From workflow agent <label>" on its approvals. The board starts from the station's workflow page and applies the later events with the same rules. An older station shows the workflow with "This station does not report the agents of a workflow."

## DL-424: Agents in the fleet view (B19)

- Decision: the strip shows "Agents running" (subagents + running workflow agents, "N+" when partial) and "Background commands" (tasks that are neither); the header sentence of Fleet and Watch adds "N at work" (working or background live sessions plus their running agents; unreachable stations not counted, said in the title). Queued agents and shell tasks do not count.

## DL-425: Workflows after stop, takeover and resume (B12)

- Facts: Claude's interrupt kills running workflows (recorded between turns, sent outside Fleet Deck); Fleet Deck sends an interrupt only during a main turn; a resumed process reports an unfinished workflow `stopped` and does not continue it; a shim takeover keeps it running.
- Decision: Fleet Deck keeps its interrupt guards; between turns, Stop ends the workflow with the process. At a final status or the end of the process, the station projection and the board set the running and queued agents `stopped`; the board keeps the `local_workflow` kind over an `unknown` notification; agent chats stay readable from their binding rows; the launch facts of a shim process survive a takeover.

## DL-426: The workflow projection of the station (B12)

- Decision: the station keeps `workflow_runs`, `workflow_phases` and `workflow_agents` (migration V45), written in the event transaction with the view rules R1 to R6 (every running run, at most 32 ended runs per session, 1,000 agents and 100 phases per run). Link method `session.workflows` (feature `workflows.v1`, scope `read`) gives pages of at most 512 KiB with `as_of`; deck route `GET /api/sessions/{station}/{session}/workflows`, readable by the owner and the members of the session. The board reads the pages when a session opens and again when its event window passes `as_of`.

## Architect decisions (2026-10-23)

The architect answered the owner questions of §Owner questions. These answers override the draft where they differ.

1. Fleet total: should the header show sessions plus their running agents ('9 at work'), or agents only ('6 agents running')? — **The recommended default: Sessions plus agents, 'N at work', with the split in its title**
2. Claude's interrupt (the Stop button) also ends every running workflow. Should Stop warn about it when a workflow runs? — **The recommended default: No change in this track; note it for a later per-workflow stop item**
3. Should the wall show the 'at work' total? — **The recommended default: Not in this track; B17 decides (the count is already in the display card)**
4. May members of a shared session see workflow agents' task/result previews (max 200 characters), as they see tool inputs today? — **The recommended default: Yes, as other session events; the agent chat ('Show its work') stays owner only**

Migration numbers (architect, to avoid collisions between parallel tracks): station V43 is taken by the DL-402 fix (b0cdbf5f). Station: V44 resume-after-stop, V45 workflow-agents, V46 mcp-apps, V47 usage-cost. Deck: V48 wall-control, V49 the wake-key fix (G5), V50 usage-cost. A track uses only its own numbers; the merge order follows the numbers, else the architect renumbers at merge.

## Revision 2 (design review round 1)

Report: `~/fleetdeck-briefs/backlog/workflow-agents/adv-design-r1.md` (reviewed commit `00055138`). I checked every finding against the code before the change. All eight findings are confirmed.

| # | Finding | Check against the code | What changed |
| --- | --- | --- | --- |
| 1 | BLOCKER: one `workflow.updated` with 1,000 changed agents can pass the 1 MiB control frame and stop the station stream; the resends and stops have the same problem | Confirmed: `link.rs:17`, `control.rs:232-243`, `link/stream.rs:273-298` send each event whole and return the error | §4.1 rule 9: the changes of a frame go out in events of at most `WORKFLOW_EVENT_BUDGET` (256 KiB, measured JSON), each with the same absolute fields (count semantics across chunks). Rule 4: no resend of agents (the station binds from its projection). Rule 6: a final status carries no agents (R2 at the receivers). Pages of `session.workflows` are at most 512 KiB. Tests: T2 `a_maximum_frame_splits_under_the_frame_limit` (through `ControlFrame::Event` encoding), T3b `a_maximum_run_is_paged_under_the_budget` (through `ControlFrame::Response`), T2 stop test without agent entries |
| 2 | MAJOR: the binding hook misses the shim host, the Linux default | Confirmed: `shim_target.rs:694-744` (hook `:732`), `config.rs:96-105` | §4.4: one shared hook `workflows::observe` replaces the subagent line in `pump.rs:68` and `shim_target.rs:732`. T3a owns both files and has the shim-host test `a_shim_hosted_session_binds_workflow_agents` (rig of `shim_resume_rig.rs`) |
| 3 | MAJOR: after a takeover the session has `launch: None`, so agents of later phases cannot be bound | Confirmed: `shim_target.rs:167-171` | §4.4: new table `process_launch_facts`; `workflows::launch::settle` stores the facts at launch and reads them at the adoption (same process incarnation and session). Test `a_restart_between_phases_binds_the_later_agent`; threat note "Launch facts" |
| 4 | MAJOR: the board view needs history that the deck (500) and the browser (1,000) drop | Confirmed: `read_model.rs:31,129-137`, `state.ts:225` | New §4.3a view rules; station projection (`workflow_runs`, `workflow_phases`, `workflow_agents`) in the event transaction; link method `session.workflows` and deck route (member readable) with pages and `as_of`; board hook `useWorkflows` loads the pages and reloads when the window passes `as_of`. Tests: T3a `three_thousand_events_keep_every_agent`, T5 cold load and continued viewing beyond the window |
| 5 | MAJOR: the fold keeps a vanished workflow while another workflow runs | Confirmed: the draft rule needed an empty list or no `local_workflow`; `fold.rs:158` treats the list as complete | §4.3: `background.updated` removes every entry that it does not list; ordering protection stated separately (list before start, no running event after the end, a later running event adds it again). Same rule R3 in the projection and the board. Tests: T1 `background_updated_removes_a_missing_workflow_while_another_runs`, T3a and T5 twins |
| 6 | MAJOR: the documented interrupt of a background workflow cannot pass Fleet Deck's guards | Confirmed: `controller.ts:33,45`, `decide.rs:197-203`, `core.rs:432-441` | Option "correct the behavior": §2.4 note, §4.8 table split into "interrupt during a main turn" and "workflow runs, no main turn" (Stop ends it with the process), §4.1 "Interrupt: no change", decision 19, DL-425, owner question 2 correction. Tests through the Fleet Deck command path: T3a `an_interrupt_between_turns_is_refused_while_a_workflow_runs` (manager receipt), T2 core refusal, T5 controller flags |
| 7 | MAJOR: the caps (16 in the fold, 32 in the core with eviction of running entries) break the total and lose run ids | Confirmed in the draft text (§4.1 limits, §4.3) | The core follows at most 64 running workflows and **never** removes a running entry (ended entries capped at 32 separately); more are `untracked` (one event at start and end). The fold keeps up to 256 entries and sets `agents_partial`; the board shows "N+" with a note. The checkpoint keeps a digest per agent to bound its size. Tests: T2 `the_65th_running_workflow_is_untracked`, T1 `at_most_256_running_workflows_then_partial`, T6 "N+" tests |
| 8 | MINOR: the downgrade claim is wrong; an older binary refuses the database | Confirmed: `db.rs:220-229` (`TooNew`), backup in `db_backup.rs:70` | §5 Migration: forward-only schema and the rollback procedure with the pre-upgrade backup; §4.9 bullet. Fixed although MINOR, because the text was wrong |

Findings not fixed: none.

Other changes that the fixes need:

- Task split: T3 became **T3a** (projection, launch facts, binding on both hosts; depends on T1) and **T3b** (the two methods, path, intake, method table; depends on T3a). The adversary gate covers T3a, T3b and T4.
- The feature is now `workflows.v1` (`WORKFLOWS`) for both methods; the 409 code is `workflows_unsupported`.
- The migration is V45 (architect numbering), with five tables.
- New code avoids `history/mod.rs` (798 lines) and `manager/mod.rs` (797 lines): the new top-level module `crates/fleetdeck-station/src/workflows/`, the shim test declared from `shim_target.rs`, the intake handlers in `link/intake_workflows.rs`.
- `commands.ts` moved from T5b to T5 (T5 needs the `workflows` action for the loading).
