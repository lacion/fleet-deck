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

## Round 47 (2026-10-12, 14:52–)

- Committed round 46 as 3423c4b. Merged `m3/board` (bb1a63c, plans and usage meters) as c480748 without conflicts; `m3/orchestration` and `m3/ops` had no new commits. `make ts-types` gave no diff; fmt, clippy, web lint and typecheck clean; proto + deck tests 136 passed.
- Review of round 46 (7-1, 7-2): findings added to the follow-up list below; no rework.
- 7-3 Drain and self-upgrade done (DL-128):
  - `crates/fleetdeck-station/src/manager/drain.rs` (+ `drain_tests.rs`), local request `drain`, CLI `fleetdeck station drain`.
  - `crates/fleetdeck-station/src/upgrade.rs` (+ `upgrade_tests.rs`): records, snapshot, trial, rollback plan, restore with a new incarnation, stable launcher; schema V15 `shims.protocol`; `serve.rs` marks the update live before `Store::open`.
  - `crates/fleetdeck-cli/src/upgrade.rs`, `cli.rs` `UpgradeCommand`; `crates/fleetdeck-providers/src/host/shim/wire.rs` `pick_common`.
  - Tests: `cargo nextest run -p fleetdeck-station -E 'test(/drain/)'` → 2 passed; `-E 'test(/upgrade::/)'` → 5 passed; `cargo nextest run -p fleetdeck-link --test version_skew` → 4 passed; `cargo nextest run -p fleetdeck-cli --test station_upgrade` → 2 passed; station local/schema/shims tests → 17 passed.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 92 (2026-10-12, 17:05–)

- Round start: committed round 91 as 80282d7. `git log v2..m3/orchestration`, `v2..m3/board`, `v2..m3/ops`: no new commits; `m3-orchestration.md` has no "LANE COMPLETE", so 6-13, 7-7 and 7-8 stay blocked.
- 7-6 cutover guide: `docs/v2/cutover.md` §1 said only "The full parity table is PLAN §16". It now lists the five open parity checks of round 91 (session grid, Shift+Enter newline, model badge after a model change, owner resume from the board, station discovery on the LAN), so a v1 user knows what is not yet proven before the cutover. The list goes away when 6-13 adds a test or a deferral for each row.
- Checks: `cargo nextest run -p fleetdeck-cli --test docs_commands`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`.

## Round 91 (2026-10-12, 17:03–)

- Round start: committed round 90 as 708fdd4. `git log v2..m3/orchestration`, `v2..m3/board`, `v2..m3/ops`: no new commits; `m3-orchestration.md` has no "LANE COMPLETE", so 6-13, 7-7 and 7-8 stay blocked.
- 6-13 preparation (no checklist change): the "Gap to check" column of the round-88 draft table was checked with `grep` over `web/src`, `web/e2e` and `crates`:
  - Confirmed present: `Nx` batch (`web/src/spawn/spawn.test.tsx` "a batch starts one session per line and Nx repeats a task (M3 6-1)"; `web/e2e/spawn.spec.ts` "a batch starts one worktree session per line with Nx (M3 6-1)"); board approvals (`web/e2e/session.spec.ts` "an approval is answered from the needs-you rail"); rename (`web/e2e/workspace.spec.ts` "a session is renamed from the board, also after it ended"; `web/src/session/session.test.tsx` "rename works on an ended session and needs drive and a live board (DL-095)"); event feed (`web/e2e/workspace.spec.ts` "the event feed shows fleet changes newest first, filters them and opens a session"; `web/src/board/feed.test.ts`).
  - Confirmed gaps (6-13 must add a test or a deferral entry): session grid (no grid test in `web/`); composer newline (no Shift+Enter or newline test; the composer has no key handler in `web/src/session/`); model badge update after a model change (no test that a model change event updates the card); owner resume from the board (only the import form refuses `--resume`); station discovery on the LAN (no discovery code or test in `crates/`; tailnet stays an owner step).
- Checks: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` (docs only, no test run).

## Round 90 (2026-10-12, 17:01–)

- Round start: committed round 89 as 615de04. `git log v2..m3/*`: no new commits on m3/orchestration, m3/board or m3/ops; nothing to merge. `m3-orchestration.md` still has no "LANE COMPLETE", so 6-13, 7-7 and 7-8 stay blocked.
- Review r89 findings (lane B code, merged in f8dbc14): recorded as three 6-5 rows in "Review follow-ups (after M3)". Verified in the code: `transmit` checks only the binding fence, not the row's `retry_until` at send time; the deduplication test uses a fake deck; the lifecycle watermark test covers only the local fence. Lane B owns the code; lane A does not change it.
- Checks: `cargo fmt --all --check` -> clean; `cargo clippy --workspace --all-targets -- -D warnings` -> clean (docs-only round).

## Round 89 (2026-10-12, 16:58–)

