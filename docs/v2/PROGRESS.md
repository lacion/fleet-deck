# Fleet Deck v2 — progress (goal loop memory)

Current milestone: **M1 = PLAN Phase 0 (spikes S1, S2, S4)**. Max 15 rounds.

Rules for this file: one checklist item per M1 deliverable, in PLAN order. Each round updates the item status, the commands it ran, and the results. The work of a round stays uncommitted for the reviewer; the next round commits it first.

## M1 checklist

- [x] **P0-1 Scaffold.** Root Cargo workspace (spike members only, DL-001), `deny.toml`, lint policy (`unsafe_code = forbid`, `clippy::unwrap_used = deny` outside tests), `spike-canon` (canonical event subset of §6.3), `docs/v2/DECISIONS-LOG.md`, `spikes/README.md`, this file. *(round 1)*
- [x] **S1-1 Claude protocol core.** Frame parser (never panics, unknown frames kept, proptest), host control frames, sans-IO adapter core: init, prompt, partial deltas, `can_use_tool` with native option ids + edited input, idempotent answers, fail-closed unknown requests, interrupt, set mode (native modes only), exit expiry. *(round 1; round 2 review fix: a prompt is confirmed only by `command_lifecycle` `started` with the matching uuid, DL-006)*
- [x] **S1-2 Claude live driver + fixture format + `record-claude`.** tokio process driver around the core; JSONL fixture format (`spawn`/`host`/`in`/`out`/`raw`/`exit`/`note` records + header, scrubbed); `record-claude`; live recordings (haiku, `--safe-mode`, temp dir) of all 8 §13 shared scenarios plus `tool_call_edit_input`, `interrupt_during_tool`, `set_permission_mode`, `approval_restart`. *(round 2; an early replay harness + `replay-claude` is pulled forward from S1-4)*
- [x] **S1-3 TypeScript SDK reference recordings.** `spikes/s1-claude/sdk-recorder/` (bun + `@anthropic-ai/claude-agent-sdk` 0.3.296, `tee-claude.mjs` as `pathToClaudeCodeExecutable`); 10 SDK fixtures in `spikes/s1-claude/fixtures/sdk/` (8 shared + `tool_call_edit_input`, `set_permission_mode`); `reference` module + `replay-claude` replay them offline (DL-008). *(round 3)*
- [x] **S1-4 Claude replay + evidence.** *(round 2: `replay` module, `replay-claude`, `tests/replay_fixtures.rs`.)* *(round 3: replay rejects unmatched writes before `exit`; SDK reference replay.)* *(round 4: `insta` snapshots of the canonical event stream of all 22 fixtures; `tests/evidence.rs`; `spikes/s1-claude/EVIDENCE.md` for "prompt confirmed", queue/steer timing, restart survival, native option ids; SDK recorder exits nonzero and kills timed-out sessions + `selftest.sh`.)*
- [ ] **S1-5 ADR-001** (Proposed): native Claude adapter design, effect states and crash boundaries (§6.4), fixture set, unsupported scenarios with reasons.
- [ ] **S2-1 Codex protocol core.** ADR-002 option evaluation (git pin of `codex-app-server-protocol` vs codegen from `codex app-server generate-json-schema`); JSON-RPC framing; sans-IO core: initialize, `thread/start`, `turn/start`, both approval requests, questions (L4), `turn/interrupt`, `thread/resume`, diff notifications.
- [ ] **S2-2 Codex live driver + `record-codex` + fixtures** for the §13 shared scenarios Codex supports (unsupported ones listed with reasons); restart survival experiment.
- [ ] **S2-3 Codex replay + `replay-codex` + ADR-002** (Proposed) with effect states and crash boundaries.
- [ ] **S4-1 Link identity + address rules.** Ed25519 identity keys, self-signed certs from them, mTLS with pinned-key verifiers, D13 address rules (loopback, RFC 1918, ULA, link-local, Tailscale ranges; reject public at bind and dial) as unit tests.
- [ ] **S4-2 Link handshake + channels.** `Hello` with `proto_min/max`, feature flags, roles, `link_epoch`, nonce; transcript signature bound to the TLS exporter; version negotiation; control channel (frames of §8.3) and data channel (multiplexed streams, credit flow control).
- [ ] **S4-3 Link pairing + reconnect.** `fdinv1:` invitations both directions (single use, 15 min, fingerprint-bound, secret proof bound to TLS); jittered backoff reconnect; duplicate link (`link_epoch`) rule; station-only-dials (NAT) case; multi-process loopback test with separate data dirs.
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
