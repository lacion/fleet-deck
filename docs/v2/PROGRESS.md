# Fleet Deck v2 — progress (goal loop memory)

Current milestone: **M1 = PLAN Phase 0 (spikes S1, S2, S4)**. Max 15 rounds.

Rules for this file: one checklist item per M1 deliverable, in PLAN order. Each round updates the item status, the commands it ran, and the results. The work of a round stays uncommitted for the reviewer; the next round commits it first.

## M1 checklist

- [x] **P0-1 Scaffold.** Root Cargo workspace (spike members only, DL-001), `deny.toml`, lint policy (`unsafe_code = forbid`, `clippy::unwrap_used = deny` outside tests), `spike-canon` (canonical event subset of §6.3), `docs/v2/DECISIONS-LOG.md`, `spikes/README.md`, this file. *(round 1)*
- [x] **S1-1 Claude protocol core.** Frame parser (never panics, unknown frames kept, proptest), host control frames, sans-IO adapter core: init, prompt, partial deltas, `can_use_tool` with native option ids + edited input, idempotent answers, fail-closed unknown requests, interrupt, set mode (native modes only), exit expiry. *(round 1; round 2 review fix: a prompt is confirmed only by `command_lifecycle` `started` with the matching uuid, DL-006)*
- [x] **S1-2 Claude live driver + fixture format + `record-claude`.** tokio process driver around the core; JSONL fixture format (`spawn`/`host`/`in`/`out`/`raw`/`exit`/`note` records + header, scrubbed); `record-claude`; live recordings (haiku, `--safe-mode`, temp dir) of all 8 §13 shared scenarios plus `tool_call_edit_input`, `interrupt_during_tool`, `set_permission_mode`, `approval_restart`. *(round 2; an early replay harness + `replay-claude` is pulled forward from S1-4)*
- [x] **S1-3 TypeScript SDK reference recordings.** `spikes/s1-claude/sdk-recorder/` (bun + `@anthropic-ai/claude-agent-sdk` 0.3.296, `tee-claude.mjs` as `pathToClaudeCodeExecutable`); 10 SDK fixtures in `spikes/s1-claude/fixtures/sdk/` (8 shared + `tool_call_edit_input`, `set_permission_mode`); `reference` module + `replay-claude` replay them offline (DL-008). *(round 3)*
- [x] **S1-4 Claude replay + evidence.** *(round 2: `replay` module, `replay-claude`, `tests/replay_fixtures.rs`.)* *(round 3: replay rejects unmatched writes before `exit`; SDK reference replay.)* *(round 4: `insta` snapshots of the canonical event stream of all 22 fixtures; `tests/evidence.rs`; `spikes/s1-claude/EVIDENCE.md` for "prompt confirmed", queue/steer timing, restart survival, native option ids; SDK recorder exits nonzero and kills timed-out sessions + `selftest.sh`.)*
- [x] **S1-5 ADR-001** (Proposed): native Claude adapter design, effect states and crash boundaries (§6.4), fixture set, unsupported scenarios with reasons. *(round 5: `docs/v2/adr/ADR-001-native-claude-adapter.md`; 7 extra recordings for the matrix rows; `tests/capabilities.rs`; EVIDENCE.md E7; DL-010, DL-011)*
- [x] **S2-1 Codex protocol core.** ADR-002 option evaluation (git pin of `codex-app-server-protocol` vs codegen from `codex app-server generate-json-schema`); JSON-RPC framing; sans-IO core: initialize, `thread/start`, `turn/start`, both approval requests, questions (L4), `turn/interrupt`, `thread/resume`, diff notifications. *(round 6: `spikes/s2-codex` with `rpc` + `adapter` (mod/map/requests/host); option evaluation → DL-012 (vendored schema `schema/0.160.0/`, `tests/schema_methods.rs`); experimental API + mode names → DL-013; 23 tests)*
- [x] **S2-2 Codex live driver + `record-codex` + fixtures** for the §13 shared scenarios Codex supports (unsupported ones listed with reasons); restart survival experiment. *(round 7: 8 shared + 7 extra fixtures, none unsupported; `replay-codex` and evidence tests done early)*
- [x] **S2-3 Codex replay + `replay-codex` + ADR-002** (Proposed) with effect states and crash boundaries. *(round 7: replay + `replay-codex` + evidence tests; round 8: ADR-002, schema conformance, both question channels)*
- [x] **S4-1 Link identity + address rules.** Ed25519 identity keys, self-signed certs from them, mTLS with pinned-key verifiers, D13 address rules (loopback, RFC 1918, ULA, link-local, Tailscale ranges; reject public at bind and dial) as unit tests. *(round 9)*
- [x] **S4-2 Link handshake + channels.** `Hello` with `proto_min/max`, feature flags, roles, `link_epoch`, nonce; transcript signature bound to the TLS exporter; version negotiation; control channel (frames of §8.3) and data channel (multiplexed streams, credit flow control). *(round 10)*
- [x] **S4-3 Link pairing + reconnect.** `fdinv1:` invitations both directions (single use, 15 min, fingerprint-bound, secret proof bound to TLS); jittered backoff reconnect; duplicate link (`link_epoch`) rule; station-only-dials (NAT) case; multi-process loopback test with separate data dirs. *(round 11)*
- [ ] **S4-4 ADR-004** (Proposed): link protocol v1 (frames, handshake, pairing, scopes, reconnect, feature flags).
- [ ] **M1-F Close-out.** PLAN §6.2 matrix: verified value + evidence per cell; `spikes/README.md` offline commands for every spike; full acceptance run; DECISIONS-LOG complete.