- Round start: committed round 88 as 2f1df49. Merged `m3/orchestration` 560032c (2d661f1, 6-5 slice DL-218: durable station coordinator outbox, station schema V17 `coordinator_outbox`, `crates/fleetdeck-station/src/coordinator/outbox.rs`, `store/coordinator_outbox.rs`) as f8dbc14 without conflicts. `crates/fleetdeck-proto` unchanged, so no `make ts-types`. `m3/board` and `m3/ops`: no new commits. `m3-orchestration.md` still has no "LANE COMPLETE", so 6-13, 7-7 and 7-8 stay blocked.
- Merge validation: `cargo nextest run -p fleetdeck-station -E 'test(outbox) | test(coordinator)'` -> 33 passed.
- 7-6 docs for DL-218: internals §8 coordinator row names `outbox.rs`, `store/coordinator_outbox.rs`, V17 and the outbox states; operations §11 has a row for a coordinator call that fails `deck_unreachable` before the first deck link.
- Checks: `cargo nextest run -p fleetdeck-cli --test docs_commands`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`.

## Round 88 (2026-10-12, 16:56–)

- Round start: committed round 87 as 26d350a. `git log v2..m3/orchestration`, `v2..m3/board`, `v2..m3/ops`: no new commits; `m3-orchestration.md` has no "LANE COMPLETE", so 6-13, 7-7 and 7-8 stay blocked.
- 6-13 preparation (not the item; the 6-13 checklist line is unchanged): a draft map of PLAN §16 rows to candidate tests, found with `grep` on test names in `crates/` and `web/`. 6-13 must confirm each row, add a test or a DECISIONS-LOG deferral for each gap, and do the same for PLAN §4.1.

### 6-13 preparation draft: PLAN §16 parity rows (unconfirmed)

| §16 row | Candidate evidence (test names) | Gap to check in 6-13 |
| --- | --- | --- |
| Live derived status lanes + `unreachable` / `lost` | `a_session_moves_through_the_lanes`; `a_deck_without_a_link_listener_cannot_invite_the_first_sync_is_a_snapshot_and_offline_cards_say_unreachable`; `web/src/board/fleet.test.tsx` | none found |
| Model badge follows `/model` | `web/src/App.test.tsx` "cards show the title, station, provider badge, workspace, current tool, usage and model"; `a_busy_follow_up_and_a_mode_or_model_change_survive_a_claude_restart` | a test that a model change event updates the card |
| Conflict radar, across stations | `the_same_file_changed_on_two_stations_is_one_conflict_inside_the_window`; `web/e2e/conflicts.spec.ts` | none found |
| Needs-you rail: approvals from the board | `a_session_runs_asks_for_approval_is_answered_interrupted_and_stopped`; `web/e2e/session.spec.ts` | confirm the board path, not only the CLI path |
| Re-armed questions | not needed (protocol keeps the request open); `a_station_kill_expires_the_open_approval_and_rejects_its_stale_reply` | none |
| Mail between sessions | `a_mail_is_delivered_once_as_a_framed_follow_up_or_steer`; `web/e2e/fleet.spec.ts` | none found |
| `assign auto` + station terms | `assign_auto_prefers_idle_then_queued_then_busy_then_least_buried`; `assign_auto_needs_the_right_repo_and_provider`; `web/e2e/assign.spec.ts` | none found |
| Spawn, batch, `Nx`, station picker | `web/e2e/spawn.spec.ts`; `web/src/spawn/spawn.test.tsx` | confirm an `Nx` batch test name |
| Live terminal → session view + human shell (D10) | `a_shell_runs_in_its_cwd_with_the_scrubbed_environment`; `a_reattached_board_gets_the_scrollback_and_the_shell_survives_a_detach`; `web/e2e/shell.spec.ts` | none found |
| Terminal grid → session grid | no named grid test found | session-grid parity test (named in the 6-13 line) |
| Shift+Enter, image paste | `a_prompt_with_a_pasted_image_reaches_the_station_and_a_bad_image_is_refused`; `web/e2e/images.spec.ts` | composer newline test |
| Revive → native resume | resume tests in `crates/fleetdeck-cli/tests/shim_manager_parts/live_exit.rs`; `web/src/session/timeline.test.ts` | confirm an owner resume test from the board |
| Remote control (claude.ai) | dropped by PLAN | DECISIONS-LOG reference only |
| Plan library | `a_proposed_plan_enters_the_library_once_and_keeps_the_owner_status`; `a_plan_is_idempotent_per_input_and_a_start_is_never_open` | none found |
| Workspace tools (files, worktrees, rename, event feed) | `a_worktree_start_creates_the_callsign_worktree_on_its_branch`; `web/e2e/workspace.spec.ts`; `web/e2e/revert.spec.ts` | confirm rename and event feed |
| Share board on LAN → deck access + station discovery | `cookie_sockets_need_the_board_origin_and_bearer_clients_need_a_single_use_ticket`; `web/e2e/stations.spec.ts` | station discovery test; tailnet is an owner step |
| Adopt / takeover → import session id | `an_imported_claude_session_continues_the_given_native_session`; `an_import_is_refused_for_a_bad_id_a_foreign_workspace_or_a_native_session_in_use` | none found |
| Gateway routing → station provider profiles | `two_accounts_on_one_driver_are_two_instances`; `the_instance_adds_back_only_its_own_settings` | none found |
| Coder support | dropped (D13) | DECISIONS-LOG reference only |
| `/fleet` + fleet-doctrine skill → control skill | `an_owner_coordinator_start_creates_one_grant_and_sends_its_id_and_the_control_skill` | depends on 6-5 (lane B); starts fail `handoff_unverified` |
| Plugin install + version bump → binary release | `the_cargo_dist_config_releases_fleetdeck_for_linux_and_macos_with_the_packaging_files` | release run is an owner step |

- Checks: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` (docs only, no test run).

## Round 87 (2026-10-12, 16:55–)

- Round start: committed round 86 as 12df9f5. `git log v2..m3/orchestration`, `v2..m3/board`, `v2..m3/ops`: no new commits; `m3-orchestration.md` has no "LANE COMPLETE", so 6-13, 7-7 and 7-8 stay blocked.
- 7-6 owner-step list: `docs/v2/operations.md` §12 lacked two items. Added the real GitHub Actions run of `.github/workflows/v2.yml` (owner step recorded since M2, `docs/v2/PROGRESS.md` line 222) and the open coordinator live smoke (ADR-006 §8.5, item 6-5) with its `handoff_unverified` effect.
- Checks: `cargo nextest run -p fleetdeck-cli --test docs_commands`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`.

## Round 86 (2026-10-12, 16:53–)

- Round start: committed round 85 as 2991ef1. `git log v2..m3/orchestration`, `v2..m3/board`, `v2..m3/ops`: no new commits; `m3-orchestration.md` has no "LANE COMPLETE", so 6-13, 7-7 and 7-8 stay blocked.
- 7-6 consistency with the r84 finding: `docs/v2/cutover.md` §1 `/fleet` row was stale (it said the grant and the control skill were not in; DL-217 added them). The row now names DL-217 and the `handoff_unverified` limit, and tells the owner to keep v1 `/fleet` until the coordinator live smoke passes. README "What you can do" coordinator bullet names the same limit and links operations §7.2.
- Checks: `cargo nextest run -p fleetdeck-cli --test docs_commands`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`.

## Round 85 (2026-10-12, 16:51–)

- Round start: committed round 84 as 1bed9ce.
- Sync: no new commits on m3/orchestration, m3/board or m3/ops.
- Review finding of round 84 (operations §7.2 did not say that all providers refuse a coordinator start with `handoff_unverified`): checked against `crates/fleetdeck-providers/src/coordinator.rs` (`handoff` returns `Err(UNVERIFIED)` for Claude and Codex; test `the_capability_stays_off_until_the_live_smoke_passes`). The guide was wrong for the current build, so this round fixed it: §7.2 has a "Current limit" paragraph, and the §11 row names `handoff_unverified`. Follow-up row added as FIXED.
- Lane B `m3-orchestration.md` still has no "LANE COMPLETE"; 6-13, 7-7 and 7-8 stay blocked.
- Checks: `cargo nextest run -p fleetdeck-cli --test docs_commands` → passed; `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 84 (2026-10-12, 16:50–)

- Round start: committed round 83 as 4c49703.
- Sync: no new commits on m3/orchestration, m3/board or m3/ops.
- 7-6 Docs: new `docs/v2/operations.md` §7.2 "Start a coordinator session": the owner steps on the board (spawn form "Coordinator", the "Coordinator grants" list of the "Mail and tasks" panel, "Revoke" and "Renew"), a caution before the steps, the classes `read`, `mail`, `drive` that the form asks for (`web/src/spawn/SpawnForm.tsx`), the control skill and the start refusals (DL-216, DL-217). It states the open 6-5 parts (no durable coordinator outbox, no coordinator live smoke).
- Lane B `m3-orchestration.md` still has no "LANE COMPLETE"; 6-13, 7-7 and 7-8 stay blocked.
- Checks: `cargo nextest run -p fleetdeck-cli --test docs_commands` → passed; `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 83 (2026-10-12, 16:47–)

- Round start: committed round 82 as 9b62718.
- Sync: merged m3/orchestration d3ac028/6232db8 as 5b7e1dd without conflicts (6-5 slice DL-217: owner coordinator start creates the grant with the control skill `crates/fleetdeck-deck/src/control_skill.md`, deck schema V11 `tokens.start_command_id`, station binding lifecycle in `crates/fleetdeck-station/src/manager/coordinator_start.rs` and `manager/target.rs`, spawn form toggle "Coordinator" in `web/src/spawn/SpawnForm.tsx`). crates/fleetdeck-proto did not change, so `make ts-types` was not needed. m3/board and m3/ops had no new commits.
- Merge validation: `cargo nextest run -p fleetdeck-deck -p fleetdeck-station -E 'test(coordinator) | test(owner_start) | test(grant)'` → 58 passed.
- Review finding of round 82: MD033 inline HTML on `<reason>` in `docs/v2/operations.md` §10. Fixed now (it breaks a lint check): the text is `&lt;reason&gt;`. `markdownlint-cli2 docs/v2/operations.md` reports no MD033 (only MD013 line-length notices, which the repository does not enforce).
- 7-6 Docs: `docs/v2/internals.md` §8 coordinator row names the binding lifecycle (`bound_events`, `ended_events`) and `owner_start` with the control skill (DL-217).
- Lane B `m3-orchestration.md` still has no "LANE COMPLETE"; 6-13, 7-7 and 7-8 stay blocked.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean; `bun run --cwd web lint` → clean (web changed in the merge).

