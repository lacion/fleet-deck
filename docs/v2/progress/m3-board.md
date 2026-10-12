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

## Review follow-ups (after M3)

| Item | Finding | File |
|---|---|---|
| 6-6 | "Assign a plan via mail" (v0 parity) waits for lane B 6-2 Mail. | `web/src/board/PlanLibrary.tsx` |
| 6-6 | A plan that the deck receives only through a snapshot (link gap, no events) does not enter the library. | `crates/fleetdeck-deck/src/store/plans.rs` |
| 6-9 | Rate limits are per provider account; the board keys them by station and provider instance. | `web/src/board/usage.ts` |
| 6-9 | A rate-limit-only `usage.updated` moves the summary's `updated_at` (lane sort order). | `crates/fleetdeck-proto/src/fold.rs` |