## Round log

### Round 1 (2026-10-11)

- Done: P0-1; S1-1 core (frames, control, adapter core + tests).
- Live probe (one run, haiku, temp dir): `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio --model haiku`. Findings:
  - `initialize` control request → `control_response` with `commands`, `agents`, `models`, `account`, `current_permission_mode`, `capabilities`, ... (fixtures must scrub `account`).
  - `system/init` has `session_id`, `model`, `permissionMode`, `claude_code_version`, `tools`, `capabilities`.
  - The user's settings default to `bypassPermissions` → DL-004 (always pass `--permission-mode`).
  - The CLI modes are `manual|acceptEdits|plan|auto|dontAsk|bypassPermissions` → DL-003.
  - `result` has `terminal_reason` (`completed`), `stop_reason`, `usage`, `total_cost_usd`, `permission_denials`.
  - The user's global hooks run (`system/hook_*` frames): recordings should use `--safe-mode` or `--setting-sources` to keep fixtures free of local setup (decide in S1-2).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `cargo nextest run --workspace` → 19 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `web/` checks and TS-types drift check → not applicable in M1 (no `web/`, no `ts-rs` types yet; Phase 1 adds them)
- Next: S1-2.

### Round 2 (2026-10-11)

- Committed round 1 as `4973896 feat(spikes): scaffold v2 workspace and S1 Claude protocol core`.
- Review fix: a pending prompt is confirmed only by `command_lifecycle` `started` with the same `command_uuid` (a client UUID on each user message). `system/*`, stream, and assistant frames never confirm an input. A turn without host input (a background task finished, trap 4) gets `origin: provider`. Regression tests in `spikes/s1-claude/src/adapter/tests.rs`: `unrelated_frames_never_confirm_a_pending_prompt`, `provider_turn_does_not_confirm_a_pending_prompt`, `only_the_matching_uuid_confirms`, `replay_echo_is_ignored_once_lifecycle_frames_exist`, `unknown_lifecycle_state_is_counted_not_applied`, `input_started_during_a_turn_folds_into_it`. DL-006.
- S1-2 files: `spikes/s1-claude/src/driver.rs` (`LiveSession`), `spikes/spike-canon/src/fixture.rs` (format + scrubber), `spikes/s1-claude/src/record/` (12 scenarios), `src/bin/record-claude.rs`, `src/replay.rs`, `src/bin/replay-claude.rs`, `tests/replay_fixtures.rs`, 12 fixtures in `spikes/s1-claude/fixtures/rust/`.
- Live recordings: `record-claude --scenario <name>` for all 12 → all `ok` (haiku, `--safe-mode`, about $0.03–0.06 each). The first attempt failed for 7 scenarios: a plain `echo` is read-only and needs no approval in `default` mode (fixed: `touch fd-approve.txt && echo approve-ok`); haiku used the Write tool for the long-text prompt (fixed: "Do not use any tools"); `sleep 30` was blocked and the model moved it to the background (fixed: a foreground `python3 -c "import time; time.sleep(40)"`). The background run gave trap-4 evidence: `system/task_notification` started a new turn with no host input.
- Evidence (input for S1-4 and ADR-001):
  - Prompt confirmed = `command_lifecycle` `started`. `queued` arrives first; `completed` arrives after the turn's `result`. The `cancelled` frame of an interrupted input arrives only when the next input is written (`turn_interrupt`).
  - Queue: a second prompt during a turn runs as its own turn (`queued_turn`: 2 turns).
  - Steer: a prompt during a running tool joins the running turn after the tool result (`message_steering`: 1 turn; the reply ends with "banana").
  - Interrupt: `control_response` `{"still_queued":[]}`, then `result` `error_during_execution` with `terminal_reason` `aborted_streaming` (text) or `aborted_tools` (tool). After `aborted_tools` the CLI exits with code 1 when stdin closes.
  - Restart: an open `can_use_tool` request does not survive a kill. The resumed process reports `pending_permission_requests: []` and does not ask again without input. After a new prompt the model calls the tool again, and the CLI sends a new request with a new native id (`approval_restart`).
  - Native ids: the `can_use_tool` `request_id` is a UUID. Options: `allow`, `deny`, plus `allow_always` when `permission_suggestions` is not empty. The CLI echoes the host's `control_response` on stdout; the core ignores it.
  - Modes: the wire name is `default` for the flag `manual` (DL-003 update).
