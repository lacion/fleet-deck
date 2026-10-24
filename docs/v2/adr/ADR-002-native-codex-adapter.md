# ADR-002 — Native Codex adapter (`codex app-server` JSON-RPC)

- **Status:** Accepted (owner, 2026-10-11)
- **Date:** 2026-10-11
- **Deciders:** owner (accepted 2026-10-11), architect
- **Implements:** PLAN A8 (native adapters), A9, A10, §6.2–§6.4, §14 Phase 0 S2; lessons L1–L4, L6–L8, L12; t3code-lessons §5 traps 1–10
- **Evidence:** spike S2 (`spikes/s2-codex/`), decisions DL-012 to DL-015 in `docs/v2/DECISIONS-LOG.md`
- **Verified against:** `codex-cli` 0.160.0 (`codex app-server`), model `gpt-5.6-luna`, effort `low`

## 1. Context

PLAN §14 Phase 0 asks spike S2 to prove the Codex app-server protocol from Rust: `initialize`, `thread/start`, `turn/start`, both approval requests, the response channel of questions (L4: RPC reply or new user message), `turn/interrupt`, `thread/resume`, and diff notifications, and to record the shared scenarios of §13. PLAN also asks this ADR to choose the protocol source: "depend on `codex-app-server-protocol` (git pin) **or** codegen from `codex app-server generate-json-schema`".

`codex app-server` speaks JSON-RPC 2.0 without the `jsonrpc` member, one JSON object per line on stdin/stdout. The client sends requests and notifications. The server sends responses, notifications, and its own requests (approvals, questions), which the client answers with a response that has the same `id`. The CLI can write the full protocol as JSON Schema (`codex app-server generate-json-schema --experimental`): 167 client requests, 11 server requests, 83 notifications in 0.160.0.

## 2. Decision

### 2.1 Protocol source (the PLAN §14 choice)

**Choice: a narrow form of the PLAN's codegen option (DL-012).** The station vendors the generated schema of the pinned CLI version, unchanged, at `schema/<version>/codex_app_server_protocol.schemas.json`. The adapter has hand-written serde access for the subset that it uses (about 20 message shapes). Tests bind the subset to the schema:

- `tests/schema_methods.rs`: every method that the core sends or matches (`CLIENT_REQUESTS`, `HANDLED_REQUESTS`, `DECLINED_REQUESTS`, `HANDLED_NOTIFICATIONS`, `IGNORED_NOTIFICATIONS`) exists in the schema; the approval decision names exist; the async question fields exist.
- `tests/schema_conformance.rs`: a small JSON-schema subset checker (`$ref`, `type`, `enum`, `required`, `properties`, `additionalProperties`, `items`, `anyOf`/`oneOf`/`allOf`) validates every recorded frame. Host writes are checked strictly (an undeclared member such as a misspelled parameter fails; `the_checker_rejects_a_misspelled_parameter`), including the values of the question answer map (`the_checker_validates_answer_map_values`). Provider notifications, server requests, and the responses to every client request that the core sends (`provider_responses_conform`, `the_checker_rejects_a_response_without_a_required_member`) are checked for required members and types.

Rejected: the git pin of `codex-app-server-protocol` (at `rust-v0.160.0` it depends on about 15 internal monorepo crates, a heavy unpublished tree for 20 shapes), the stale crates.io release (0.63.0), and full codegen with `typify` (alpha, hundreds of unused types). A CLI upgrade writes a new schema directory; the two schema tests and the fixture replay show the differences.

### 2.2 Process model

