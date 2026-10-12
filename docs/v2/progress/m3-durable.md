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
| 7-6 (review r49) | The docs do not say that auto-continue is not supported on the shim host (a shim session gets `unsupported`) | `docs/v2/operations.md` §10 |
| 7-6 (review r49) | The docs present the auto-continue rechecks as complete; the dispatch recheck covers only part of the A10 rules (see the 7-2 follow-ups) | `docs/v2/operations.md` §10, `docs/v2/internals.md` |
| 7-6 (review r49) | The README says a revert changes nothing on failure; a revert that cannot be proven needs reconciliation and blocks input (PLAN §6.6) | `README.md` |
| 6-12 merge (review r50) | FIXED round 56 (DL-130, `a_draining_station_refuses_an_import`): `session.import` is not in the drain guard's list of commands that create work, so an import passes a drain | `crates/fleetdeck-station/src/manager/drain.rs` |
| 6-12 merge (review r50) | An import of a native session whose earlier session is `lost` with an unresolved shim can run that conversation twice (no unended-execution check for the native id) | `crates/fleetdeck-station/src/manager/decide.rs` |
| 6-12 merge (review r50) | The board keeps the clone place and directory choices while an import id is set; the import ignores them without a visible note | `web/src/spawn/SpawnForm.tsx` |
| 7-6 (review r50) | `docs/v2/operations.md` §7.1 says the import uses "the stored mode of the provider instance"; a provider instance has no stored permission mode (the import uses the mode of the spawn form) | `docs/v2/operations.md` |
