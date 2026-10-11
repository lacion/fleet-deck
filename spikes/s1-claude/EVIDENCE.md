# S1 evidence — Claude Code stream-json (claude 2.1.289)

This file lists what the S1 recordings prove, with the fixture and the offline test for each claim. ADR-001 and the PLAN §6.2 capability matrix cite these entries. All tests run offline:

```sh
cargo nextest run -p s1-claude
cargo run -p s1-claude --bin replay-claude
```

Fixtures: `fixtures/rust/*.jsonl` (our recorder, `record-claude`) and `fixtures/sdk/*.jsonl` (the TypeScript Agent SDK 0.3.296, `sdk-recorder/`). Recording setup: `--model haiku --safe-mode --permission-mode default`, a fresh temporary directory per scenario (DL-004, DL-005).

## E1 — What "prompt confirmed" means (§6.4 `confirmed`)

| Claim | Fixture | Test |
| --- | --- | --- |
| The host writes each user message with a client `uuid`. The CLI answers with `command_lifecycle` frames for that `uuid`: `queued` → `started` → `completed` (or `cancelled`, `discarded`, `refused`). | `rust/simple`, `rust/queued_turn` | `evidence::prompt_confirmed_is_command_lifecycle_started` |
| **Confirmed = `command_lifecycle` `started` for the input's `uuid`.** `queued` only means "accepted into the CLI's command queue"; the effect has not started. | all shared `rust/*` | same |
| No other frame confirms an input: `system/status`, `system/init`, hook frames, and a turn that the CLI starts on its own (for example after a background task, trap 4) do not. | unit tests | `adapter::tests::unrelated_frames_never_confirm_a_pending_prompt`, `adapter::tests::provider_turn_does_not_confirm_a_pending_prompt`, `adapter::tests::only_the_matching_uuid_confirms` |
| `completed` arrives after the turn's `result` frame. | `rust/queued_turn` | `event_snapshots` (`rust__queued_turn`) |
| An interrupted input ends `cancelled`, not `completed`. The `cancelled` frame can arrive late (after the next host write). | `rust/turn_interrupt`, `rust/interrupt_during_tool` | `evidence::an_interrupted_input_ends_cancelled` |
| A client that sends no `uuid` (the TypeScript SDK) gets no `command_lifecycle` frames and therefore has **no** per-input confirmation. The station adapter must always send a `uuid` (DL-006, DL-008). | `sdk/*` | `reference_fixtures::sdk_prompts_are_never_confirmed` |

## E2 — Queue and steer timing

| Claim | Fixture | Test |
| --- | --- | --- |
| Two prompts written at once run as two turns; the second is confirmed only after the first turn's `result`. | `rust/queued_turn`, `sdk/queued_turn` | `evidence::a_queued_prompt_is_confirmed_when_its_turn_starts`, `replay_fixtures::queued_turn_runs_as_two_turns` |
| A prompt written while a tool runs (steer) joins the running turn: the CLI drains it after the tool result, before the next model request. One turn, one `result`. | `rust/message_steering`, `sdk/message_steering` | `evidence::a_steer_prompt_starts_inside_the_running_turn_after_a_tool_result`, `replay_fixtures::steering_folds_into_the_running_turn` |
| The SDK and our adapter end every shared scenario's turns the same way. | `sdk/*`, `rust/*` | `reference_fixtures::sdk_and_rust_recordings_end_turns_the_same_way` |

## E3 — Open approval requests and a provider restart (§6.4 crash boundary)

| Claim | Fixture | Test |
| --- | --- | --- |
| An open `can_use_tool` request does **not** survive a CLI restart. After a kill with the request open, `claude --resume <session>` reports `pending_permission_requests: []` and `session_state: "idle"` in the `initialize` response, and it does not ask again without new input. | `rust/approval_restart` | `replay_fixtures::open_approval_does_not_survive_a_restart` |
| The adapter expires the open request with reason `process_lost` when the process exits, and closes the running turn as `failed` (trap 6). | `rust/approval_restart` | same; `adapter::tests::exit_expires_open_requests` |
| After a new prompt the resumed CLI issues a **new** request with a **new** native id. An answer to the old id has no target. | `rust/approval_restart` | `replay_fixtures::open_approval_does_not_survive_a_restart` (2 `approval.requested`, 1 `approval.expired`) |

## E4 — Approval options and native ids (§6.2)

| Claim | Fixture | Test |
| --- | --- | --- |
| The native request id is a UUID chosen by the CLI (`control_request.request_id`). The adapter maps it to an app id (`req-N`) and back. | `rust/tool_call_approve` | `evidence::approval_options_map_to_native_behaviors` |
| The CLI has **no option ids**. The native answer is `behavior: "allow"` (with `updatedInput`, optional `updatedPermissions`) or `behavior: "deny"` (with `message`, optional `interrupt`). The adapter offers `allow`, `allow_always` (only when a session-scoped allow rule exists, DL-009), and `deny`. | `rust/tool_call_approve`, `rust/tool_call_deny` | `evidence::approval_options_map_to_native_behaviors`, `adapter::tests::allow_always_sends_session_scoped_rules_only`, `adapter::tests::no_allow_rule_means_no_allow_always` |
| `permission_suggestions` name a destination; `localSettings` would write into the workspace. | `rust/tool_call_approve` | `evidence::permission_suggestions_name_their_destination` |
| Edited input: `allow` with `updatedInput` runs the edited command. | `rust/tool_call_edit_input`, `sdk/tool_call_edit_input` | `replay_fixtures::every_fixture_replays_with_identical_host_writes`, `reference_fixtures::approval_answers_match_the_sdk` |
| The answer echoes `toolUseID`, as the SDK does (DL-007). A second answer to the same request writes nothing (idempotent). An unknown option id is rejected. | `sdk/tool_call_*` | `reference_fixtures::approval_answers_match_the_sdk`, `adapter::tests::approval_answer_is_idempotent_and_uses_native_ids` |
| An unknown control request fails closed: the adapter answers with `control_response` `error`. | unit test | `adapter::tests::unknown_control_request_fails_closed` |