- Spawn `codex app-server` with the session `cwd`, stdin/stdout piped, stderr discarded, `kill_on_drop`. One process per station adapter; the spike uses one thread per process.
- `initialize` with `clientInfo` and `capabilities.experimentalApi: true` (DL-013: the RPC question channel and `collaborationMode` exist only with the experimental API). The reply has `userAgent` `"<client>/<version> (…)"`; the core reads the provider version from it and then sends the `initialized` notification.
- `thread/start` with `cwd`, `model`, `approvalPolicy`, `sandbox`, `ephemeral: false`, or `thread/resume { threadId, … }`. The reply starts the session (`session.started` with `thread_id`).
- Close: the host closes stdin; the process exits with code 0 (all fixtures). Kill: `approval_restart`.
- Recordings use the user's Codex home (global `AGENTS.md`, MCP servers, `sessionStart` hooks run). Codex has no `--safe-mode`; the scrubber removes account data and local paths (DL-014). Phase 1 decides whether product sessions pass `-c` overrides to isolate the session (§8).

### 2.3 Code structure

The spike has the same three layers as S1 (ADR-001 §2.2); Phase 1 moves them to `crates/provider-codex`:

- `rpc.rs`: line → `Incoming { Response, ErrorResponse, Request, Notification, Unknown }`; never panics (proptest); builders for requests, notifications, replies, error replies.
- `adapter/` (sans IO): `CodexCore` takes host commands and incoming frames and returns `Output::{Emit(CanonEvent), Write(Value)}`. `requests.rs` holds the server requests; `map.rs` maps notifications.
- `driver.rs`: the tokio process driver; `record/` (`record-codex`); `replay.rs` (`replay-codex`).

### 2.4 Host commands and their wire frames

| Host command | Wire | Notes |
| --- | --- | --- |
| `initialize` | `initialize` request, then `initialized` | experimental API on |
| `open_thread` | `thread/start`, `thread/resume`, or `thread/fork` (resume + `fork`) | mode → `approvalPolicy` + `sandbox` |
| `prompt { text, input_id }` | idle: `turn/start { threadId, input, clientUserMessageId: input_id, effort }`; busy: held host-side | a `turn/start` during a turn joins the running turn (it acts as a steer), and `thread/queue/*` is experimental, so the core queues follow-ups itself (DL-015) |
| `steer { text, input_id }` | `turn/steer { threadId, expectedTurnId, input, clientUserMessageId }` | `NoTurn` if no turn runs |
| `answer { request_id, option_id, updated_input?, message? }` | reply to the server request, or a new user message (§2.6) | idempotent |
| `interrupt` | `turn/interrupt { threadId, turnId }` | held until `turn/started` (§2.8) |
| `set_mode { mode }` | `approvalPolicy` + `sandboxPolicy` on the next `turn/start` | `mode.changed` when that turn is accepted |
| `set_collaboration { mode }` | `collaborationMode { mode: plan\|default, settings }` on the next `turn/start` | plan capture, RPC questions |
| `list_background` | `thread/backgroundTerminals/list { threadId }` | reply `data[]` → `background.updated` (DL-011); the only way to see work that outlives the turn (§2.10) |

Codex has no separate `set_model` request: `turn/start` and `thread/resume` take `model`. Phase 1 maps `LiveSession::set_model` to the next `turn/start`, as for modes. Fork: `thread/fork` of a finished thread returns a new thread id with the source's context (`fork_thread`, `matrix::fork_gives_a_new_thread_with_the_old_context`). The fork's rollout does not repeat the source's client ids.

### 2.5 Provider frames → canonical events