## Round 82 (2026-10-12, 16:46–)

- Round start: committed round 81 as 5c4bb26.
- Sync: no new commits on m3/orchestration, m3/board or m3/ops.
- 7-6 Docs: `docs/v2/operations.md` §7.1 has a caution before the step that enters the native session id. Do not import the id of an earlier session that is `lost` with a shim host until its timeline shows the exit, because the station checks only the sessions that drive the id now (open follow-up of review r50, `crates/fleetdeck-station/src/manager/decide.rs`). The follow-up row names the caution; the code check stays open.
- Lane B `m3-orchestration.md` still has no "LANE COMPLETE"; 6-13, 7-7 and 7-8 stay blocked.
- Checks: `cargo nextest run -p fleetdeck-cli --test docs_commands` → passed (the guide still names only real commands); `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 81 (2026-10-12, 16:44–)

- Round start: committed round 80 as c131df9.
- Sync: no new commits on m3/orchestration, m3/board or m3/ops.
- 7-6 follow-up status: four doc follow-ups of reviews r49 and r50 were already fixed in commit 8798ac2 (15:35) but were still listed as open. Checked each against the current text and marked them FIXED in the follow-up table:
  - `docs/v2/operations.md` §10 "Limits of this release": the shim host gets `unsupported`; the dispatch rechecks only identity, proven end, newer owner work, requested stop and the time window.
  - `README.md` line 24: a revert that cannot be proven needs reconciliation and blocks input.
  - `docs/v2/operations.md` §7.1: the import uses the permission mode of the spawn form; a provider instance stores no mode.
- The round-71 evidence-map row on A10 coverage is marked "claim narrowed" (the round-72 "Limits of this map" note); the missing A10 recheck tests stay open as 7-2 follow-ups.
- Lane B `m3-orchestration.md` still has no "LANE COMPLETE"; 6-13, 7-7 and 7-8 stay blocked.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 80 (2026-10-12, 16:41–)

- Round start: committed round 79 as 3d5d586.
- Sync: no new commits on m3/orchestration, m3/board or m3/ops.
- 7-8 preparation: the module size check now has named tests, so `make check` (through the workspace test run) proves both directions without a hand count. New `crates/fleetdeck-cli/tests/module_size.rs`:
  - `every_module_has_fewer_than_800_lines`: `sh tools/module-size.sh` exits 0, and the largest module that the test finds in `crates/` and `web/src/` has fewer than 800 lines.
  - `the_module_size_check_fails_and_names_the_largest_module_below_its_size`: with `FD_MODULE_LIMIT` set to the line count of the largest module, the script exits 1 and its output names that file and its line count.
  - `cargo nextest run -p fleetdeck-cli --test module_size` → 2 passed.
- Lane B `m3-orchestration.md` still has no "LANE COMPLETE"; 6-13, 7-7 and 7-8 stay blocked.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 79 (2026-10-12, 16:40–)

- Round start: committed round 78 as 8600d2a.
- Sync: no new commits on m3/orchestration, m3/board or m3/ops.
- Review r78: the 700-line probe count in the round-78 entry was wrong. `FD_MODULE_LIMIT=700 sh tools/module-size.sh` → exit 1 and lists 17 files, from 702 lines (`crates/fleetdeck-providers/tests/capability_matrix.rs`) to 798 lines (`crates/fleetdeck-providers/src/host/shim/host.rs`). The round-78 entry is corrected and the finding is a follow-up row.
- Lane B `m3-orchestration.md` still has no "LANE COMPLETE"; 6-13, 7-7 and 7-8 stay blocked.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 78 (2026-10-12, 16:38–)

- Round start: committed round 77 as 72a8fc8.
- Sync: no new commits on m3/orchestration, m3/board or m3/ops.
- 7-8 preparation: the goal rules "no `unwrap()` outside tests" and "modules stay under 800 lines" are now both reachable through `make check`.
  - `unwrap()`: already enforced. `Cargo.toml` sets `unwrap_used = "deny"`, `clippy.toml` sets `allow-unwrap-in-tests = true`, and `make check` runs clippy with `-D warnings`.
  - Module size: was not checked. New `tools/module-size.sh` fails when a `.rs` file under `crates/` or a `.ts`/`.tsx` file under `web/src/` has 800 or more lines (`FD_MODULE_LIMIT` overrides the limit). `tools/check.sh` runs it after `cargo deny check`.
  - `sh tools/module-size.sh` → exit 0 (largest module: `crates/fleetdeck-providers/src/host/shim/host.rs`, 798 lines); `FD_MODULE_LIMIT=700 sh tools/module-size.sh` → exit 1 and lists the files with 700 or more lines, so the check can fail (round 79 correction: it lists 17 files, among them `host.rs` with 798 lines; the round-78 text said two files because the probe output was cut by `head -3`); `sh -n tools/check.sh` → syntax ok.
- Lane B `m3-orchestration.md` still has no "LANE COMPLETE"; 6-13, 7-7 and 7-8 stay blocked.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 77 (2026-10-12, 16:37–)

- Round start: committed round 76 as 73e61e5.
- Sync: no new commits on m3/orchestration, m3/board or m3/ops.
- 7-8 preparation (the copy itself waits for LANE COMPLETE): checked the lane decision files against `docs/v2/DECISIONS-LOG.md`.
  - `docs/v2/decisions/m3-orchestration.md`: DL-200 to DL-216 (17 entries; DL-204 is written before DL-203).
  - `docs/v2/decisions/m3-board.md`: DL-300 to DL-309 (10 entries).
  - `docs/v2/decisions/m3-ops.md`: DL-400 to DL-405 (6 entries; DL-404 and DL-405 are written before DL-403).
  - No number is defined twice across the four files. Every `DL-nnn` reference in `docs/`, `README.md`, `crates/` and `web/src/` resolves to a defined entry; the only unresolved strings are the range bounds DL-199, DL-299, DL-399 and DL-499.
  - The 7-8 copy keeps the numbers and puts the entries in numeric order under one "M3 lane decisions" heading per lane.
- Lane B `m3-orchestration.md` still has no "LANE COMPLETE"; 6-13, 7-7 and 7-8 stay blocked.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 76 (2026-10-12, 16:35–)

- Round start: committed round 75 as 5b3e375.
- Sync: no new commits on m3/orchestration, m3/board or m3/ops.
- 7-6 Docs: `docs/v2/operations.md` §11 troubleshooting row "A coordinator session does not start" with the exact start-effect reasons from `crates/fleetdeck-station/src/manager/coordinator_start.rs` (`a coordinator session starts only as a new direct child`, `the coordinator endpoint does not listen`) and the unverified handoff (DL-215, DL-216).
- Lane B `m3-orchestration.md` still has no "LANE COMPLETE"; 6-13, 7-7 and 7-8 stay blocked.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 75 (2026-10-12, 16:33–)

- Round start: committed round 74 as a1786dc.
- Sync: merged m3/orchestration 046dea1 (6-5 slice, DL-216: the start effect mints the coordinator credential and hands it to the child environment, `crates/fleetdeck-station/src/manager/coordinator_start.rs`) as 3df2a77. No board or ops commits. `crates/fleetdeck-proto` unchanged, so `make ts-types` was not needed.
- Merge validation: `cargo nextest run -p fleetdeck-station -E 'test(coordinator_start)'` → 2 passed.
- 7-6 Docs: `docs/v2/internals.md` §8 coordinator row names `manager/coordinator_start.rs` and its start-effect checks (DL-216).
- Lane B `m3-orchestration.md` still has no "LANE COMPLETE"; 6-13, 7-7 and 7-8 stay blocked.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 74 (2026-10-12, 16:31–)

- Round start: committed round 73 as 32d3170. No new commits on m3/orchestration, m3/board or m3/ops. Lane B still has no "LANE COMPLETE", so 6-13, 7-7 and 7-8 stay blocked.
- Module size fix (rule "modules stay under 800 lines"): a scan of `crates/**/*.rs` found two lane A test files over the limit, `crates/fleetdeck-cli/tests/shim_manager.rs` (863 lines) and `crates/fleetdeck-cli/tests/shim_stdin.rs` (818 lines). The tail of each moved into a child module with `#[path]` and `use super::*` (no behaviour change, same test binary):
  - `crates/fleetdeck-cli/tests/shim_manager_parts/live_exit.rs`: `ended_row`, `resume`, `a_live_shim_exit_with_a_surviving_child_keeps_the_evidence_and_resume_stays_refused`, `a_live_shim_exit_without_survivors_is_a_proven_end`.
  - `crates/fleetdeck-cli/tests/shim_stdin_parts/parallel_recovery.rs`: `recovery_waits_for_blocked_writes_of_several_shims_in_parallel`.
  - Now: `shim_manager.rs` 729, `shim_stdin.rs` 771 lines; the largest file in `crates/` has 798 lines. The ADR-005 §9 map row 40 names the new module.