## E5 — Other "Must prove" rows of PLAN §14 S1

| Row | Evidence |
| --- | --- |
| spawn, initialize | every fixture; `initialize` response has `commands`, `models`, `current_permission_mode`, `session_state`, `pending_permission_requests` |
| prompt, partial deltas | `--include-partial-messages` → `stream_event` `text_delta` → `message.delta` (`event_snapshots` `rust__simple`) |
| `can_use_tool` allow / deny with edited input | E4 |
| interrupt during a tool | `rust/interrupt_during_tool`: `interrupt` → `control_response` `{"still_queued":[]}`, the tool result says the user rejected the tool, `result` `error_during_execution` with `terminal_reason: "aborted_tools"`, turn `interrupted` (`replay_fixtures::interrupts_end_the_turn_as_interrupted`) |
| interrupt while streaming | `rust/turn_interrupt`, `sdk/turn_interrupt`: `terminal_reason: "aborted_streaming"` |
| resume | `rust/provider_thread_resume`, `sdk/provider_thread_resume`: `--resume <session>` keeps the session id and the context (the reply names the code word) |
| set permission mode | `rust/set_permission_mode`, `sdk/set_permission_mode`: `set_permission_mode` → `{"mode":"plan"}`, next `system/init` has `permissionMode: "plan"`; wire names `default`, `acceptEdits`, `plan`, `auto`, `dontAsk`, `bypassPermissions` (DL-003); other names rejected (`adapter::tests::set_mode_accepts_only_native_modes`) |
| unknown-frame handling | the parser keeps unknown frames (`frames::tests::parser_never_panics_on_any_json`), the core counts them (`adapter::tests::unknown_control_request_fails_closed`, `adapter::tests::unknown_lifecycle_state_is_counted_not_applied`); all 29 fixtures replay with 0 unknown frames |
| TS SDK frame recording + replay | `sdk-recorder/`, `src/reference.rs`, `tests/reference_fixtures.rs` (DL-008) |
| response channel | the approval answer is a `control_response` on stdin to the CLI's `control_request` id (RPC reply); `toolUseID` binds it to one tool call |
| open requests survive restart | E3: no |
| canonical event stream | `tests/event_snapshots.rs`: one `insta` snapshot per fixture (`tests/snapshots/`) |

## E6 — Observed CLI behaviour that the adapter must handle

- The CLI sends `command_lifecycle` frames **before** the first `system/init`. The adapter holds those events so that the canonical stream opens with `session.started` (`adapter::tests::events_before_init_follow_session_started`, `adapter::tests::held_events_are_flushed_on_exit`).
- A second `system/init` arrives before every turn; it is not a new session.
- The CLI echoes the host's `control_response` frames on stdout; the adapter ignores responses to ids it did not send.
- A long foreground `sleep` is blocked by the CLI and the model retries in the background; the background task later starts a turn without host input (trap 4). The adapter maps such a turn as `origin: provider` and never confirms an input with it.
- `--permission-mode manual` is an alias: the CLI reports `default` (DL-003). The owner's settings default to `bypassPermissions`, so the adapter always passes a mode (DL-004).

## E7 — Capability evidence from the extra recordings (§6.2 matrix)

Tests in `spikes/s1-claude/tests/capabilities.rs`, one per matrix row:

| Capability | Finding | Test |
| --- | --- | --- |
| fork | `--resume <id> --fork-session` gives a new session id with the old context | `capabilities::fork_starts_a_new_session_with_the_old_context` |
| plan capture | in `plan` mode the plan is the `ExitPlanMode` tool input (`plan` markdown, `planFilePath`) and comes as an approval; the CLI first writes the plan file to `~/.claude/plans/` | `capabilities::plan_mode_captures_the_plan_from_exit_plan_mode` |
| subagent parent link | the `Agent` tool starts an async subagent; its frames carry `parent_tool_use_id`; it reports after the parent turn's `result` and starts a provider-origin turn | `capabilities::subagent_frames_link_to_the_parent_tool_call` |
| background work | `background_tasks_changed` / `task_started` / `task_notification`; the set drains after the turn; the notification starts a provider-origin turn | `capabilities::background_work_reports_after_the_turn` |
| file-change telemetry | no diff event; `Edit`/`Write` tool results with `file_path` | `capabilities::write_tool_results_map_to_file_changes` |
| questions (L4) | `AskUserQuestion` is a `can_use_tool` request; the answer is `allow` + `updatedInput.answers` | `capabilities::questions_are_answered_by_rpc_reply` |
| stop before the turn started (trap 3) | the interrupt works; the input ends `cancelled`, the next prompt runs | `capabilities::interrupt_before_the_turn_started_cancels_the_input` |
| reconciliation (§6.4) | the CLI transcript stores each delivered prompt under the client `uuid`; a found `uuid` confirms delivery, a missing `uuid` does not prove non-delivery (the effect stays `outcome_unknown`) | `capabilities::the_transcript_keeps_prompts_under_the_client_uuid` |
| terminal paths (trap 6) | open tools close on `result` and on exit | `adapter::tests::terminal_paths_close_open_tools` |
