# Running subagents and tasks in the chat view (B6)

- **Status:** Accepted, revision 1 (2026-10-16). Risk: normal (owner reads of data the station already has); one Codex review of the branch.
- **Owner request:** "there is no way in the chat view to visualize all the subagents, and tasks currently running … switch from agent to agent to see their chat windows, just like it works in claude code".

## 1. Facts (code map, 2026-10-16)

- The board builds subagent cards from `tool.started` (`tool_kind: subagent`); `tree.subagents` (all of them, outermost first) is computed in `web/src/session/timeline.ts` and not used.
- Claude sends `system/task_started`, `task_updated`, `task_notification` (with `task_id`, `tool_use_id`, `task_type` `local_agent`/`local_bash`/…, `description`, `status`, `summary`); `claude/map.rs` drops them. The card binds to a task by guess.
- Claude: a background subagent's text after the parent turn ended is dropped (`MessageCompleted` needs a turn), and the parent `result` closes every open tool as `Failed`, also the running tools of a live background agent (`close_open_tools`).
- Codex: `collabAgentToolCall` items carry `agentsStates { <thread>: { status, message } }`; child `turn/started`/`turn/completed` and `thread/status/changed` are ignored; a `spawnAgent` card shows done at once.
- `ChatView` renders any `Item[]`, so a subagent's `items` render as a chat without change. The DL-191 log (`session.subagent`) is the full record of a Claude subagent.

## 2. Design

1. **Event `task.updated`** (proto, additive): `TaskUpdated { task_id: String, node_id: Option<String>, task_kind: String, status: String, description: Option<String>, summary: Option<String> }`. `status`: `running`, `completed`, `failed`, `stopped`, or the provider's text. The fold keeps no new state; the board reads the events.
   - Claude: `task_started` → `running` with `description`; `task_updated` → its `patch.status`; `task_notification` → its `status` and `summary`. `node_id` = the tool node of `tool_use_id` (the Agent/Task card or the Bash card).
   - Codex: `agentsStates` on a collab item → one `task.updated` per thread (`task_id` = thread id, `node_id` = the collab item's node, `task_kind: codex_agent`, `status` mapped: `pendingInit`/`running` → `running`, `completed` → `completed`, `errored`/`failed` → `failed`, else the text; `summary` = `message` cut to 200 characters); child `turn/started` → `running`, child `turn/completed` → `completed` for the node that names the thread. New module `codex/agents.rs` (`codex/map.rs` is at 784 lines).
2. **Claude mapping fixes** (new helper file, `claude/map.rs` is at 674 lines):
   - A child message (`parent_tool_use_id` set) whose parent turn ended is emitted with the turn id of the turn that launched the subagent (kept per Agent/Task tool node).
   - At the parent `result`, open tools whose parent node is a live background agent (its task is `running`) stay open; their later results complete them.
3. **Board: the agents switcher.** In the chat tab, when the session has at least one subagent or background task, a control "Agents (N running)" opens a list: "Main chat", then each subagent (title, input, status from `task.updated` when present, else from the card), then each background command task (`local_bash`: the Bash card's command, status, summary). Choosing a subagent shows `ChatView` with that subagent's `items` (live), a header "‹ Main chat · <title> · <status>", and the DL-191 "Show its work" log below for the full record (owner only). The composer is hidden in an agent view, with the line "A subagent takes no input. Write in the main chat." Choosing a background command task scrolls the main chat to its Bash card and highlights it (no output: the task output file is not read). The choice is in the URL (`agent=<node_id>`); a back action returns to the main chat. Phone: the control is a select in the filter row; the agent view uses the full width.
4. **Status on the card.** Subagent cards show the `task.updated` status when present (exact binding by `node_id`), else today's status.

## 3. Tests

- Providers: the Claude fixture `spikes/s1-claude/fixtures/rust/subagent.jsonl` gives `task.updated running` then `completed` with the summary `sub-ok` and the child text after the parent `result` (no longer dropped); `background_task.jsonl` gives `local_bash` running/completed bound to the Bash node; no child tool is closed as failed while its agent runs. Codex `spikes/s2-codex/fixtures/subagent.jsonl` gives `running` then `completed` with `sub-ok`.
- Board: the switcher lists main, subagents and tasks with statuses; choosing one shows its items and the header; the composer is hidden there; the URL parameter restores the view; a background task choice highlights its card; phone layout.

## 4. Out of scope

- Reading background command output files. Sending input to a subagent. A Codex subagent log (Codex has no `session.subagent`).
