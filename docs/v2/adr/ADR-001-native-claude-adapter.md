# ADR-001 — Native Claude Code adapter (stream-json control protocol)

- **Status:** Proposed
- **Date:** 2026-10-11
- **Deciders:** owner (approval pending), architect
- **Implements:** PLAN A8, A9, A10, D8; lessons L1–L4, L6–L8, L12; t3code-lessons §5 traps 1–10
- **Evidence:** spike S1 (`spikes/s1-claude/`), `spikes/s1-claude/EVIDENCE.md`, decisions DL-003 to DL-009 in `docs/v2/DECISIONS-LOG.md`
- **Verified against:** `claude` 2.1.289 (native build), TypeScript Agent SDK `@anthropic-ai/claude-agent-sdk` 0.3.296, model `haiku`

## 1. Context

PLAN A8/D8 decide a native Rust adapter for Claude Code. The TypeScript Agent SDK is only a behaviour reference. Spike S1 had to prove the rows of PLAN §14 Phase 0 for S1 and to give evidence for the PLAN §6.2 capability matrix and the PLAN §6.4 effect states.

The protocol is the CLI's stream-json mode (`claude -p --input-format stream-json --output-format stream-json`). It has no public specification. The CLI binary embeds zod schemas with field descriptions. The spike used these schemas, live recordings, and the SDK's own frames as the sources of truth.

## 2. Decision

### 2.1 Process model

One live session is one `claude` child process. The adapter starts it with these arguments (`adapter::cli_args`):

```
claude -p --input-format stream-json --output-format stream-json --verbose
       --include-partial-messages --permission-prompt-tool stdio --replay-user-messages
       --permission-mode <native mode>  [--model <id>]  [--resume <session> [--fork-session]]
```

- `--permission-mode` is always present (DL-004). The owner's settings can default to `bypassPermissions`. A session must not inherit a mode that the owner did not choose for it.
- `--model` is present only when the owner chose a model. The adapter never sends a placeholder model name (trap 2).
- The working directory is the session workspace. The station canonicalizes it before spawn, because the CLI keys its transcripts by the encoded cwd (trap 8).
- stdin carries host frames, stdout carries CLI frames (one JSON object per line), and stderr goes to the raw-frame diagnostic log (L12).
- Recordings use `--safe-mode` to keep the owner's hooks, plugins, and `CLAUDE.md` out of the fixtures (DL-005). Product sessions do not use `--safe-mode`; the owner's Claude setup applies.

### 2.2 Code structure

- **Frame parser** (`frames.rs`). A total function from one line to `CliFrame`. It never panics (property tests). It keeps an unknown frame as `CliFrame::Unknown` with its raw value.
- **Sans-IO core** (`adapter/`). `ClaudeCore` takes host commands (`HostCmd`) and CLI frames. It returns canonical events and frames to write (`Output::Emit`, `Output::Write`). It does no I/O, so the same core runs live, in replay, and in unit tests.
- **Driver** (`driver.rs`). A tokio task owns the child, writes `Output::Write` to stdin, feeds stdout lines to the core, and reports the exit to the core (`on_exit`).
- Phase 1 moves the core into `crates/provider-claude` behind the PLAN §6.2 `ProviderAdapter` / `LiveSession` traits. The driver becomes the session host of A9.

### 2.3 Host commands and their wire frames

| Host command | Wire frame (stdin) | Notes |
| --- | --- | --- |
| initialize | `control_request` `initialize` | First frame. The response has `commands`, `models`, `current_permission_mode`, `session_state`, `pending_permission_requests` |
| prompt / steer / follow-up | `user` message with a client `uuid` (UUID v4) and `session_id` | The `uuid` is the input id. A message written while a turn runs is a steer: the CLI drains it into the running turn after the current tool result (E2). A message written while another input waits is queued for its own turn |
| answer | `control_response` to the CLI's `control_request` id | §2.5 |
| interrupt | `control_request` `interrupt` | A request: the response says `still_queued`; the turn ends only with its `result` frame (trap 10) |
| set mode | `control_request` `set_permission_mode` | Native wire names only (§2.6) |
| set model | `control_request` `set_model` | Phase 1 validates the id against the `initialize` `models` list (trap 1) |
| stop | close stdin; kill after a grace period | §2.9 |
| resume / fork | new process with `--resume` (and `--fork-session`) | §2.8 |

The adapter allocates host request ids (`fd-N`). It ignores `control_response` frames for ids that it did not send, because the CLI echoes the host's own responses on stdout.

### 2.4 CLI frames → canonical events