- Tests: `cargo nextest run -p fleetdeck-cli --test shim_manager --test shim_stdin -E 'test(live_exit::) | test(parallel_recovery::)'` → 3 passed.
- Checks: `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean.

## Round 73 (2026-10-12, 16:28–)

- Round start: committed round 72 as ff0800a. No new commits on m3/orchestration, m3/board or m3/ops. Lane B still has no "LANE COMPLETE", so 6-13, 7-7 and 7-8 stay blocked.
- 7-6 docs for the merged DL-215 slice: `docs/v2/internals.md` §8 coordinator row names `check_spec` in `manager/coordinator_command.rs` (owner start only, unverified handoff refused) and `coordinator/handoff.rs`.
- Checks: `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean. Docs only: no tests run.

## Round 72 (2026-10-12, 16:25–)

- Round start: committed round 71 as 0a2b7ac. Merged m3/orchestration 8771bf4 (lane B 6-5 slice: `SessionSpec.coordinator`, station start checks, credential handoff, DL-215) as 63a7ec1. `crates/fleetdeck-proto` changed: `make ts-types` regenerated `web/src/gen` (no diff after the merge). m3/board and m3/ops: no new commits.
- Merge validation: `cargo nextest run -p fleetdeck-station -E 'test(coordinator)'` → 23 passed (incl. `only_an_owner_start_may_carry_a_complete_coordinator_spec`, `a_coordinator_spec_for_an_unverified_handoff_is_refused_unsupported`). No merge fix needed.
- Lane B still has no "LANE COMPLETE" in `docs/v2/progress/m3-orchestration.md`, so 6-13, 7-7 and 7-8 stay blocked.
- Review of round 71: the evidence map overstated the A10 and the ShimHost upgrade coverage. Recorded as two follow-ups, and a "Limits of this map" note under the map table.
- Checks: `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; `bun run --cwd web lint` clean.

## Round 71 (2026-10-12, 16:24–)

- Committed round 70 as 6566386.
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`. `m3-orchestration.md` has no LANE COMPLETE (6-5 open); 6-13, 7-7 and 7-8 still wait.
- Pre-PR review preparation: new section "Phase 7 acceptance evidence map" in this file. Each Phase 7 acceptance line names its tests or target (test names read from the test files).
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 70 (2026-10-12, 16:21–)

- Committed round 69 as 0d4bbcd.
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`. `m3-orchestration.md` has no LANE COMPLETE (6-5 open); 6-13, 7-7 and 7-8 still wait.
- 7-6 docs link check (script over README, internals, operations, cutover, LIVE-SMOKE and packaging README): every relative Markdown link and every `crates/…`, `web/…`, `docs/…`, `packaging/…` path exists. The §8 table of `docs/v2/internals.md` had three crate paths without the `crates/` prefix; they now have it.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 69 (2026-10-12, 16:21–)

- Committed round 68 as ff86800.
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`. `m3-orchestration.md` has no LANE COMPLETE (6-5 open); 6-13, 7-7 and 7-8 still wait.
- 7-6 docs: `docs/v2/internals.md` §1 glossary adds **workspace**, **mail / task** and **coordinator grant** (the lane B terms).
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 68 (2026-10-12, 16:20–)

- Committed round 67 as ecdcafd.
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`. `m3-orchestration.md` has no LANE COMPLETE.
- 6-5 remaining work, copied from the lane B time-box status (`docs/v2/progress/m3-orchestration.md`, DL-209), so that lane A knows what 6-13 must check after the merge:
  - the coordinator spec on `session.start` and the §8.2 handoff wiring with the secrecy tests 10–13;
  - the durable station coordinator outbox (ADR-006 §6.5, §8.4);
  - the snapshot projection (test 25e);
  - grant creation at an owner start with the deck-served control skill;
  - the coordinator live smoke (§8.5).
- 7-6 docs: `docs/v2/cutover.md` §1 coordinator row names the open parts, so that a v1 user does not expect a `/fleet` replacement before 6-5 closes.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 67 (2026-10-12, 16:19–)

- Committed round 66 as 9a653cf.
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`. `m3-orchestration.md` has no LANE COMPLETE (6-5 open); 6-13, 7-7 and 7-8 still wait. The time-box status table of round 65 stays valid.
- 7-6 docs: `docs/v2/operations.md` §11 troubleshooting row for `feature_not_negotiated` (version skew, `coordinator.v1`).
- Tests: `cargo nextest run -p fleetdeck-cli --test docs_commands` → 1 passed.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 66 (2026-10-12, 16:18–)

- Committed round 65 as 49b1f67.
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`. `m3-orchestration.md` has no LANE COMPLETE (6-5 open); 6-13, 7-7 and 7-8 still wait. The time-box status table of round 65 stays valid.
- 7-6 docs: `docs/v2/internals.md` §8 coordinator row names the `coordinator.v1` gate of the link forward methods.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 65 (2026-10-12, 16:17–)

- Committed round 64 as a86a08c.
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`. `m3-orchestration.md` has no LANE COMPLETE (6-5 open).

### M3 status at the 16:20 time box