- Decisions: DL-003 updated, DL-005 (`--safe-mode` recordings), DL-006 (prompt confirmation).
- Commands and results:
  - `cargo run -p s1-claude --bin replay-claude` → 12 fixtures ok, unknown=0
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `cargo nextest run --workspace` → 33 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `web/` checks and TS-types drift check → not applicable in M1
- Next: S1-3 (TypeScript SDK reference recordings).

### Round 3 (2026-10-11)

- Committed round 2 as `8a114f5`.
- Review fix: `replay` now checks that every write of a process is in the recording **before** `exit` (it no longer drops them). Regression tests in `spikes/s1-claude/tests/replay_fixtures.rs`: `a_write_missing_from_the_recording_fails_before_exit`, `a_dropped_write_in_a_restarted_session_is_not_hidden_by_the_exit`, `a_recorded_write_that_the_core_does_not_make_fails`, `a_changed_write_fails`.
- Done: S1-3. SDK recorder (live, haiku, `--safe-mode`, temp dirs; `bun record.ts all` → 10 ok). Reference replay `spikes/s1-claude/src/reference.rs` + `tests/reference_fixtures.rs` (6 tests).
- SDK findings (evidence for ADR-001):
  - SDK args: `--output-format stream-json --verbose --input-format stream-json --model=haiku --permission-prompt-tool=stdio --permission-mode=default --include-partial-messages` (no `--replay-user-messages`).
  - SDK user messages have no `uuid` → no `command_lifecycle` frames → no per-input confirmation in the SDK stream (DL-008). Our adapter must keep sending a `uuid`.
  - The SDK sets `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS` → `system/session_state_changed` (`running`/`requires_action`/`idle`); the core parses it as a system frame (0 unknown frames).
  - SDK permission answers carry `toolUseID` → DL-007: our answers now do too; the 6 Rust fixtures with approvals were re-recorded. `approval_restart` now names the command again in the resumed prompt (the model did not repeat it on its own once).
  - Steering via the SDK behaves like ours: the steer prompt joins the running turn.
  - The turn outcomes of every shared scenario are equal for SDK and Rust frames (`sdk_and_rust_recordings_end_turns_the_same_way`).
- Commands and results:
  - `bun record.ts all` (live, spike) → 10 ok; `record-claude --scenario <6 approval scenarios>` (live) → 6 ok
  - `grep -lE "/home|<user name>" spikes/s1-claude/fixtures/*/*.jsonl` → none
  - `cargo run -p s1-claude --bin replay-claude` → 22 ok (12 rust + 10 sdk), unknown=0
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `cargo nextest run --workspace` → 43 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `web/` checks and TS-types drift check → not applicable in M1
- Next: S1-4 remainder (`insta` snapshots of the canonical events, evidence write-up).

### Round 4 (2026-10-11)

