# M3 lane A (durable) — round log

Lane A owns branch v2, `docs/v2/DECISIONS-LOG.md` (DL-125 to DL-199) and the merges of the other lanes. Items, in order: 7-1 ShimHost, 7-2 Auto-continue, 7-3 Drain and self-upgrade, 7-6 Docs; after the other lanes say "LANE COMPLETE" and are merged: 6-13 Parity, 7-7 Cutover, 7-8 Final checks.

## Round 46 (2026-10-12, 14:22–)

- Start: committed the interrupted round-45 work (DL-124, schema V14 `shims.adapter_gap`) as `af8c78b`. `m3/orchestration`, `m3/board` and `m3/ops` had no commits beyond v2 (all at `106c5b5`): the merges were no-ops.
- DL-125: the owner decision "fast rounds and parallel lanes", recorded as approved.
- **7-1 ShimHost — done (DL-126).** New tests in `crates/fleetdeck-cli/tests/shim_gap.rs`: `a_partial_line_is_never_acknowledged` (ADR-005 test 4) and `an_unterminated_last_line_at_eof_is_committed_as_malformed_before_the_exit_is_acknowledged` (test 39, renamed, with the gap-before-exit order). Default host stays `direct`; `host = "shim"` is the opt-in.
- **7-2 Auto-continue — done (DL-127).** Proto: `SessionCommand::SetAutoContinue`, feature `auto_continue.v1`, events `session.auto_continue_changed` / `session.auto_continue_skipped`, `RunOrigin::Continuation`, `SessionSummary.auto_continue`. Station: `store/auto_continue.rs`, `manager/auto_continue.rs`, `admit_resume` with a first input, dispatch recheck in `manager/target.rs`. Board: the toggle "Continue after a station restart", the label "Station continuation", the skip notice. ADR-004 rows; ADR-007 status Accepted.
- Commands and results:
  - `cargo nextest run -p fleetdeck-cli --test shim_gap -E 'test(a_partial_line_is_never_acknowledged) | test(an_unterminated_last_line_at_eof)'` → 2 passed.
  - `cargo nextest run -p fleetdeck-station -E 'test(/auto_continue_tests/)'` → 16 passed.
  - `make ts-types` → bindings written (`SessionCommand`, `Event`, `RunOrigin`, `SessionSummary`).
  - `bun test src` → 107 pass (3 new auto-continue tests); `bun run typecheck` → clean; `bun run --cwd web lint` → clean.
  - `cargo nextest run -p fleetdeck-proto -p fleetdeck-link -p fleetdeck-deck -p fleetdeck-station` → 483 passed, after the table tests were updated for the new method and events (deck command count 15, ADR-004 method table 30, mutation count 18, event samples for the two new kinds, envelope row order).
  - `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean; `bun run --cwd web lint` → clean.
- Next: 7-3 Drain and self-upgrade (with ADR-005 tests 20, 21, 22, 48).

## ADR-005 §9 test map (7-1 close)

| ADR test | Status | Test |
| --- | --- | --- |
| 1 | covered | `crates/fleetdeck-cli/tests/station_shim_sigkill.rs` `providers_under_shims_survive_a_station_sigkill_and_the_restarted_station_reattaches_them` |
| 2, 5, 7–14, 17, 18, 30–33, 35–38, 41–43, 46, 47, 49, 50 | covered by exact name | `crates/fleetdeck-cli/tests/shim_*.rs`, `host::shim::tests` |
| 3 | partial | `shim_recovery.rs` `a_station_restart_reattaches_with_a_higher_generation_and_replays_from_the_committed_cursor_once` (no named fault point) → follow-up |
| 4 | covered (round 46) | `shim_gap.rs` `a_partial_line_is_never_acknowledged` |
| 6 | covered | `shim_gap.rs` `a_below_floor_gap_invalidates_the_adapter_state_at_once_while_the_provider_runs` |
| 15 | covered | `crates/fleetdeck-station/src/store/request_time_tests.rs` |
| 16 | open | stale socket → follow-up |
| 19 | covered | `shim_recovery.rs` `a_shim_that_was_killed_is_exited_with_its_exit_file_or_lost_without_one`, `shim_station.rs` `a_dead_shim_leaves_its_session_lost_and_its_claims_outcome_unknown_after_a_station_start` |
| 20, 21, 22, 48 | moved to 7-3 | upgrade, drain, N-1 |
| 23 | covered | `shim_manager.rs` `a_shim_session_keeps_its_turn_checkpoints_and_survives_a_station_restart` |
| 24 | not built | auto-continue under the shim host skips `unsupported` (DL-127 deviation 7) |
| 25 | covered | `shim_station.rs` `uncertain_inputs_are_settled_before_commands_are_admitted_after_a_station_start` |
| 26–29, 34 | covered | `shim_stdin.rs` |
| 39 | covered (round 46) | `shim_gap.rs` `an_unterminated_last_line_at_eof_is_committed_as_malformed_before_the_exit_is_acknowledged` |
| 40 | partial | `shim_manager.rs` `a_live_shim_exit_with_a_surviving_child_keeps_the_evidence_and_resume_stays_refused`, `manager::tests::resume` |
| 44 | open | exit file on ENOSPC → follow-up |
| 45 | open | `ack_exit` fault points → follow-up |

## Review follow-ups (after M3)

| Item | Finding | File |
| --- | --- | --- |
| 7-1 | ADR-005 test 3: a named fault point between the attach and the replay | `crates/fleetdeck-cli/tests/shim_recovery.rs` |
| 7-1 | ADR-005 test 16: a stale socket file is replaced, a live one is refused | `crates/fleetdeck-providers/src/host/shim/launch.rs` |
| 7-1 | ADR-005 test 44: the exit file write on ENOSPC | `crates/fleetdeck-providers/src/host/shim/server_proc.rs` |
| 7-1 | ADR-005 test 45: `ack_exit` fault points | `crates/fleetdeck-providers/src/host/shim/server_conn.rs` |
| 7-1 | ADR-005 test 40: the full matrix of exit evidence | `crates/fleetdeck-cli/tests/shim_manager.rs` |
| 7-2 | ADR-007 test 15: a resume that reports another native identity ends the new process with `outcome_unknown` | `crates/fleetdeck-station/src/manager/target.rs` |
| 7-2 | Rule 10 (workspace, slot) is not rechecked at dispatch | `crates/fleetdeck-station/src/store/auto_continue.rs` |
| 7-2 | Auto-continue under the shim host (ADR-005 test 24) | `crates/fleetdeck-station/src/manager/auto_continue.rs` |
| 7-2 | A Playwright phone-viewport flow for the toggle | `web/e2e/` |