| Item | State | Evidence |
| --- | --- | --- |
| P2-a, P2-b, SIGKILL child survival | Done | Named regression tests (rounds 1-3 of this log). |
| 6-1 Workspaces, 6-2 Mail, 6-3 Assign auto, 6-4 Tasks | Done (lane B) | `docs/v2/progress/m3-orchestration.md`. |
| 6-5 Coordinator | Open (lane B, partial) | ADR-006 tests up to 26 merged (9b27b2e); no LANE COMPLETE. |
| 6-6 to 6-10, 6-12 | Done (lane C, LANE COMPLETE) | `docs/v2/progress/m3-board.md`. |
| 6-11 Human shell, 7-4 Performance, 7-5 Packaging | Done (lane D, LANE COMPLETE) | `docs/v2/progress/m3-ops.md`. |
| 7-1 ShimHost, 7-2 Auto-continue, 7-3 Drain and self-upgrade, 7-6 Docs | Done (lane A) | This log. |
| 6-13 Parity, 7-7 Cutover, 7-8 Final checks | Open (lane A) | Wait for lane B LANE COMPLETE and its merge. |

- Owner steps (unchanged): Docker image build, real tailnet, macOS launchd, physical phone, cargo-dist release (`docs/v2/operations.md` §12).
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 64 (2026-10-12, 16:15–)

- Committed round 63 as dc177bd.
- Lanes: merged `m3/orchestration` 42b27a9 as 9b27b2e, no conflicts (`coordinator.call` and `coordinator.query` gated by `coordinator.v1`, ADR-006 test 26). `crates/fleetdeck-proto` changed (doc comment): `make ts-types` → no change. `m3/board`, `m3/ops`: no new commits. `m3-orchestration.md` has no LANE COMPLETE (6-5 open); 6-13, 7-7 and 7-8 still wait.
- Merge validation: `cargo nextest run -p fleetdeck-link` → 106 passed.
- 7-6 docs: `docs/v2/internals.md` §link names the `coordinator.v1` feature gate as the example of `feature_not_negotiated`.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 63 (2026-10-12, 16:13–)

- Committed round 62 as f0b3ba7.
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`. `m3-orchestration.md` has no LANE COMPLETE (6-5 open); 6-13, 7-7 and 7-8 still wait.
- 7-6 docs: the README documentation list links the ADRs (001, 002, 004, 005, 006, 007).
- Tests: `cargo nextest run -p fleetdeck-cli --test docs_commands` → 1 passed.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 62 (2026-10-12, 16:12–)

- Committed round 61 as ae42039.
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`. `m3-orchestration.md` has no LANE COMPLETE (6-5 open); 6-13, 7-7 and 7-8 still wait.
- 7-6 docs: `docs/v2/cutover.md` §1 maps the v1 `/fleet` slash command and fleet-doctrine skill to the v2 coordinator grant and deck-served control skill (PLAN §16 row; marked in progress until 6-5 closes).
- Tests: `cargo nextest run -p fleetdeck-cli --test docs_commands` → 1 passed.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 61 (2026-10-12, 16:11–)

- Committed round 60 as 5b7654c.
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`. `m3-orchestration.md` has no LANE COMPLETE (6-5 open); 6-13, 7-7 and 7-8 still wait.
- 7-6 docs: `README.md` "What you can do" lists the import of a CLI session (6-12) and coordinator sessions (ADR-006, marked in progress until 6-5 closes).
- Tests: `cargo nextest run -p fleetdeck-cli --test docs_commands` → 1 passed.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 60 (2026-10-12, 16:09–)

- Committed round 59 as b8176c8.
- Merged `m3/orchestration` ee38ccd (6-5 partial: the owner renewal waits for the station acknowledgment, ADR-006 25b; deck schema and grant routes; board fleet panel) without conflicts. `m3/board`, `m3/ops`: no new commits. No proto change, so no `make ts-types`.
- Merge validation: `cargo nextest run -p fleetdeck-deck -E 'test(/coordinator/) | test(/grant/) | test(/schema/) | test(/migration/) | binary(board_fleet)'` → 31 passed.
- 7-6 docs: the internals coordinator row names `web/src/fleet/FleetPanel.tsx` (grant renewal).
- Lanes: `m3-orchestration.md` still has no LANE COMPLETE (6-5 open); 6-13, 7-7 and 7-8 still wait.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean; `bun run --cwd web lint` → clean (94 files).

## Round 59 (2026-10-12, 16:07–)

- Committed round 58 as 48518f8.
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`. `m3-orchestration.md` has no LANE COMPLETE (6-5 open); 6-13, 7-7 and 7-8 still wait.
- 7-6 docs: `docs/v2/operations.md` §12 (owner steps) now also lists lane D's owner steps (`dist generate` + tap repository, the LAN approval round trip on two hosts) and the opt-in live smoke tests (`make smoke-live`, `LIVE-SMOKE.md`). The doc-command test (`docs_commands`) is unaffected: the new text names only `make` targets.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 58 (2026-10-12, 16:05–)