- Review feedback: "continue: S1-4 canonical-event snapshots and evidence write-up; make the SDK recorder exit nonzero on scenario failure and terminate timed-out sessions."
- Committed round 3 as `a1e2389`.
- SDK recorder (`sdk-recorder/record.ts`): every session has an `AbortController`; a scenario that times out (turn limit 120 s, scenario limit 300 s) is written as `<name>.failed.jsonl`, its sessions are aborted and noted ("host aborted the session"), and the process exits 1 if any scenario failed or is unknown. `sdk-recorder/selftest.sh` proves it offline with a fake CLI (`exec sleep 600`): unknown scenario → exit 1; timed-out scenario → exit 1, `.failed.jsonl` with the abort note and the `exit` record, fake process gone → `ok sdk-recorder selftest`.
- Snapshots: `tests/event_snapshots.rs` + 22 snapshots (`rust__*`, `sdk__*`).
- Core fixes found by the snapshots and the evidence work:
  - `command_lifecycle` frames arrive before the first `system/init`; the core now holds those events so that the stream opens with `session.started` (`events_before_init_follow_session_started`, `held_events_are_flushed_on_exit`).
  - `allow_always` echoed all `permission_suggestions`, including a `localSettings` destination (writes into the workspace), `setMode`, and `addDirectories`. It now sends session-scoped allow rules only (DL-009; `allow_always_sends_session_scoped_rules_only`, `no_allow_rule_means_no_allow_always`).