| Codex frame | Canonical event |
| --- | --- |
| reply to `thread/start` / `thread/resume` | `session.started { provider: codex, provider_version, model, mode, provider_refs.thread_id }` |
| reply to `turn/start` (own thread) | `input.updated { received }`; the first time for a native turn id also `turn.started`; a pending mode → `mode.changed` |
| `turn/started` (own thread) | `turn.started` if not started yet; sends a held interrupt |
| `item/started` `userMessage` with `clientId` | `input.updated { started }` = **prompt confirmed** (§3.1) |
| `item/agentMessage/delta` | `message.delta` |
| `item/completed` `agentMessage` (own thread, text not empty) | `message.completed`; `questions` with `delivery: async` → `approval.requested { response: message }` |
| `item/reasoning/summaryTextDelta`, `item/reasoning/textDelta` | `reasoning.delta` |
| `item/started` / `item/completed` `commandExecution`, `fileChange`, `mcpToolCall`, `webSearch`, `collabAgentToolCall`, … | `tool.started` / `tool.completed { ok \| failed \| denied }`; tool items of a child thread get `parent_node_id` = the collab call that named the thread in `receiverThreadIds` (`spawnAgent` names it only in its completed item) |
| `item/completed` `fileChange` (completed) | `file.changed` per change (`add`/`delete`/`update` → added/deleted/modified) |
| `turn/diff/updated` | keeps the aggregated diff; each path that no item reported yet → `file.changed` (paths joined with the thread cwd before the duplicate check) |
| `item/completed` `plan` | `plan.proposed { markdown }` |
| `thread/tokenUsage/updated` | `usage.updated` |
| `serverRequest/resolved` | answered: silent confirmation; unanswered: `approval.expired { cancelled_by_provider }` (DL-010) |
| `turn/completed` (own thread, own native turn) | open requests → `approval.expired { turn_ended }`, open tools closed, `turn.completed { done \| interrupted \| failed }`, inputs of the turn → `completed` / `cancelled`, next follow-up dispatched |
| `error` | `error`; `codexErrorInfo` `usageLimitExceeded` → code `usage_limit` (trap 7) |
| a frame of another thread (child agent) | `turn/started` / `turn/completed` ignored; a second native turn id never replaces the running turn |
| known noise (`thread/status/changed`, `mcpServer/*`, `account/*`, `hook/*`, `deprecationNotice`, …) | ignored (`IGNORED_NOTIFICATIONS`) |
| anything else | counted as unknown (L12: the raw frame stays in the station's raw log); `replay-codex` fails on an unknown frame |

### 2.6 Approvals and questions (L4)

**Approvals.** `item/commandExecution/requestApproval` → kind `command`; `item/fileChange/requestApproval` → kind `file_change`. Both are answered by an **RPC reply** `{ "decision": <native value> }`.

- Option ids are the native decision values. A string decision is its own id; an object decision such as `{ "acceptWithExecpolicyAmendment": { "execpolicy_amendment": [...] } }` has its single key as id, and the reply sends the full object back. Without `availableDecisions` the core offers `accept`, `acceptForSession`, `decline`, `cancel`.
- Observed (0.160.0, `untrusted`): a command offers `accept`, `acceptWithExecpolicyAmendment`, `cancel` and **no `decline`**; a file change offers `accept`, `acceptForSession`, … The station's deny maps to `decline` when offered, else `cancel` (DL-015). `cancel` ends the turn as interrupted (`tool_call_deny`).
- Edited input before allow is **not supported**: an answer with `updated_input` fails with `Unsupported("edited input before allow")` (the decision has no input field).
- `decline`/`cancel` mark the tool node denied; its `tool.completed` is `denied`.

**Questions, channel 1: RPC reply.** `item/tool/requestUserInput { questions: [{ id, header, question, options, isOther, isSecret }], isBlocking }` → `approval.requested { request_kind: question, response: rpc_reply, options: [answer] }`. The answer must carry `updated_input: { "answers": { <question id>: { "answers": [..] } } }`. Before the write the core checks it against the request and `ToolRequestUserInputResponse`: a non-empty map, only question ids that the request asked, each value `{ "answers": [<string>, ..] }`. A bad answer fails with `InvalidAnswer`, writes nothing, and leaves the request open (`rpc_question_answers_are_validated_before_the_write`); a good one is replied unchanged. Recorded live: `question_rpc` (plan collaboration mode).

**Questions, channel 2: new user message.** An `agentMessage` item with `delivery: "async"` and `questions: [{ title, options }]` → one `approval.requested { response: message }` per question (`provider_refs.question_index`), so no question is dropped; options = the suggested answers plus `answer` (a free-text question offers only `answer`). The answer is sent as a **new user message** (a `prompt` with input id `answer-<request>`; it waits host-side if a turn runs). With one question the message is the bare answer; with several, each answer names its question (`<title>: <answer>`) so that the agent can match it (`every_async_question_becomes_its_own_request`, `a_single_async_question_gets_the_bare_answer`). These requests stay open when the turn ends (the question outlives the turn) and expire only when the process is lost. In 0.160.0 the tool behind this channel (`request_user_input_async`) is behind the under-development feature `send_message_to_user_async`; a live probe with `--enable send_message_to_user_async` did not expose the tool to the model ("I can't access `request_user_input_async`"). The channel is therefore proven by the unit test `async_questions_are_answered_by_a_new_message` and the schema test `async_question_fields_exist_in_the_schema`, not by a recording. The capability matrix states this.

**Fail closed.** `mcpServer/elicitation/request` gets the fixed reply `{ "action": "decline" }` and an `error { unsupported_request }`. Every other server request (`item/permissions/requestApproval`, `item/tool/call`, `account/chatgptAuthTokens/refresh`, the legacy `applyPatchApproval`/`execCommandApproval`, …) gets the JSON-RPC error `-32601` and an `error` event; nothing is ever approved by default (`unknown_server_requests_fail_closed`).

**Idempotence.** The station addresses requests by app ids `req-N` of one core (one process incarnation). A second answer returns the first receipt with `duplicate: true` and writes nothing, also after `serverRequest/resolved` or the end of the turn; an answer to a request that expired unanswered fails with `Expired` (`answers_stay_idempotent_after_resolution_and_turn_end`). Native request ids restart at `0` in each process, so a native id must never be used to route an answer across a restart.

### 2.7 Permission modes (L8)

A mode is `<approvalPolicy>/<sandbox>` with only native values: policies `untrusted`, `on-request`, `never`; sandboxes `read-only`, `workspace-write`, `danger-full-access` (DL-013). A new session with no stored mode uses `untrusted/workspace-write`; an unknown stored mode or a sentinel such as `default` runs as `untrusted/read-only` (trap 2). `set_mode` applies with the next accepted `turn/start` (`set_mode` fixture: `mode.changed` before turn 2, `sandboxPolicy: readOnly` on the wire). The station never offers `never/danger-full-access` outside a temporary directory.

### 2.8 Interrupt (traps 3 and 10)

`turn/interrupt` needs the native turn id **and** a started turn: sent after the `turn/start` reply but before `turn/started`, it fails with `-32600 "no active turn to interrupt"` (first `interrupt_right_after_send` recording). The core holds an interrupt until `turn/started` and then sends it once; repeated interrupts write nothing. The interrupt is a request first (trap 10): the reply `{}` acknowledges it, and the turn is interrupted only when `turn/completed { status: interrupted }` arrives. An interrupt during a running command ends the tool as `failed` and the turn as `interrupted` (`interrupt_during_tool`).

### 2.9 Resume

`thread/resume { threadId }` in a new process continues the same thread with its context (`provider_thread_resume`: same thread id, the model remembers the word). The reply lists the earlier turns with their items, including `userMessage.clientId` (`the_resume_reply_lists_earlier_prompts_by_client_id`). 0.160.0 also sends `deprecationNotice` "Full-history hydration is deprecated for paginated threads; use `excludeTurns: true`, then page with `thread/turns/list` and `thread/items/list`." Phase 1 resumes with `excludeTurns: true` and reads history with `thread/items/list` when it needs it (§7).

### 2.10 Terminal paths and other traps

| Trap (t3code-lessons §5) | Codex behaviour | Adapter rule |
| --- | --- | --- |
| 1 setting before advertised | modes/models apply per turn | pending mode is applied and announced only when a turn is accepted |
| 2 `default` sentinel | not a native mode | `untrusted/read-only` |
| 3 stop before the turn started | `-32600` | held interrupt (§2.8) |
| 4 provider-started work | a command that outlives its turn sends no notification; its `item/completed` arrives after `turn/completed` (`background_terminal`) | the adapter polls `thread/backgroundTerminals/list` after `turn.completed` until it is empty and reports `background.updated`; a `commandExecution` still open when the turn completes **normally** stays open as background work and closes with its real status when its `item/completed` arrives (`matrix::a_background_command_completes_after_the_turn_with_its_real_status`, DL-025); frames of unknown turns never confirm inputs |
| 5 subagents outlive the run | collab agents run on child threads; the child's turn and items stream on the same connection (`subagent`) | child-thread turn frames never touch the parent turn; child tool items link to the collab call; child messages are not parent messages |
| 6 close everything on terminal paths | — | `turn/completed` closes open requests and open tools as `failed`, except commands of a normally completed turn (trap 4 row); an interrupted or failed turn closes its commands too (`matrix::a_command_of_an_interrupted_turn_closes_with_the_turn`); process exit closes everything, background commands included, and discards waiting follow-ups (`exit_expires_requests_and_discards_follow_ups`, `background_commands_close_when_the_process_ends`) |
| 7 usage limits | `error.codexErrorInfo.usageLimitExceeded` | `error { usage_limit }` (`usage_limit_errors_get_their_own_code`) |
| 8 symlinked workspaces | item paths are absolute, diff paths relative | join with the thread cwd before comparing; Phase 1 canonicalizes the cwd once at session start |
| 9 unknown sign-in | `account/read` exists | Phase 1 `probe()` reports unknown unless `account/read` gives a definite answer |
| 10 interrupt is a request | reply `{}` before `turn/completed` | turn ends only on `turn/completed` |

## 3. Effect states and crash boundaries (PLAN §6.4)

### 3.1 What "confirmed" means for Codex

| Effect | Claimed (before the write) | Confirmed | Not a confirmation |
| --- | --- | --- | --- |
| prompt | `turn/start` with `clientUserMessageId = input_id` | `item/started` `userMessage` with `clientId == input_id` on the own thread (`a_prompt_is_confirmed_by_its_user_message_item`) | the `turn/start` reply (`received`: Codex accepted the request; for a busy thread it returns the running turn); our own write |
| steer | `turn/steer` with `clientUserMessageId` | the `userMessage` item with that `clientId` in the running turn (`a_steer_joins_the_running_turn`) | the `turn/steer` reply |
| follow-up | held host-side; its `turn/start` is claimed when the previous turn completes | as prompt | — |
| answer (RPC) | the reply to the native request id | `serverRequest/resolved` for that request id (recorded after every answer) | our write |
| answer (message channel) | a `prompt` with input id `answer-<request>` | as prompt | — |
| interrupt | `turn/interrupt` | reply `{}` = acknowledged; `turn/completed { interrupted }` = done | — |
| set mode | the next `turn/start` with `approvalPolicy`/`sandboxPolicy` | the reply to that `turn/start` (`mode.changed`) | — |

A refused `turn/start` (JSON-RPC error) marks the input `refused` (`a_refused_turn_start_marks_the_input_refused`): the effect is settled without dispatch.

### 3.2 Crash boundaries

The MVP has no session shim (A9): a station crash also ends `codex app-server` (A10). The provider incarnation is the child process.

| Crash point | State after recovery | Reconciliation (no resend) |
| --- | --- | --- |
| before the command transaction commits | nothing exists; the deck retries the same `command_id` | — |
| after commit, before the claim | `queued` for a dead incarnation → `retired_not_dispatched` | — |
| after the claim, before the write | `outcome_unknown` (indistinguishable from the next row) | history lookup (below) |
| after the write, before the `userMessage` item | `outcome_unknown` | history lookup |
| after the item, before the station stored it | `outcome_unknown` | history lookup; here the client id is expected |
| provider process dies, station alive | open requests → `approval.expired { process_lost }`; open turn → `failed`; open tools closed; waiting follow-ups `discarded`; unconfirmed inputs → `outcome_unknown` | history lookup after `thread/resume` |
| station restarts | open requests of the dead process → `approval.expired { not_resumable }` (A10); the session is "resumed", not "reattached" | same |
| effect worker restarts, process alive | claimed effects stay `outcome_unknown` until the live core reports the `userMessage` item (late confirmation) | the live item frames |
| stale approval reply (after a restart) | the old app id is expired; the station rejects the answer ("request expired"). It must never be sent to the new process: native ids restart at 0, so the stale reply could hit an unrelated new request | — |

**History lookup (one-sided).** After `thread/resume`, the station searches the thread history for a `userMessage` with the effect's `clientId`: the `thread/resume` reply (or `thread/items/list` with `excludeTurns: true`, §2.9), and as a second source the rollout file named by `thread.path`.

- **client id found** → `confirmed` (delivered). The rest of that turn shows whether it ran (`the_resume_reply_lists_earlier_prompts_by_client_id`, `the_rollout_keeps_every_client_id`).
- **client id not found** → the effect **stays `outcome_unknown`**. A missing item does not prove non-delivery: the crash can come after Codex read the request and before it persisted the item. The spike has no crash-timing evidence that rules this out.
- In both cases the station **never resends automatically**. The owner decides whether to send a new command (a new effect with a new client id).

### 3.3 Open requests survive a provider restart: **no**

After a kill with an open command approval, a new process that resumes the same thread does not ask again without input ("approval asked again by the resumed process without input: false"). After a new prompt it issues a new request with a new native id (`approval_restart`, `an_open_approval_does_not_survive_a_restart`). The station expires open requests when the process is lost and never offers them as answerable after a restart (PLAN §6.5).

## 4. Fixture set and tests

Fixtures: `spikes/s2-codex/fixtures/*.jsonl` (format in `spikes/README.md`), recorded by `record-codex` in temporary directories, scrubbed (`fixture_hygiene.rs`).

| Scenario | Shared (§13) | What it shows |
| --- | --- | --- |
| `simple` | yes | handshake, one turn, prompt confirmation |
| `multi_turn` | yes | two turns on one thread |
| `queued_turn` | yes | host-side follow-up queue: two turns (DL-015) |
| `turn_interrupt` | yes | interrupt of a text turn; next turn runs |
| `message_steering` | yes | `turn/steer` joins the running turn |
| `provider_thread_resume` | yes | `thread/resume` in a new process keeps context |
| `tool_call_approve` | yes | command approval, native options, `accept` |
| `tool_call_deny` | yes | `cancel` (no `decline` offered), tool denied, file not created |
| `question_rpc` | — | L4 RPC question channel |
| `plan_capture` | — | plan collaboration mode, `plan.proposed` |
| `file_change` | — | file-change approval, item + diff notifications, one `file.changed` |
| `interrupt_during_tool` | — | interrupt during a running command |
| `interrupt_right_after_send` | — | trap 3, held interrupt |
| `set_mode` | — | mode change applied with the next turn |
| `approval_restart` | — | kill with an open approval, resume, new request |
| `fork_thread` | — | `thread/fork`: new thread id, context kept |
| `subagent` | — | `spawnAgent` + `wait` collab calls, child-thread frames on the same connection |
| `background_terminal` | — | a command outlives the turn; found by `thread/backgroundTerminals/list` (1, then 0) |

All 8 shared scenarios are supported; none is unsupported. Tests: `src/adapter/tests.rs` and `tests_review.rs` (core), `src/rpc.rs` (framing, proptest), `tests/replay_fixtures.rs` (every fixture replays with identical host writes; a changed write fails), `tests/evidence.rs`, `tests/matrix.rs` (PLAN §6.2 matrix cells), `tests/schema_methods.rs`, `tests/schema_conformance.rs`, `tests/fixture_hygiene.rs`. Offline command: `cargo run -p s2-codex --bin replay-codex` and `cargo nextest run -p s2-codex`.

## 5. Coverage of PLAN §14 Phase 0, S2

| Must prove | Section | Evidence |
| --- | --- | --- |
| initialize | §2.2 | `evidence::initialize_and_thread_start_open_the_session`; all fixtures |
| `thread/start` | §2.2, §2.4 | `evidence::initialize_and_thread_start_open_the_session`; `schema_conformance::host_writes_conform_strictly` |
| `turn/start` | §2.4, §3.1 | `evidence::a_prompt_is_confirmed_by_its_user_message_item`; `queued_prompts_run_as_separate_turns` |
| both approval requests | §2.6 | `evidence::approval_option_ids_are_native_decisions` (command), `evidence::file_change_approvals_use_native_decisions` (file change); fixtures `tool_call_approve`, `tool_call_deny`, `file_change` |
| response channel of questions (L4): RPC reply | §2.6 | `evidence::questions_use_the_rpc_reply_channel`; fixture `question_rpc`; `request_user_input_is_answered_by_rpc_reply` |
| response channel of questions (L4): new user message | §2.6 | `async_questions_are_answered_by_a_new_message`, `every_async_question_becomes_its_own_request`; `schema_methods::async_question_fields_exist_in_the_schema` (no live fixture: feature-gated in 0.160.0) |
| `turn/interrupt` | §2.8 | `evidence::an_interrupt_before_turn_started_is_held`, `evidence::an_interrupt_during_a_tool_ends_both`; fixture `turn_interrupt` |
| `thread/resume` | §2.9 | `evidence::thread_resume_keeps_the_thread_and_its_context`, `evidence::the_resume_reply_lists_earlier_prompts_by_client_id` |
| diff notifications | §2.5 | `evidence::diff_notifications_report_each_file_once`; `file_changes_come_from_items_and_the_turn_diff_once` |
| shared replay scenarios (§13) | §4 | `replay_fixtures::every_fixture_replays_with_identical_host_writes` (requires all 8) |
| protocol source (git pin or codegen) | §2.1 | `schema_methods`, `schema_conformance`; DL-012 |
| open requests survive restart (§6.2) | §3.3 | `evidence::an_open_approval_does_not_survive_a_restart` |
| prompt confirmed (§6.4) | §3.1 | `evidence::a_prompt_is_confirmed_by_its_user_message_item`, `the_rollout_keeps_every_client_id` |

## 6. Alternatives considered

- **Git pin of `codex-app-server-protocol`** and **full `typify` codegen**: rejected in §2.1.
- **`codex exec --json`**: one process per turn, no approvals or questions over the wire; rejected (no live session).
- **The stable API only** (no `experimentalApi`): no RPC question channel and no `collaborationMode`; rejected for the spike (DL-013). Phase 1 keeps the experimental surface to `item/tool/requestUserInput` and `collaborationMode`, and the schema tests show any change.
- **Codex's own queue** (`thread/queue/*`): experimental; the host-side queue gives the same behaviour with stable requests (DL-015).

## 7. Consequences and risks

- Experimental API: `item/tool/requestUserInput` and `collaborationMode` can change without notice. The schema tests and the nightly live smoke (A8) detect it.
- Full-history hydration on resume is deprecated (§2.9); Phase 1 must page history.
- The async question channel is unrecorded; when Codex enables it, record a fixture and update the matrix.
- Recordings depend on the user's Codex home (DL-014). Product sessions may need config overrides so that a user's global hooks or MCP servers do not change station behaviour.
- `file.changed` can name files outside the workspace; Phase 1 filters by the session workspace (as ADR-001 §7).
- Only one model and effort were recorded. The decision lists (`availableDecisions`) depend on the policy; the adapter must always use the offered list, never a fixed one.

## 8. Open points for the owner

1. Accept the vendored schema + subset + conformance tests as the protocol source (DL-012).
2. Accept the experimental API for questions and plan mode (DL-013).
3. Should product Codex sessions run with config overrides that disable the user's global hooks and MCP servers (DL-014)?
4. Deny = `decline` if offered, else `cancel`; `cancel` interrupts the turn (DL-015). Is that the wanted owner experience?
