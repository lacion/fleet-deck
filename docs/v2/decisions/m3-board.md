# Decisions of lane C (board), M3

Lane C numbers its decisions DL-300 to DL-399 (parallel lanes, owner change of 2026-10-12). The format is the format of `docs/v2/DECISIONS-LOG.md`.

## DL-300: Fast rounds and parallel lanes (owner decision, 2026-10-12)

- Context: the owner wants a usable M3 by 16:20 CEST on 2026-10-12 and gives feedback from real use.
- Choice (owner): four goal loops build M3 at the same time, each in its own worktree and branch (lane C: `m3/board`, items 6-6, 6-9, 6-10, 6-7, the rest of 6-8, 6-12). Each item gets one review; its findings go to the "Review follow-ups (after M3)" list in `docs/v2/progress/m3-board.md`. Normal rounds run only `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `bun run --cwd web lint`; the full `make check` runs once at the end, in lane A.
- Owner review: approved (owner decision, 2026-10-12)

## DL-301: The plan library in the deck (M3 round 1, 6-6)

- Context: PLAN Phase 6 "Plans (capture from Claude ExitPlanMode and Codex plan items), plan library" and §16 "Plan library | Keep; capture from the protocol". The providers already emit `plan.proposed { plan_id, turn_id?, markdown }` (`crates/fleetdeck-providers/src/claude/map.rs`, `codex/map.rs`). The deck keeps the per-session timeline only for the current incarnation, so a plan must have its own durable row. v0 `board/src/components/PlanLibrary.tsx` had the statuses proposed, approved, captured, executed and archived, an "execute" that spawned a session, and "assign via mail".
- Options: (a) derive the library from the timelines on each request; (b) a deck table `plans`, written in the same transaction as the folded event; (c) a station-side library over the link. (a) loses plans when a station incarnation clears its timeline; (c) adds a link method and loses plans of a removed station.
- Choice: (b). Deck schema V6 `plans` (key `station/session/plan`, no foreign key to `stations` so that the library survives a station removal; `INSERT OR IGNORE`, so a replay or a new incarnation with the same plan id keeps the first text and the owner's status). The text is bounded to 256 KiB at a character boundary (`fleetdeck_proto::plans::bounded_plan`, flag `truncated`). Statuses: `proposed`, `executed`, `archived` (the v0 `approved` and `captured` came from v0 hook paths that v2 does not have; an approval of a plan is the provider's own permission flow). Routes: `GET /api/plans[?archived=true]` (read) and `POST /api/plans/mark {id, status}` (drive, CSRF, audit `plans.mark`). Board: a "Plans" screen; "Start a session with this plan" opens the spawn form with the plan as the first prompt (the owner chooses the station and the directory; nothing starts by itself).
- Not done (follow-up): "assign via mail" waits for lane B (6-2 Mail). A plan that a station emitted while the link was down and that the deck receives only as a snapshot (no events) is not in the library.
- Owner review: pending (executor decision, M3 round 1)

## DL-302: A free port for the Playwright tests of a lane (M3 round 1)

- Context: the lane rules forbid fixed test ports, but `web/playwright.config.ts` serves the board on port 4179.
- Choice: the config reads `FD_E2E_PORT` and keeps 4179 as the default, so `make` targets and lane A do not change. A lane runs its new specs with a free port: `FD_E2E_PORT=<free port> bunx playwright test e2e/<spec>`.
- Owner review: pending (executor decision, M3 round 1)

## DL-303: Usage and rate-limit meters (M3 round 1, 6-9)

- Context: PLAN Phase 6 "Usage and rate-limit meters ("unknown" is never shown as zero)" and §11 fleet card "usage". The summary already summed token usage with unknown parts as `None`; the `rate_limits` of `usage.updated` were dropped by the fold, and Codex `account/rateLimits/updated` was an ignored notification.
- Options: (a) a deck table of the latest windows per station and provider instance; (b) the latest windows per scope in `SessionSummary.rate_limits`, and the board picks the newest per station, provider instance and scope. (a) needs a schema, a route and a delta type; (b) reuses the snapshot and delta of the summary.
- Choice: (b). `SessionSummary.rate_limits` (`#[serde(default)]`, so an N-1 summary without it reads as "no window", not zero), merged by scope (a sparse update keeps the other scopes), at most 16 scopes. Codex: `account/rateLimits/updated` maps to `usage.updated` with all usage parts unknown and the windows `<limitId>.primary|secondary` (`usedPercent`, `resetsAt` seconds → ms; a missing window is not reported). Board: a "Usage" panel on the fleet screen with the totals of the shown sessions ("unknown" when no session reported a part, "at least N (k sessions unknown)" when some did not) and one meter per window; a window with an unknown percentage shows "use unknown" and no bar.
- Not done (follow-up): rate limits are per provider account, but the board keys them by station and provider instance (the station does not expose the account id). A rate-limit-only `usage.updated` changes the summary's `updated_at`.
- Owner review: pending (executor decision, M3 round 1)