- Evidence: `tests/evidence.rs` (6 tests) + `spikes/s1-claude/EVIDENCE.md` (E1 prompt confirmed = `command_lifecycle` `started` for the input's `uuid`; E2 queue/steer timing; E3 open requests do not survive a restart; E4 no native option ids, `behavior` allow/deny, CLI UUID request ids; E5 every §14 S1 row; E6 observed CLI behaviour).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 55 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s1-claude --bin replay-claude` → 22 ok, unknown=0
  - `spikes/s1-claude/sdk-recorder/selftest.sh` → ok sdk-recorder selftest
  - `web/` checks, TS-types drift → not applicable in M1
- Next: S1-5 ADR-001.

### Round 5 (2026-10-11)

- Review feedback: "continue: S1-5 ADR-001 with Proposed status, covering every S1 Must prove row, provider effect states, and crash boundaries."
- Committed round 4 as `da057cd`.
- ADR-001 (`docs/v2/adr/ADR-001-native-claude-adapter.md`, Status: Proposed): process model and arguments, code structure, host commands → wire frames, CLI frames → canonical events, approvals/questions (no native option ids; `allow` / `allow_always` / `deny`), modes, interrupt, resume/fork, terminal paths, traps 1–10; §3 effect states ("confirmed" per effect type), a crash-boundary table with transcript reconciliation, open requests do not survive a restart; §4 fixture set (all 8 shared scenarios supported, none unsupported); §5 one row per §14 S1 "Must prove" with its evidence.
- New live recordings (haiku, `--safe-mode`, temp dir) for the matrix rows: `fork_session`, `plan_capture`, `subagent`, `background_task`, `file_edit`, `ask_user_question`, `interrupt_right_after_send`; `approval_restart` re-recorded with a transcript note.
- Findings: the subagent tool is `Agent` and runs async (it reports after the parent turn's `result` and starts a provider-origin turn); background Bash drains after the turn and its `task_notification` starts a provider-origin turn; plan mode writes the plan to `~/.claude/plans/` and sends it as the `ExitPlanMode` input; the CLI transcript stores each prompt under the client `uuid` (reconciliation key for `outcome_unknown`).
- Core changes: open tool nodes close on `result` and on exit (trap 6, `terminal_paths_close_open_tools`); `background.updated` spike event (DL-011); `approval.expired` reasons `turn_ended` / `cancelled_by_provider` documented (DL-010).
- Fixture hygiene: a home path and the temp dir name were split across streamed deltas and passed the scrubber. `record/coalesce.rs` merges such delta runs before scrubbing; `tests/fixture_hygiene.rs` checks raw frames and joined deltas.
- Evidence: `tests/capabilities.rs` (8 tests) + EVIDENCE.md E7.
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 67 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok (19 Rust, 10 SDK), unknown=0
  - `spikes/s1-claude/sdk-recorder/selftest.sh` → ok sdk-recorder selftest
  - `web/` checks, TS-types drift → not applicable in M1
- Next: S2-1 Codex protocol core.

### Round 6 (2026-10-11)

- Committed round 5 work first: `2c3634b docs(adr): ADR-001 native Claude adapter with capability evidence recordings`.
- Review fix: ADR-001 §3.2 now treats the transcript lookup as one-sided. A uuid found in the provider transcript confirms the effect; a uuid not found keeps the effect `outcome_unknown` (the CLI can crash between its read and its transcript write). Never resend automatically. EVIDENCE.md E7 matches.
- Done: S2-1.
  - Live probes of `codex app-server` (codex-cli 0.160.0, `gpt-5.6-luna`, effort low, temp dirs, `untrusted/workspace-write`): handshake (`initialize` → `initialized`), `thread/start`, `turn/start` with `clientUserMessageId`, the echo `item/started userMessage {clientId}`, `item/commandExecution/requestApproval` with integer id 0 and `availableDecisions` (`accept`, `{acceptWithExecpolicyAmendment}`, `cancel`), reply `{decision}` → `serverRequest/resolved`, `item/tool/requestUserInput` (experimental API, plan collaboration mode) answered with `{answers: {<id>: {answers: [..]}}}`.
  - Finding: `turn/start` while a turn runs returns the running turn id and the new user message joins that turn (it acts as a steer). The stable API has no follow-up queue (`thread/queue/*` is experimental), so the core queues follow-ups host-side and sends each one after `turn/completed` (the PLAN §6.2 degradation).
  - Core semantics: a prompt is `received` when the `turn/start` reply arrives and `started` (confirmed) only on its own `userMessage` item (`clientId` == input id, own thread); interrupts before the native turn id are held and sent with the reply (trap 3); approvals keep native decision values as option ids, refuse edited input, are idempotent, and expire on `serverRequest/resolved` without an answer (`cancelled_by_provider`), at turn end (`turn_ended`) and on exit (`process_lost`); async `agentMessage` questions are answered by a new user message and survive the turn end (L4 message channel); unknown server requests get a JSON-RPC error, elicitations a `decline` (fail closed); `usageLimitExceeded` → `usage_limit` (trap 7); `turn/diff/updated` paths dedupe with `fileChange` items.
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 90 passed (s2-codex: 23)
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, unknown=0 (S1 unchanged)
  - `web/` checks and TS-types drift check → not applicable in M1
- Next: S2-2 Codex live driver + `record-codex` + fixtures.

### Round 7 (2026-10-11)

- Committed round 6 as `2f87b73`.
- Review fixes (S2 core):
  - Answer idempotence after resolution and turn end: closed requests keep their receipt; a late answer returns the first receipt with `duplicate: true` and writes nothing; a late answer to an expired, unanswered request returns `AdapterError::Expired`. Test `answers_stay_idempotent_after_resolution_and_turn_end`.
  - Child threads: `turn/started` and `turn/completed` of another thread are ignored, and a second native turn id never replaces the running turn. Tests `child_thread_turns_never_replace_the_parent_turn`, `a_second_native_turn_never_replaces_the_running_one`.
- S2-2: `driver.rs` (tokio process driver), `record/` (`record-codex`, 15 scenarios, scrubber, rollout notes), `replay.rs` + `replay-codex`; 15 fixtures in `spikes/s2-codex/fixtures/` recorded live (`gpt-5.6-luna`, effort low, temp dirs).
- Bugs found by the recordings and fixed in the core, each with a test:
  - Trap 3: `turn/interrupt` after the `turn/start` reply but before `turn/started` fails with -32600 "no active turn to interrupt". The core now holds the interrupt until `turn/started` (`interrupt_before_the_turn_id_is_held_then_sent`, fixture `interrupt_right_after_send` re-recorded: turn-1 interrupted).
  - Relative `turn/diff/updated` paths are joined with the thread cwd before the duplicate check (`relative_diff_paths_join_the_thread_cwd_before_the_duplicate_check`).
  - Empty commentary messages are not reported (`empty_agent_messages_are_not_reported`).
  - `thread/goal/cleared` and `thread/settings/updated` are known, ignored notifications.
- Evidence (`spikes/s2-codex/tests/evidence.rs`): prompt confirmed = `userMessage` item with the client id; the rollout keeps every client id; queued prompts = separate turns; steer joins the running turn; an open approval does not survive a restart (`process_lost`, the resumed process does not ask again); native option ids `accept`, `acceptWithExecpolicyAmendment`, `cancel` (no `decline` under `untrusted`); RPC question channel; held interrupt; interrupt during a tool.
- DECISIONS-LOG: DL-014 (recordings use the user's `~/.codex`), DL-015 (host-side follow-up queue; deny = `decline` if offered, else `cancel`).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 109 passed (s2-codex: 42)
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s2-codex --bin replay-codex` → 15 ok, unknown=0
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, unknown=0
- Next: S2-3 ADR-002.

### Round 8 (2026-10-11)

- Committed round 7 as `7453425`.
- S2-3: `docs/v2/adr/ADR-002-native-codex-adapter.md` (Status Proposed): protocol source (DL-012), process model, host commands, frame → event map, approvals, **both L4 question channels** (RPC reply for `item/tool/requestUserInput`; new user message for `agentMessage` async questions), fail-closed rules, idempotence, modes, interrupt, resume, traps 1–10, **effect states** (what "confirmed" means per effect), **crash boundaries** with one-sided history lookup (client id found → confirmed; not found → stays `outcome_unknown`; never auto-resend), open requests survive restart: no, fixture set, PLAN §14 S2 coverage table (every cited test exists: checked with a grep loop over the ADR).
- New tests:
  - `tests/schema_conformance.rs`: a JSON-schema subset checker validates every recorded frame against the vendored 0.160.0 schema; host writes strictly (`host_writes_conform_strictly`), provider frames for required members and types (`provider_frames_conform`), and a negative test (`the_checker_rejects_a_misspelled_parameter`).
  - `tests/evidence.rs`: `initialize_and_thread_start_open_the_session`, `thread_resume_keeps_the_thread_and_its_context`, `file_change_approvals_use_native_decisions`, `diff_notifications_report_each_file_once`, `plan_mode_reports_the_plan`, `the_resume_reply_lists_earlier_prompts_by_client_id`.
  - `tests/schema_methods.rs`: `async_question_fields_exist_in_the_schema`.
- Core fix found by the new diff test: a scrubbed or already cwd-relative path was joined with the cwd twice; `file_changed` now joins only paths that are relative and do not start with the cwd.
- Core tests split to stay readable: `src/adapter/tests.rs` (625 lines) and `src/adapter/tests_review.rs` (review regressions).
- Live probe: `codex app-server --enable send_message_to_user_async` does not give the model the `request_user_input_async` tool in 0.160.0, so the message question channel has no live fixture; the unit test and the schema test cover it (README + ADR-002 §2.6 say so).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 119 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s2-codex --bin replay-codex` → 15 ok, unknown=0, exit 0
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, unknown=0
  - `web/` checks and TS-types drift → not applicable in M1
- Next: S4-1 link identity + address rules.

### Round 9 (2026-10-11)

- Committed round 8 first: `707a04d docs(adr): ADR-002 native Codex adapter with schema conformance and evidence tests`.
- Review fixes (S2):
  - Async message-channel questions: every question of an `agentMessage` is now its own request (one answer each; with more than one question the message names the question). Tests `every_async_question_becomes_its_own_request`, `a_single_async_question_gets_the_bare_answer`.
  - RPC question answers are validated before the write (non-empty map, only asked ids, `{"answers":[strings]}` values); a bad answer writes nothing and the request stays open. Test `rpc_question_answers_are_validated_before_the_write`.
  - `tests/schema_conformance.rs`: provider responses to every client request are checked against `<Method>Response` (`provider_responses_conform`), `additionalProperties` is enforced (`the_checker_validates_answer_map_values`), and a response without a required member fails (`the_checker_rejects_a_response_without_a_required_member`). ADR-002 §2.1/§2.6/§5 updated.
- S4-1 done: new crate `spikes/s4-link` (`secret`, `addr`, `identity`, `tls`, `net`) and `tests/tls.rs` (11 loopback handshake tests).
- Finding: TLS 1.3 session resumption skips the client and server certificate checks, so a revoked key could reconnect with a resumed session. `a_revoked_key_is_refused_on_the_next_handshake` failed until both configs turned resumption off → DL-017.
- DL-016 (rustls + ring provider, tokio-rustls, rustls-webpki), DL-017 (no `0.0.0.0`/`::` bind, accepted-peer address check, no TLS resumption).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 155 passed (s4-link: 30)
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s2-codex --bin replay-codex` → 15 ok, unknown=0, exit 0
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, unknown=0
  - `web/` checks and TS-types drift → not applicable in M1
- Next: S4-2 link handshake + channels.

### Round 10 (2026-10-11)

- Committed round 9 first: `e02206e feat(spikes): S4 link identity, D13 address rules and pinned-key mTLS`.
- S4-2 done in `spikes/s4-link`:
  - `src/hello.rs` (+ `src/hello_tests.rs`, 8 tests): `Hello {proto_min, proto_max, features, role, node_id, link_epoch, nonce, channel}`, `negotiate()` (highest common version, feature intersection, required `control.v1`, identity/role/channel/nonce checks), transcript over the TLS exporter (`EXPORTER-fleet-deck-link-v1`), both raw hellos and both keys, Ed25519 `auth` signatures with a side tag, `ready` and `close` frames with stable reason codes.
  - `src/frames.rs` (5 tests): control frames `request/response/event/ack/snapshot/ping/pong/close`, 1 MiB limit, method table with scopes and feature gates, `authorize()`.
  - `src/mux.rs` (5 tests incl. a property test): data-channel stream multiplexer with credit flow control.
  - `src/link/` (`mod.rs`, `handshake.rs`, `control.rs`, `data.rs`): `dial()` (control, then data when `data_streams.v1` is negotiated, same epoch), `listen()` with a per-peer link table (higher epoch supersedes with `superseded`, lower or equal → `stale_link_epoch`, data needs the control channel of the same epoch), request/response in both directions with scope checks per request, ping/pong, data streams.
  - `tests/link.rs` (9 tests): `the_station_dials_the_deck`, `the_deck_dials_the_station`, `no_common_version_fails_on_both_sides_with_the_same_reason`, `features_are_negotiated_and_gate_methods_and_channels`, `requests_are_checked_against_the_method_table_and_the_granted_scopes`, `a_higher_link_epoch_supersedes_and_a_lower_one_is_refused`, `a_data_channel_needs_its_control_channel`, `the_data_channel_streams_a_mebibyte_each_way_with_flow_control`, `a_reader_that_gives_no_credit_stops_the_writer`.
- DL-018 (tokio-tungstenite + futures-util), DL-019 (spike link scopes and method table), DL-020 (handshake details PLAN §8.2 does not state).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 182 passed (s4-link: 57)
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s2-codex --bin replay-codex` → 15 ok, unknown=0
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, unknown=0
  - `web/` checks and TS-types drift → not applicable in M1
- Next: S4-3 link pairing + reconnect.

### Round 11 (2026-10-11)

- Committed round 10 first: `97a1490 feat(spikes): S4 link handshake, feature negotiation, epochs and control/data channels`.
- Review fix: the data channel now ends with its control channel. `src/link/control.rs` sends its end reason to the data task (`data_stop`); `src/link/data.rs` ends, fails pending writes, and reports `data_channel_lost` to the control task when it ends first. Because the two TCP connections race, a control task that sees `data_channel_lost` waits up to 500 ms for a peer `close` frame, so a superseded dialer reports `superseded`. Regression tests in `tests/link.rs`: `closing_the_control_channel_ends_the_data_channel_on_both_sides`, `a_superseded_link_loses_its_data_channel_too`, `dropping_the_data_handles_keeps_the_link_up`. 20 repeated runs of `tests/link.rs`: 0 failures.
- S4-3 done in `spikes/s4-link`:
  - `src/pairing.rs` (5 tests): `Invitation` (`fdinv1:wss://<ip>:<port>#<fingerprint>.<secret>.<expiry>`, `Secret` with redacted `Debug`), HMAC-SHA256 proofs over the handshake transcript in both directions, `Invitations` store (single use, 15 min, failure cap 5).
  - `src/link/pair.rs` + `Channel::Pair`: `link::pair()` (joiner) and the listener side; `PeerPolicy::PinnedOrInvited` lets unknown keys through TLS only while an invitation is open, and only onto the pair channel.
  - `src/reconnect.rs` (3 tests): `Backoff` (equal jitter, 500 ms → 5 min cap, reset after 60 s uptime), `EpochClock`, `spawn_dialer()`.
  - `src/bin/fd-link-node.rs`: one deck or station process with its own data directory (identity key, `peers.json`).
  - `tests/pairing.rs` (8 tests) and `tests/multiprocess.rs` (3 tests: `nat_stations_pair_dial_and_reconnect_after_a_deck_restart`, `the_deck_joins_a_station_invitation_and_dials_it`, `public_and_wildcard_addresses_are_refused`). 8 repeated runs of both files: 0 failures.
- DL-021 (pairing details), DL-022 (backoff and epoch source).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 204 passed (s4-link: 79)
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s2-codex --bin replay-codex` → 15 ok, unknown=0, exit 0
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, unknown=0, exit 0
  - `web/` checks and TS-types drift → not applicable in M1
- Next: S4-4 ADR-004.