- Committed round 57 as bb7df4a (`docs(v2): round 57 log, internals coordinator row`).
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`; nothing to merge. `m3-orchestration.md` has no LANE COMPLETE (6-5 open); 6-13, 7-7 and 7-8 still wait.
- Merge validation of 860d919 (lane B 6-5 partial on top of lane A's drain, auto-continue and import): `cargo nextest run -p fleetdeck-station -p fleetdeck-deck -E 'test(/coordinator/) | test(/grant/)'` → 51 passed (incl. `a_coordinator_or_the_local_socket_cannot_enable_auto_continue` and the three `coordinator_command` tests). No merge fix needed.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Round 57 (2026-10-12, 16:04–)

- Committed round 56 as 3d57be1 (`fix(station): a drain refuses session.import (DL-130)`).
- Merged `m3/orchestration` 69833de (6-5 partial: station link forward, lineage from the start receipt, `session.coordinator_renew` / `session.coordinator_revoke` link methods) as 860d919, without conflicts. `m3/board` and `m3/ops`: no new commits.
- Merge check: the two new commands create no provider work, so the drain guard (`manager/drain.rs`) stays unchanged. `make ts-types` → no change (lane B committed the generated `SessionCommand.ts`).
- `docs/v2/internals.md` §8: the coordinator row names `coordinator/forward.rs`, `manager/coordinator_command.rs`, `http/grant_routes.rs` and the board command file.
- Lanes: `m3-orchestration.md` still has no LANE COMPLETE (6-5 open), so 6-13, 7-7 and 7-8 still wait.
- Checks: `cargo fmt --all --check` → clean; `cargo clippy --workspace --all-targets -- -D warnings` → clean; `bun run --cwd web lint` → clean (94 files).

## Round 56 (2026-10-12, 16:01–)

- Committed round 55 as the round-start commit (`docs(v2): round 55 log and internals touch-up`).
- Lanes: no new commits on `m3/orchestration`, `m3/board`, `m3/ops`; nothing to merge. `m3-board` and `m3-ops` say LANE COMPLETE; `m3-orchestration` does not (6-5 Coordinator partial), so 6-13, 7-7 and 7-8 still wait.
- Merge fix (6-12 into the 7-3 drain): `session.import` now counts as new work and a drain refuses it (`crates/fleetdeck-station/src/manager/drain.rs`, DL-130); test `manager::tests::drain::a_draining_station_refuses_an_import`; `docs/v2/operations.md` §9 lists import.
- Docs check: the r49/r50 7-6 doc findings (shim auto-continue limit, partial dispatch rechecks, README revert text, import mode) are already correct in `docs/v2/operations.md` §7.1/§10 and `README.md`.
- Tests: `cargo nextest run -p fleetdeck-station -E 'test(/drain/)'` → 3 passed.

## Round 55 (2026-10-12, 15:47–)

- Committed round 54 as 65df80d.
- `m3/orchestration`: no new commits; no LANE COMPLETE (6-5 Coordinator still partial).
- Merged `m3/board` 7fa38ad (`make test-lint`, DL-309, lane C close-out notes) without conflicts.
- Merged `m3/ops` 749df03 (6-11 review fixes, DL-405) as e2914ef. Conflicts:
  - `crates/fleetdeck-station/src/store/schema.rs`: both lanes added station schema V15. Lane A's V15 (`shims.protocol`, DL-128) came first; lane D's `shell_effects` table is now V16 (`M::up(V16)` added, comment says it was renumbered). Unreleased versions need no data compatibility.
  - `docs/v2/PROGRESS.md`: kept the `[x]` lines of 6-10 and 6-12; took lane D's updated 6-11 line.
- Checks after the merge: workspace build clean; station tests `shell | schema | upgrade` 13 pass; `make ts-types` no change; fmt, clippy and web lint clean.
- `docs/v2/internals.md` §8: the human-shell row names `store/shell_effects.rs` (schema V16).

## Round 54 (2026-10-12, 15:44–)

- Committed round 53 as 7cb013c.
- Merged `m3/orchestration` e7956cd (lane B's merge of v2 with its own resolution of the V9 renumber) as 4d9e9f4. The three conflicts (`read_model.rs` call order, the V9 doc line in `schema.rs`, the export order in `ts.rs`) had the same content on both sides; lane B's version taken. `make ts-types`: no change. Build, fmt and clippy clean.
- `m3/board`, `m3/ops`: no new commits. Lane B has no LANE COMPLETE (6-5 Coordinator still partial), so 6-13, 7-7 and 7-8 still wait.

## Round 53 (2026-10-12, 15:39–)

- Committed round 52 as cce4e69.
- Merged `m3/orchestration` 0a647eb (6-5 Coordinator, partial: deck decision core, grants, queries, lifecycle, station MCP ingress) as d1e504b. Conflicts resolved, both sides kept:
  - `crates/fleetdeck-deck/src/store/schema.rs`: lane B's coordinator grants (`tokens`, `coordinator_lineage`) renumbered V8 → V9 (file touches were V8 first); `M::up(V9)` added; module doc updated.
  - `crates/fleetdeck-deck/src/store/read_model.rs`: both `conflicts::record_touches` and `grants::record_lifecycle` run per event.
  - `crates/fleetdeck-proto/src/mutation.rs` imports and `crates/fleetdeck-proto/src/ts.rs` exports of both sides; `Cargo.lock` keeps `hyper`, `hyper-util` and `portable-pty`.
  - `crates/fleetdeck-deck/src/store/coordinator_ops.rs` and `coordinator_tests.rs`: `UserInput::new(..)` (the images field came from lane C).
  - `make ts-types` regenerated `web/src/gen` (coordinator types).
- Checks after the merge: build clean; `cargo nextest run -p fleetdeck-proto -p fleetdeck-link -p fleetdeck-deck` → 279 passed; station coordinator and ingress tests → 15 passed; fmt, clippy, biome lint and typecheck clean.
- Lane B has no LANE COMPLETE yet (6-5 is partial), so 6-13, 7-7 and 7-8 still wait.
- 7-6: `docs/v2/internals.md` §8 coordinator row names the station `coordinator/` ingress and the deck grant modules.

## Round 52 (2026-10-12, 15:35–)

- Committed round 51 as 8798ac2. `m3/orchestration`, `m3/board`, `m3/ops`: no new commits. Lane B has no LANE COMPLETE (6-5 Coordinator: 49 uncommitted files in its worktree), so 6-13, 7-7 and 7-8 still wait.
- 7-6 docs while waiting: `docs/v2/internals.md` §8 "Where each M3 feature lives" (feature → station, deck and board code and e2e spec). Every listed path exists (checked with a shell loop).

## Round 51 (2026-10-12, 15:34–)

- Committed round 50 as b690031. `m3/orchestration`, `m3/board`, `m3/ops`: no new commits. Lane B has no LANE COMPLETE (6-5 Coordinator still uncommitted in its worktree): 6-13, 7-7 and 7-8 still wait.
- 7-6 docs corrections while waiting (the lane rules allow 7-6 work): `README.md` (revert: rejected revert changes nothing, an unproven revert needs reconciliation and blocks input; auto-continue only with the direct host), `docs/v2/operations.md` §7.1 (the import uses the mode of the spawn form; a provider instance stores no mode; the import ignores workspace and batch) and §10 (shim host → `unsupported`; the dispatch rechecks only identity, proven end, newer owner work, stop and window). These correct the documentation follow-ups of rounds 49 and 50; the code follow-ups stay open.
- Test: `cargo nextest run -p fleetdeck-cli --test docs_commands` → 1 passed.

## Round 50 (2026-10-12, 15:32–)

- Committed round 49 as c460b9b.
- Merged `m3/board` (cce6d03, lane C's own merge of v2) as a97cdf4; the spawn form, `spawn.ts` and `manager/tests.rs` take lane C's resolution (same behaviour; an import also unblocks the clone readiness check). `bun test src/spawn` 10 passed; biome lint and typecheck clean.
- `m3/orchestration` and `m3/ops`: no new commits. Lane B has no LANE COMPLETE (6-5 Coordinator uncommitted in its worktree), so 6-13, 7-7 and 7-8 still wait.
- Recorded the round-50 review findings as follow-ups.

## Round 49 (2026-10-12, 15:26–)

- Committed round 48 (7-6) as c0aad4f.
- Merged `m3/board` (6-12 import, rest of 6-8; LANE COMPLETE) as e2ca83b. Conflicts: `decide.rs` `start()` takes both the workspace request (6-1) and the import id (6-12; an import is a `Resume` effect, a start keeps its workspace plan); method counts 36 (link) and 22 (mutations); `manager/tests.rs` both modules; PROGRESS rows 6-11 and 6-12 both `[x]`; board spawn form keeps batch, clone and worktree, plus the import field (an import is one session without a workspace request); `e2e/spawn.spec.ts` keeps both tests; `make ts-types`.
- Checks after the merge: `cargo build --workspace --all-targets` clean; nextest (import, method table, mutation, workspace, drain filters) 73 passed; `bun test src/spawn` 10 passed; biome lint and typecheck clean.
- `m3/orchestration` and `m3/ops`: no new commits. Lane B (6-5 Coordinator) is not complete, so 6-13, 7-7 and 7-8 wait.
- 7-6 docs update for the merged 6-12 import: `docs/v2/operations.md` §7.1, `docs/v2/cutover.md`.
- Recorded the round-49 review findings on 7-6 as follow-ups.

## Round 48 (2026-10-12, 15:14–)

- Committed round 47 (7-3) as 2ff7794.
- Merged `m3/orchestration` (6-1, 6-3, 6-4, 6-2), `m3/board` (6-10, 6-7; conflicts: `UserInput` with images, `SessionCommand::Start.workspace`, deck schema V7 mail/tasks then V8 file touches) and `m3/ops` (7-5, 7-4, 6-11; PROGRESS rows of both lanes, `app.css`). `make ts-types`; fmt, clippy, biome lint and typecheck clean after `bun install --frozen-lockfile`.
- 7-6 Docs: new `README.md` (v2), the v1 README moved to `docs/v0/README.md`, new `docs/v2/operations.md`, `docs/v2/cutover.md` and `docs/v2/internals.md` (DL-129).
- Test: `crates/fleetdeck-cli/tests/docs_commands.rs` `every_documented_fleetdeck_command_and_flag_exists` → PASS.
- Recorded the round-48 review findings on 7-3 as follow-ups.
- Lanes: only `m3-ops` says LANE COMPLETE; 6-13, 7-7 and 7-8 wait.

## Phase 7 acceptance evidence map (pre-PR review preparation)

This map is for the architect's adversary PRE-PR REVIEW. It is not a claim of that review. Every row names the test or the target that proves the acceptance line of the M3 goal.

| Acceptance line | Evidence |
| --- | --- |
| ShimHost: survive a station restart and upgrade, fencing, exclusive reattachment, output cursor recovery, explicit exit state, socket permissions, update and restart failures | The ADR-005 §9 test map below; `crates/fleetdeck-cli/tests/shim_*.rs`, `station_shim_sigkill.rs`, `station_shim_revert_crash.rs`, `host::shim::tests`. |
| Auto-continue default OFF with the A10 rechecks (ADR-007) | `crates/fleetdeck-station/src/manager/auto_continue_tests.rs`: `auto_continue_is_off_by_default_and_a_restart_never_continues_a_session_without_it`, `a_coordinator_or_the_local_socket_cannot_enable_auto_continue`, `a_session_lost_with_execution_unknown_is_never_continued`, `an_input_with_an_unknown_outcome_prevents_the_continuation_and_is_never_resent`, `a_changed_provider_instance_or_native_identity_prevents_the_continuation`, `a_crash_after_the_resume_before_the_input_never_starts_a_second_process_and_never_resends`, `the_continued_session_runs_with_the_stored_mode_never_a_more_permissive_one` and 9 more. |
| Station drain | `crates/fleetdeck-station/src/manager/drain_tests.rs`: `a_draining_station_refuses_new_work_and_reports_the_busy_sessions`, `only_the_commands_that_create_work_are_refused_while_draining`, `a_draining_station_refuses_an_import`. |
| Self-upgrade and rollback | `crates/fleetdeck-cli/tests/station_upgrade.rs`: `a_drained_station_upgrades_and_rolls_back_binary_only_after_it_went_live`, `a_rollback_before_the_new_station_ran_restores_the_snapshot_with_a_new_incarnation`. |
| N-1 version skew | `crates/fleetdeck-link/tests/version_skew.rs`: `a_deck_and_a_station_one_build_apart_negotiate_the_shared_features`, `a_method_of_the_newer_build_is_refused_by_an_older_peer_and_the_shared_ones_work`, `an_event_of_a_newer_build_is_kept_as_unknown_and_written_back_unchanged`, `a_stored_event_and_summary_of_the_previous_build_decode_with_defaults`. |
| Performance bars of PLAN §13 | `make perf` (`crates/fleetdeck-cli/tests/perf.rs`, ignored tests, release build); results in the 7-4 line of `docs/v2/PROGRESS.md` (lane D, DL-401). |
| cargo-dist, systemd, launchd, container file | `crates/fleetdeck-cli/tests/packaging.rs`: `the_cargo_dist_config_releases_fleetdeck_for_linux_and_macos_with_the_packaging_files`, `the_systemd_units_start_real_commands_and_the_station_unit_keeps_its_agents`, `the_launchd_agents_are_well_formed_and_start_real_commands`, `the_container_file_builds_a_non_root_station_image_with_pinned_versions`, `the_offline_built_binary_reports_its_version`. Image build, macOS and tailnet: owner steps (`docs/v2/operations.md` §12). |
| README, internals, operations guide | `README.md`, `docs/v2/internals.md`, `docs/v2/operations.md`, `docs/v2/cutover.md`; `crates/fleetdeck-cli/tests/docs_commands.rs` checks the documented commands. |
| Live smoke per provider (opt-in) | `make smoke-live` (`FD_LIVE_SMOKE=1`, `crates/fleetdeck-providers/tests/live_smoke.rs`), documented in `docs/v2/LIVE-SMOKE.md`. |
| GATE-M2 §6 | P2-a, P2-b and SIGKILL survival lines of `docs/v2/PROGRESS.md`; `crates/fleetdeck-cli/tests/station_sigkill.rs::providers_of_a_sigkilled_station_exit_and_the_restarted_station_reports_them_lost`. |
| v0 cutover, final `make check` | Open: items 7-7 and 7-8 (wait for lane B). |

Limits of this map (review of round 71). Two rows above claim more than the tests show:

- Auto-continue: the tests cover default OFF and the dispatch rechecks of identity, proven end, newer owner work, stop and window. A session with the shim host gets `unsupported`. The map does not prove the full A10 recheck list for every host.
- ShimHost upgrade: `crates/fleetdeck-cli/tests/station_upgrade.rs` runs with the default `direct` host. ADR-005 tests 20, 21, 22 and 48 are unit tests in `crates/fleetdeck-station/src/upgrade_tests.rs`. No end-to-end test shows a shim agent that survives a station upgrade.

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
| 20, 21, 22, 48 | covered (round 47, 7-3) | `crates/fleetdeck-station/src/upgrade_tests.rs` (exact names) |
| 23 | covered | `shim_manager.rs` `a_shim_session_keeps_its_turn_checkpoints_and_survives_a_station_restart` |
| 24 | not built | auto-continue under the shim host skips `unsupported` (DL-127 deviation 7) |
| 25 | covered | `shim_station.rs` `uncertain_inputs_are_settled_before_commands_are_admitted_after_a_station_start` |
| 26–29, 34 | covered | `shim_stdin.rs` |
| 39 | covered (round 46) | `shim_gap.rs` `an_unterminated_last_line_at_eof_is_committed_as_malformed_before_the_exit_is_acknowledged` |
| 40 | partial | `shim_manager.rs` (module `live_exit`) `a_live_shim_exit_with_a_surviving_child_keeps_the_evidence_and_resume_stays_refused`, `manager::tests::resume` |
| 44 | open | exit file on ENOSPC → follow-up |
| 45 | open | `ack_exit` fault points → follow-up |

## Review follow-ups (after M3)

| Item | Finding | File |
| --- | --- | --- |
| 7-6 | CLAIM NARROWED (round 72, "Limits of this map"); the missing A10 recheck tests stay open as 7-2 follow-ups. The Phase 7 evidence map overstated the A10 coverage: auto-continue rechecks only identity, proven end, newer owner work, stop and window, and the shim host is `unsupported`. Add the missing A10 recheck tests or narrow the claim. | `crates/fleetdeck-station/src/manager/auto_continue_tests.rs`, `docs/v2/progress/m3-durable.md` |
| 7-6 | The Phase 7 evidence map overstated the ShimHost upgrade coverage: `station_upgrade.rs` uses the `direct` host. Add an end-to-end test where a shim agent survives a station upgrade. | `crates/fleetdeck-cli/tests/station_upgrade.rs` |
| 7-1 | ADR-005 test 3: a named fault point between the attach and the replay | `crates/fleetdeck-cli/tests/shim_recovery.rs` |
| 7-1 | ADR-005 test 16: a stale socket file is replaced, a live one is refused | `crates/fleetdeck-providers/src/host/shim/launch.rs` |
| 7-1 | ADR-005 test 44: the exit file write on ENOSPC | `crates/fleetdeck-providers/src/host/shim/server_proc.rs` |
| 7-1 | ADR-005 test 45: `ack_exit` fault points | `crates/fleetdeck-providers/src/host/shim/server_conn.rs` |
| 7-1 | ADR-005 test 40: the full matrix of exit evidence | `crates/fleetdeck-cli/tests/shim_manager.rs` |
| 7-2 | ADR-007 test 15: a resume that reports another native identity ends the new process with `outcome_unknown` | `crates/fleetdeck-station/src/manager/target.rs` |
| 7-2 | Rule 10 (workspace, slot) is not rechecked at dispatch | `crates/fleetdeck-station/src/store/auto_continue.rs` |
| 7-2 | Auto-continue under the shim host (ADR-005 test 24) | `crates/fleetdeck-station/src/manager/auto_continue.rs` |
| 7-2 | A Playwright phone-viewport flow for the toggle | `web/e2e/` |
| 7-2 | A10 dispatch rechecks are incomplete: disable, turn completion, maintenance, capability, workspace and capacity are not rechecked right before the dispatch (review of round 46) | `crates/fleetdeck-station/src/store/auto_continue.rs` |
| 7-2 | Timestamp comparisons (`created_at > turn_at`, `>= lost_at`) miss an owner command in the same millisecond; compare `station_seq` instead (review of round 46) | `crates/fleetdeck-station/src/store/auto_continue.rs` |
| 7-2 | Phone-viewport Playwright flow for the auto-continue toggle and the continuation label (review of round 46) | `web/e2e/` |
| 7-3 | No bind trial: the new build does not try the link port and the local socket before activation (DL-128 deviation 1) | `crates/fleetdeck-station/src/upgrade.rs` |
| 7-3 | The service restart between `activate` and the new station is manual; wire it into the systemd/launchd units of 7-5 | `crates/fleetdeck-cli/src/upgrade.rs` |
| 7-3 | No checksum or signature check of the new binary before the trial | `crates/fleetdeck-cli/src/upgrade.rs` |
| 7-3 | The shim launch does not write `shims.protocol` (default 1 is right until protocol 2) | `crates/fleetdeck-station/src/store/shims.rs` |
| 7-3 | The drain flag is in memory and the board has no drain control | `crates/fleetdeck-station/src/manager/drain.rs` |
| 7-3 (review r48) | Drain admission and queued-work races: a command admitted just before the flag, or a queued effect, can still start work during a drain | `crates/fleetdeck-station/src/manager/drain.rs` |
| 7-3 (review r48) | The database snapshot and restore are not locked and not atomic (plain file copies) | `crates/fleetdeck-station/src/upgrade.rs` |
| 7-3 (review r48) | The live marker fails open, and activation has crash windows between the record and the symlink swap | `crates/fleetdeck-station/src/upgrade.rs` |
| 7-3 (review r48) | Stale or repeated rollbacks are not refused | `crates/fleetdeck-station/src/upgrade.rs` |
| 7-3 (review r48) | No guard for lost shims and no adapter checkpoint compatibility check in the trial | `crates/fleetdeck-station/src/upgrade.rs` |
| 7-3 (review r48) | The trial run of the new binary has no time bound | `crates/fleetdeck-cli/src/upgrade.rs` |
| 7-3 (review r48) | ADR-005 tests 20/21/22/48 use store rows, not a real ShimHost of two builds; N−1 is tested at the link layer only | `crates/fleetdeck-station/src/upgrade_tests.rs` |
| 7-6 (review r49) | FIXED (commit 8798ac2): operations §10 "Limits of this release" says the shim host gets `unsupported`. The docs do not say that auto-continue is not supported on the shim host (a shim session gets `unsupported`) | `docs/v2/operations.md` §10 |
| 7-6 (review r49) | FIXED (commit 8798ac2): operations §10 lists the five rules that the dispatch rechecks and the rules that it does not. The docs present the auto-continue rechecks as complete; the dispatch recheck covers only part of the A10 rules (see the 7-2 follow-ups) | `docs/v2/operations.md` §10, `docs/v2/internals.md` |
| 7-6 (review r49) | FIXED (commit 8798ac2): `README.md` names the reconciliation state. The README says a revert changes nothing on failure; a revert that cannot be proven needs reconciliation and blocks input (PLAN §6.6) | `README.md` |
| 6-12 merge (review r50) | FIXED round 56 (DL-130, `a_draining_station_refuses_an_import`): `session.import` is not in the drain guard's list of commands that create work, so an import passes a drain | `crates/fleetdeck-station/src/manager/drain.rs` |
| 6-12 merge (review r50) | An import of a native session whose earlier session is `lost` with an unresolved shim can run that conversation twice (no unended-execution check for the native id). Round 82: documented as a caution in `docs/v2/operations.md` §7.1; the code check stays open. | `crates/fleetdeck-station/src/manager/decide.rs` |
| 6-12 merge (review r50) | The board keeps the clone place and directory choices while an import id is set; the import ignores them without a visible note | `web/src/spawn/SpawnForm.tsx` |
| 7-6 (review r50) | FIXED (commit 8798ac2): §7.1 says the import uses the mode of the spawn form. `docs/v2/operations.md` §7.1 says the import uses "the stored mode of the provider instance"; a provider instance has no stored permission mode (the import uses the mode of the spawn form) | `docs/v2/operations.md` |
| 7-6 (review r84) | FIXED in round 85: `docs/v2/operations.md` §7.2 gave the coordinator start steps without saying that every provider refuses the start with `handoff_unverified` now (`crates/fleetdeck-providers/src/coordinator.rs::handoff`). §7.2 and the §11 row state the limit now. The steps stay valid for a verified provider version. | `docs/v2/operations.md` |
| 7-8 prep (review r78) | The round-78 log said the 700-line probe lists two files; it lists 17, including the 798-line `host.rs`. Corrected in the round-78 entry. Add a test that runs `tools/module-size.sh` with a lower limit, so the probe count is evidence and not a hand count. Closed in round 80: `crates/fleetdeck-cli/tests/module_size.rs`. | `tools/module-size.sh`, `docs/v2/progress/m3-durable.md` |
| 6-5 (review r89, lane B code, merged f8dbc14) | No send-time expiry check: `LinkForward::flush` ends rows past `retry_until` once (`coordinator_expire`) and then sends every `pending` row; `transmit` checks only the binding fence (`fence`), not the row's own `retry_until`, so a row that passes `retry_until` during the flush loop or a slow reconnect is still sent. Check `retry_until` in `transmit` before the send and add a test with a clock that moves past it between expire and send. | `crates/fleetdeck-station/src/coordinator/outbox.rs` (`transmit`, `flush`) |
| 6-5 (review r89, lane B code, merged f8dbc14) | The outbox tests do not prove deduplication: `a_call_sent_before_a_link_loss_is_sent_again_with_the_same_envelope_and_deduplicated` only proves that the station resends the same id and bytes to a fake deck; no test sends the duplicate to the real deck intake and proves one command and one effect. Add a deck-side test. | `crates/fleetdeck-station/src/coordinator/outbox_tests.rs` |
| 6-5 (review r89, lane B code, merged f8dbc14) | The outbox tests do not prove lifecycle acknowledgment: `sent_rows_are_not_resent_after_a_reconnect_before_the_lifecycle_watermark_is_acknowledged` tests only the local fence; no §6.1 lifecycle watermark is sent or acknowledged (DL-218 deviation). Add the watermark acknowledgment and a test that a resend waits for it. | `crates/fleetdeck-station/src/coordinator/outbox_tests.rs`, ADR-006 §6.1 |