| CLI frame | Canonical event (PLAN §6.3) |
| --- | --- |
| first `system/init` | `session.started` (session id, model, mode, version). Later `init` frames (one before each turn) are not new sessions |
| `command_lifecycle` `queued` / `started` / `completed` / `cancelled` / `discarded` / `refused` for a client `uuid` | input state (`input.updated` in the spike). `started` opens the turn (`turn.started` with `command_uuid`) or joins the running turn (steer) |
| `stream_event` `text_delta` / `thinking_delta` | `message.delta` / `reasoning.delta` |
| `assistant` `text` block (top level) | `message.completed` |
| `assistant` `tool_use` block | `tool.started` (`parent_node_id` from `parent_tool_use_id`); `ExitPlanMode` also gives `plan.proposed` |
| `user` `tool_result` block | `tool.completed` (`ok` / `failed` / `denied`); an `Edit`/`Write`/`MultiEdit`/`NotebookEdit` result also gives `file.changed` |
| `control_request` `can_use_tool` | `approval.requested` (`AskUserQuestion` → kind `question`) |
| `control_cancel_request` | `approval.expired` (reason `cancelled_by_provider`, DL-010) |
| `system/background_tasks_changed` | background set (`background.updated` in the spike; Phase 1: `run.completed.background_pending`, L7) |
| `result` | `usage.updated`, then `turn.completed` (`done` / `interrupted` / `failed`) |
| `control_response` to `set_permission_mode` | `mode.changed` |
| process exit | `approval.expired { process_lost }` for open requests, open tools `failed`, open turn `failed`, `session.ended` |

A turn that no host input started (the CLI works on after a background task or an async subagent finished) opens with `origin: provider`. Such a turn never confirms a host input (DL-006, traps 4 and 5).

Frames that arrive before the first `system/init` (for example `command_lifecycle queued`) are held and emitted right after `session.started`.

### 2.5 Approvals and questions

