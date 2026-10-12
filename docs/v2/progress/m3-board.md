# Lane C (board), M3: round log

Lane C works in `fleet-deck-worktrees/m3-board` on branch `m3/board`. Items, in this order: 6-6 Plans, 6-9 Usage meters, 6-10 Images and notifications, 6-7 Conflict radar, the rest of 6-8 Files and checkpoints, 6-12 Import. Decisions: `docs/v2/decisions/m3-board.md` (DL-300 to DL-399).

## Round 1 (2026-10-12)

- Start: the tree was clean; `git merge --no-edit v2` done and committed.
- Done: 6-6 Plans (DL-301), 6-9 Usage meters (DL-303). Recorded the owner decision "fast rounds and parallel lanes" (DL-300) and a free Playwright port for parallel lanes (DL-302).
- 6-6 evidence:
  - Wire: `crates/fleetdeck-proto/src/plans.rs` (`DeckPlan`, `PlanStatus`, `PlanMarkRequest`, `bounded_plan`); TS bindings `web/src/gen/plans/`.
  - Deck: schema V6 `plans` (`crates/fleetdeck-deck/src/store/schema.rs`), `crates/fleetdeck-deck/src/store/plans.rs` (written in the fold transaction of `apply_events`), routes `crates/fleetdeck-deck/src/http/plan_routes.rs` (`plans.list` read, `plans.mark` drive + CSRF + audit).
  - Board: `web/src/board/plans.ts`, `web/src/board/PlanLibrary.tsx`, the "Plans" screen in `web/src/App.tsx`, "Start a session with this plan" fills the first prompt of `web/src/spawn/SpawnForm.tsx`.
  - Tests: `plans::tests::{a_long_plan_is_cut_at_a_character_boundary, statuses_round_trip}`, `crates/fleetdeck-deck/tests/board_plans.rs::{a_proposed_plan_enters_the_library_once_and_keeps_the_owner_status, plan_marks_need_drive_csrf_and_a_known_plan}`, `web/src/board/plans.test.tsx` (4), Playwright `web/e2e/plans.spec.ts` (2, phone viewport).
- 6-9 evidence:
  - Wire: `SessionSummary.rate_limits` (`crates/fleetdeck-proto/src/session.rs`), merge by scope in `crates/fleetdeck-proto/src/fold.rs`; Codex `account/rateLimits/updated` → `usage.updated` (`crates/fleetdeck-providers/src/codex/limits.rs`).
  - Board: `web/src/board/usage.ts`, `web/src/board/UsageMeters.tsx` (the "Usage" panel of the fleet screen).
  - Tests: `fold::tests::rate_limit_windows_merge_by_scope_and_a_sparse_update_keeps_the_others`, `codex::limits::tests::codex_rate_limit_windows_keep_their_scope_and_skip_missing_windows`, `capability_matrix::row_usage_reports_tokens_for_both_and_cost_for_claude_only` (now also checks that a rate-limit-only update has unknown usage), `web/src/board/usage.test.tsx` (3), Playwright `web/e2e/usage.spec.ts` (1). The Codex board timeline fixtures `web/src/testing/fixtures/codex-*.events.json` were regenerated (`FD_UPDATE_FIXTURES=1`), because the recorded Codex scenarios contain rate-limit updates.
