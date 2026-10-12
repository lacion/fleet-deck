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

## DL-304: Image paste into prompts (M3 round 2, 6-10)

- Context: PLAN Phase 6 "Screenshot/image paste into prompts" and §16 "Shift+Enter, Ctrl+V screenshot paste → composer newline; image paste into prompts". `UserInput` carried text only. Both providers accept images in a user turn: Claude stream-json `image` content blocks with a base64 `source`; Codex app-server `turn/start` / `turn/steer` input items `{"type":"image","url":"data:..."}`.
- Options: (a) upload the image to the station first (a new file method and storage) and send a path; (b) send the image inline as base64 inside the command.
- Choice: (b), the smallest change that reaches both providers without new storage. `UserInput.images: Vec<ImageInput { media_type, data }>` with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`, so an input without images keeps the old wire form and an N-1 peer reads it. Limits in `crates/fleetdeck-proto/src/input.rs`: at most 4 images, at most 768 KiB of base64 together (the command goes to the station in one control frame of at most 1 MiB), PNG/JPEG/GIF/WebP only, standard base64. The deck refuses an input above the limits with `400 invalid_input` before it stores or sends anything. The board checks the same limits first, draws a large image again as JPEG (long side 1600 px) when the browser can, and offers an "Add image" file picker for phones next to the paste. The textarea keeps Enter as a newline (Shift+Enter parity); the Send button sends.
- Not done (follow-up): the recorded Claude/Codex host commands (`HostCmd::Prompt`) keep the text only, so a replay of a recorded scenario has no images; no live smoke test with an image.
- Owner review: pending (executor decision, M3 round 2)

## DL-305: Needs-you notifications in the browser (M3 round 2, 6-10)

- Context: PLAN Phase 6 "notifications". The v0 board has no push service; v2 has no service worker.
- Choice: the board shows a browser `Notification` ("Needs you: <session>", body = the request title, tag = `station/session/request`) for each open request that appears while notifications are on. The owner turns them on with "Notify me" in the needs-you rail (the browser permission prompt); the setting stays in this browser (`localStorage` `fleetdeck.notify`). Requests that are open at page load or at the turn-on make no notification, so a reload makes no storm. A click focuses the board and opens the session. A browser without the API or with a denied permission shows a note instead of the button.
- Not done (follow-up): no Web Push / service worker, so a phone with the board closed gets nothing; no notification for a failed or finished turn.
- Owner review: pending (executor decision, M3 round 2)

## DL-306: Conflict radar across stations (M3 round 2, 6-7)

- Context: PLAN §16 "Conflict radar | Keep, across stations", §10 `file_touches` (conflict radar input), §6.6 "a normalized origin URL is a heuristic, not an identity. Repos with several remotes or no remote are grouped only by explicit owner choice." v0 (`src/daemon/ledger.ts`) records each edit and reports a rival edit of the same file in 30 minutes.
- Choice:
  - The station records the repository of the workspace when it creates a session: `RepoInfo { root, origin }` (`fleetdeck_core::git::repo_of`, no `git` process; `commondir` of a linked worktree is followed). `origin` is the normalized URL (`host/path`, host in lower case, no user, port or `.git`) only when the repository has exactly one remote. It goes into `SessionSummary.repo` and the `session.started` event (optional field, an N-1 peer ignores it).
  - The deck folds `file.changed` paths and the `files` of `tool.completed` into `file_touches` (schema V7), in the same transaction as the event, with the latest touch per (group, file, station, session). Group: `origin:<origin>` across stations; else `root:<station>:<root>` (one work tree on one station); a file outside the repository is grouped by station with its absolute path. A relative path is relative to the session's `cwd`. Rows older than two windows are deleted.
  - `GET /api/conflicts` (`read`): every file that two or more sessions touched in the last 30 minutes, newest first, at most 200. The board shows the "Conflict radar" panel on the fleet screen; it loads again when the fleet changes; a session of a conflict opens from the panel.
- Not done (follow-up): the explicit owner choice to group repositories without one origin (no owner setting yet, so they never group across stations); no mail or in-context warning to the two sessions (v0 whisper); ended sessions still count inside the window.
- Owner review: pending (executor decision, M3 round 2)