- The native request id is the CLI's `control_request.request_id` (a UUID). The adapter maps it to an app request id and back (L6).
- The CLI has **no option ids**. The native answer is `behavior: "allow"` (with `updatedInput`, optional `updatedPermissions`) or `behavior: "deny"` (with `message`, `interrupt`). The adapter offers these option ids (E4):

  | Option id | Native answer |
  | --- | --- |
  | `allow` | `allow` + `updatedInput` (the original input, or the owner's edited input) |
  | `allow_always` | `allow` + `updatedPermissions` = the CLI's allow-rule suggestions re-scoped to `destination: "session"`. Offered only when such a suggestion exists (DL-009) |
  | `deny` | `deny` + `message`, `interrupt: false` |

- Each answer echoes `toolUseID`, as the SDK does (DL-007).
- An answer is idempotent: a second answer returns the first receipt and writes nothing (L3). An unknown option id is rejected.
- Questions (`AskUserQuestion`) use the same channel: `allow` with `updatedInput.answers` (question text → chosen label). The response channel is `rpc_reply` for approvals and questions (L4).
- An unknown `control_request` subtype gets a `control_response` error. The adapter never answers an unknown request with "allow" (fail closed). `hook_callback` and `mcp_message` also get an error, because the station registers no hooks and no SDK MCP servers.

### 2.6 Permission modes (L8)

The wire names are `default`, `acceptEdits`, `plan`, `auto`, `dontAsk`, `bypassPermissions`. The CLI flag also accepts `manual`, and the CLI reports it as `default` (DL-003). A stored mode outside this list runs as `default`, the most restrictive mode. `bypassPermissions` is offered only when the station policy allows it.

### 2.7 Interrupt

`interrupt` is a request. The CLI answers `{"still_queued": []}`, then ends the turn with a `result` frame: `terminal_reason: "aborted_streaming"` (while text streams) or `"aborted_tools"` (during a tool). The adapter maps that turn to `interrupted`. The interrupted input ends `cancelled`. An interrupt written before any turn frame also works: the CLI starts and aborts the turn, and the next prompt runs normally (trap 3).

### 2.8 Resume and fork

`--resume <session>` keeps the session id and the context. `--resume <session> --fork-session` starts a new session id with the old context. Open approval requests do not survive (§3.3).

### 2.9 Terminal paths (trap 6)

On `result`, the adapter expires open requests (reason `turn_ended`, DL-010) and closes open tool nodes (`adapter::tests::terminal_paths_close_open_tools`). On process exit, it expires open requests (`process_lost`), closes open tool nodes as `failed`, closes the open turn as `failed`, and emits `session.ended`. Stop closes stdin and waits up to 30 s for the exit; then it kills the process (the spike kills the child; Phase 1 kills the process group, because background commands are grandchildren).

### 2.10 Other traps of t3code-lessons §5

| Trap | Handling |
| --- | --- |
| 1 setting before advertised | Modes and models are validated against the native lists from `initialize` before a write |
| 2 "default" sentinel | No placeholder model name; `default` is a real permission-mode wire name |
| 7 usage limits | The CLI has `rate_limit_event` frames and `result` subtypes `error_max_turns`, `error_max_budget_usd`. Phase 1 maps them to `error { code: usage_limit }` with the reset time. Not recorded: a limit cannot be provoked cheaply (matrix: "unverified") |
| 9 unknown sign-in | `claude auth status` prints JSON (`loggedIn`, `authMethod`). A parse failure or a non-zero exit is "unknown", not "logged out" |

## 3. Effect states and crash boundaries (PLAN §6.4)

### 3.1 What "confirmed" means for Claude

| Effect | `claimed` (persisted before the write) | `confirmed` | Not confirmation |
| --- | --- | --- | --- |
| prompt, steer, follow-up | claim with fence token + process incarnation, then the `user` frame with `uuid` = effect id | `command_lifecycle` `started` for that `uuid` | the pipe write; `queued`; the `isReplay` echo; `system/status`; a provider-origin turn |
| answer | claim, then `control_response` | the CLI's echo of that `control_response`, or the next frame for the same `tool_use_id` (`tool_result`, `tool_use` progress) | the pipe write |
| interrupt | claim, then `control_request` `interrupt` | `control_response` success for its request id | turn end (that is the `result`, a separate event) |
| set mode | claim, then `set_permission_mode` | `control_response` success (`{"mode": ...}`), then `system/init.permissionMode` | the pipe write |

`completed` (after the turn's `result`) is execution, not delivery. A client that sends no `uuid`, as the SDK does, gets no lifecycle frames and has no per-input confirmation (DL-008). The station adapter always sends a `uuid`.

### 3.2 Crash boundaries

The MVP has no session shim (A9), so a station crash also ends the `claude` child (A10). "Provider incarnation" is the child process (pid + start time).

| Crash point | State after recovery | Reconciliation (no resend) |
| --- | --- | --- |
| before the command transaction commits | nothing exists; the deck retries the same `command_id` | — |
| after commit, before the claim | effect `queued` for a dead incarnation → `retired_not_dispatched` | — |
| after the claim, before the write | `claimed` without confirmation → `outcome_unknown` | resume the session; the CLI transcript (`~/.claude/projects/<cwd>/<session>.jsonl`) has no user entry with that `uuid` → "not delivered". The owner decides on a new command |
| after the write, before `started` | `outcome_unknown` | transcript has the `uuid` → delivered; a later `result`/assistant entry shows if the turn ran. Transcript without the `uuid` → not delivered (it was still in the CLI's in-memory queue) |
| after `started`, before the station stored it | `outcome_unknown` | as above: the transcript has the `uuid` |
| provider process dies, station alive | open requests → `approval.expired { process_lost }`; open turn and tools → `failed`; inputs not yet `started` → `outcome_unknown` | same transcript lookup after `--resume` |
| station restarts | open requests of the dead process → `approval.expired { not_resumable }` (A10); session shown "resumed", not "reattached" | same |
| effect worker restarts, process alive | `claimed` effects stay `outcome_unknown` until the live core reports `started` (late confirmation) | the live lifecycle frames |
| stale approval reply (after a restart) | the request is expired; the station rejects the answer with "request expired". The CLI has no request with that id (a resumed CLI issues new ids) | — |

Evidence for the transcript rule: the CLI stores each prompt under the client `uuid` (`capabilities::the_transcript_keeps_prompts_under_the_client_uuid`).

### 3.3 Open requests survive a provider restart: **no**

After a kill with an open `can_use_tool` request, `claude --resume` reports `pending_permission_requests: []` and `session_state: "idle"` and does not ask again without new input. After a new prompt it issues a new request with a new native id (E3). So the station expires open requests when the process is lost and never offers them as answerable after a restart (PLAN §6.5).

## 4. Fixture set and tests

Fixture format (spikes/README.md): a JSONL header (`provider`, `scenario`, `cli_version`, `recorder`, `model`, `notes`), then records `spawn`, `host`, `in`, `out`, `raw`, `exit`, `note`. Fixtures are scrubbed (account data, home and temporary paths, also across streamed deltas).

| Scenario (PLAN §13 shared) | Rust fixture | SDK fixture |
| --- | --- | --- |
| `simple` | `rust/simple` | `sdk/simple` |
| `multi_turn` | `rust/multi_turn` | `sdk/multi_turn` |
| `queued_turn` | `rust/queued_turn` | `sdk/queued_turn` |
| `turn_interrupt` | `rust/turn_interrupt` | `sdk/turn_interrupt` |
| `message_steering` | `rust/message_steering` | `sdk/message_steering` |
| `provider_thread_resume` | `rust/provider_thread_resume` | `sdk/provider_thread_resume` |
| `tool_call_approve` | `rust/tool_call_approve` | `sdk/tool_call_approve` |
| `tool_call_deny` | `rust/tool_call_deny` | `sdk/tool_call_deny` |

All eight shared scenarios are supported. No scenario is unsupported for Claude.

Extra Rust fixtures: `tool_call_edit_input` (also SDK), `set_permission_mode` (also SDK), `interrupt_during_tool`, `approval_restart`, `fork_session`, `plan_capture`, `subagent`, `background_task`, `file_edit`, `ask_user_question`, `interrupt_right_after_send`.

Offline tests (no network, no `claude`):

- `replay-claude`: replays every Rust fixture with byte-identical host writes and every SDK fixture semantically (`reference.rs`, DL-008).
- `tests/replay_fixtures.rs`, `tests/reference_fixtures.rs`, `tests/event_snapshots.rs` (one `insta` snapshot per fixture), `tests/evidence.rs`, `tests/capabilities.rs`, `tests/fixture_hygiene.rs`, and the unit and property tests of the core.

Live tests: `record-claude` (Rust) and `sdk-recorder/` (SDK). PLAN §13 runs a nightly live smoke test with a cheap model, and the SDK changelog watch catches protocol changes that reach the SDK first (A8).

## 5. Coverage of PLAN §14 Phase 0, S1

| Must prove | Section | Evidence |
| --- | --- | --- |
| spawn | 2.1 | every fixture (`spawn` record) |
| initialize | 2.3 | every fixture; `initialize` response fields |
| prompt | 2.3, 3.1 | `rust/simple`; `evidence::prompt_confirmed_is_command_lifecycle_started` |
| partial deltas | 2.4 | `event_snapshots` `rust__simple` (`message.delta`) |
| `can_use_tool` allow / deny with edited input | 2.5 | `rust/tool_call_approve`, `rust/tool_call_deny`, `rust/tool_call_edit_input`; `evidence::approval_options_map_to_native_behaviors` |
| interrupt during a tool | 2.7 | `rust/interrupt_during_tool`; `replay_fixtures::interrupts_end_the_turn_as_interrupted` |
| resume | 2.8 | `rust/provider_thread_resume`, `sdk/provider_thread_resume` |
| set permission mode | 2.6 | `rust/set_permission_mode`; `adapter::tests::set_mode_accepts_only_native_modes` |
| unknown-frame handling | 2.2, 2.5 | `frames::tests::parser_never_panics_on_any_json`, `adapter::tests::unknown_control_request_fails_closed` |
| TS SDK frame recording + replay | 4 | `sdk-recorder/`, `reference_fixtures::every_sdk_fixture_replays_semantically` |
| shared replay scenarios (§13) | 4 | `replay_fixtures::every_fixture_replays_with_identical_host_writes` |
| response channel | 2.5 | `evidence::approval_options_map_to_native_behaviors`, `capabilities::questions_are_answered_by_rpc_reply` |
| open requests survive a restart | 3.3 | `replay_fixtures::open_approval_does_not_survive_a_restart` |

## 6. Alternatives considered

- **TypeScript SDK sidecar.** Rejected by the owner (D8). The advisor preferred it as a first step.
- **PTY / TUI scraping.** No structured approvals, no ids. Rejected.
- **Session shim in the MVP.** Deferred to Phase 7 (A9). The `SessionHost` boundary is the driver of §2.2.

## 7. Consequences and risks

- The control protocol is internal to the CLI and can change in any release. Controls: the version probe (`system/init.claude_code_version`), the tolerant parser, the SDK reference replay, and the nightly live smoke test. A gap is fixed in Rust or the capability is marked unsupported (A8).
- The prompt `uuid` is the key to delivery confirmation and to reconciliation. `command_lifecycle` is verified only on 2.1.289. A CLI version without it falls back to the `isReplay` echo of `--replay-user-messages`, which confirms receipt only (DL-006). The probe records which mechanism a version has (`system/init.capabilities` lists `msg_lifecycle_v1`).
- In `plan` mode the CLI writes the plan file to `~/.claude/plans/` with its own `Write` call, outside the workspace (`rust/plan_capture`). The `file.changed` event for that path is not a workspace change. Phase 1 filters `file.changed` to paths inside the workspace.
- Async subagents and background commands outlive the turn and start provider-origin turns. The UI shows a session with background work as busy (L7).

## 8. Open points for the owner

- Should product sessions run with the owner's Claude hooks and plugins (the default), or with `--safe-mode`? v0's own hooks would fire inside v2 sessions.
- Should `bypassPermissions` be offered at all on a station? The spike never used it.