- Commands:
  - `cargo nextest run -p fleetdeck-deck -p fleetdeck-proto -E 'binary(board_plans) | test(plans::) | test(names_and_routes) | test(the_migrations_are_valid)'` → 6 passed.
  - `cargo nextest run -p fleetdeck-proto -p fleetdeck-providers` → 271 passed.
  - `bun test src` (in `web/`) → 111 passed; `bunx tsc --noEmit -p tsconfig.test.json` → clean.
  - `FD_E2E_PORT=<free port> bunx playwright test e2e/plans.spec.ts e2e/usage.spec.ts` → 3 passed.
  - `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `bun run --cwd web lint`: see the round summary.
- Next: 6-10 Images and notifications.

## Round 2 (2026-10-12, 14:40–15:05)

- Start: committed the round-1 work (`bb1a63c`); `git merge --no-edit v2` → already up to date.
- Review of round 1: two findings went to the follow-up list (snapshot-only plans, stale rate-limit windows).
- 6-10 evidence (DL-304, DL-305):
  - Wire: `crates/fleetdeck-proto/src/input.rs` (`UserInput.images`, `ImageInput`, limits); deck refusal `400 invalid_input` in `crates/fleetdeck-deck/src/http/command_routes.rs`.
  - Providers: Claude image content blocks (`crates/fleetdeck-providers/src/claude/control.rs`), Codex image input items (`crates/fleetdeck-providers/src/codex/rpc.rs::input_items`, used by `turn/start` and `turn/steer`).
  - Board: `web/src/session/Composer.tsx` (paste, "Add image", remove), `web/src/session/images.ts`; notifications `web/src/board/notify.ts` and the "Notify me" toggle in `web/src/board/NeedsYouRail.tsx`.
  - Tests: `input::tests::{an_input_without_images_keeps_the_old_wire_form, image_limits_refuse_bad_types_bad_base64_and_too_many_bytes}`, `a_claude_prompt_with_images_sends_image_blocks_before_the_text`, `a_codex_turn_input_carries_images_as_data_url_items`, `http_commands::a_prompt_with_a_pasted_image_reaches_the_station_and_a_bad_image_is_refused`, `web/src/session/images.test.tsx` (6), Playwright `web/e2e/images.spec.ts` (2).
- 6-7 evidence (DL-306):
  - Station: `RepoInfo` from `crates/fleetdeck-core/src/git.rs::repo_of`, stamped into the summary and `session.started` (`crates/fleetdeck-station/src/store/log.rs`).
  - Deck: schema V7 `file_touches`, `crates/fleetdeck-deck/src/store/conflicts.rs`, `GET /api/conflicts` (method `conflicts.list`, `read`).
  - Board: `web/src/board/ConflictRadar.tsx`, `web/src/board/conflicts.ts` (fleet screen).
  - Tests: `git::tests::{remote_urls_normalize_to_host_and_path, the_repo_root_and_its_single_origin_are_found_from_a_subdirectory}`, `conflicts::tests::a_file_in_a_repo_with_one_origin_groups_across_stations`, `store::tests::the_started_event_carries_the_repository_of_the_workspace`, `board_conflicts::the_same_file_changed_on_two_stations_is_one_conflict_inside_the_window`, `web/src/board/conflicts.test.tsx` (3), Playwright `web/e2e/conflicts.spec.ts` (1).
- Commands:
  - `cargo nextest run -p fleetdeck-deck -p fleetdeck-proto -p fleetdeck-providers -E '<the 6-10 tests> | binary(ts_drift)'` → 7 passed; `cargo nextest run -p fleetdeck-providers` → 210 passed.
  - `cargo nextest run -p fleetdeck-core -p fleetdeck-proto -p fleetdeck-station -p fleetdeck-deck -E 'test(git::) | test(conflicts) | test(the_started_event_carries) | binary(ts_drift) | test(fold) | test(names_and_routes) | test(the_migrations_are_valid)'` → 28 passed (after `make ts-types`).
  - `bun test src` (in `web/`) → 120 passed; `bunx tsc --noEmit -p tsconfig.test.json` → clean.
  - `FD_E2E_PORT=<free port> bunx playwright test e2e/images.spec.ts e2e/session.spec.ts e2e/conflicts.spec.ts e2e/usage.spec.ts` → 2 + 5 + 1 + 1 passed.
  - `cargo fmt --all --check` → ok; `cargo clippy --workspace --all-targets -- -D warnings` → ok; `bun run --cwd web lint` → ok.
- Note: the new `session.started` field `repo` is boxed (`Option<Box<RepoInfo>>`) because clippy `large_enum_variant` failed on `EventBody`.
- Next: the rest of 6-8 Files and checkpoints, then 6-12 Import.

## Review follow-ups (after M3)

| Item | Finding | File |
|---|---|---|
| 6-6 | "Assign a plan via mail" (v0 parity) waits for lane B 6-2 Mail. | `web/src/board/PlanLibrary.tsx` |
| 6-6 | A plan that the deck receives only through a snapshot (link gap, no events) does not enter the library. | `crates/fleetdeck-deck/src/store/plans.rs` |
| 6-9 | Rate limits are per provider account; the board keys them by station and provider instance. | `web/src/board/usage.ts` |
| 6-9 | A rate-limit-only `usage.updated` moves the summary's `updated_at` (lane sort order). | `crates/fleetdeck-proto/src/fold.rs` |
| 6-6 | Review round 1: a plan that reaches the deck only in a snapshot (link gap) is missing from the library. | `crates/fleetdeck-deck/src/store/plans.rs` |
| 6-9 | Review round 1: an older rate-limit window can replace a newer one, because the board picks the window of the session with the newest `updated_at`, not the newest report. | `web/src/board/usage.ts` |
| 6-10 | Recorded host commands (`HostCmd::Prompt`) keep the text only; a replay has no images. No live smoke test with an image. | `crates/fleetdeck-providers/src/claude/host_cmd.rs` |
| 6-10 | No Web Push or service worker: a closed board gets no notification; failed or finished turns make no notification. | `web/src/board/notify.ts` |
| 6-7 | No owner setting to group repositories without one origin across stations (PLAN §6.6 "explicit owner choice"). | `crates/fleetdeck-proto/src/conflicts.rs` |
| 6-7 | No warning to the two sessions (v0 mail and in-context whisper); ended sessions count inside the window. | `crates/fleetdeck-deck/src/store/conflicts.rs` |
