# Fleetdeck UX Feedback — Consolidated Implementation Plan

Five confirmed items, ordered by user-impact-per-effort into three independently shippable phases. Verifier corrections are incorporated and flagged inline where they change the diagnosis.

---

## Phase 1 — Dead ends and dishonest UI (all S effort, board-only, ships as one release)

Theme: the board currently contains a broken command, a dead button, a missing close affordance, and a misleading Execute action. All are small, low-risk edits that remove the sharpest "nothing happened / I can't tell what's going on" moments.

### 1.1 onboarding — fix the empty state, add first-run concept help

**Root cause.** Onboarding is pull-based reference only: no first-run experience, the one true empty state prints a command that does not exist (`$ fleetd up && claude` — a stale artifact inlining the daemon's boot log line), and core nouns (session/card/pane/worktree/plan/mail vs orchestrator) are never defined in-product.

**Verifier corrections incorporated:**
- The broken `fleetd up` string lives in **two** places, not one: `board/src/App.jsx:307` **and** `board/src/components/LanPanel.jsx:104`. Both must be patched.
- The board is not literally CTA-less — the header's `+ Spawn` button (Header.jsx:82-84) shows on an empty board — but the empty-state component itself has none. Fix the component anyway.
- HelpOverlay already explains two nouns (mail vs orchestrator, turn boundary) at HelpOverlay.jsx:54-58; extend rather than duplicate. BOARD_ACTIONS has 13 entries, not 14.

**Fix (options 1 + 2 + 3 together):**
- `board/src/App.jsx` + `board/src/components/LanPanel.jsx` — replace the nonexistent command with the accurate path ("start `claude` in any terminal on this machine — the hooks report in"; standalone-CLI users: `fleetdeck serve`). Add a primary "Spawn your first agent" button (gated on the existing `spawnAvailable` check so FLEETDECK_SPAWN=off never offers a 403ing button) and a secondary "What is this?" link opening HelpOverlay.
- `board/src/helpText.js` — add a CONCEPTS section (six nouns, one line each) and a Glossary (turn boundary, whisper, callsign, arm, rail, capture & release, worktree). helpText.js stays the single source of truth per its header contract.
- `board/src/components/HelpOverlay.jsx` + `board/src/App.jsx` — render the concept section; auto-open the overlay once on first load via a new `fd-help-seen` localStorage flag, wired through App.jsx's modal coordination (must not fire before TokenGate or stack over Compose/Spawn).
- `board/src/components/PlanLibrary.jsx` — rewrite the empty state to name the creation path ("spawn with permission-mode: plan, then Approve or Capture its exit-plan question in NEEDS YOU"); default the strip open when plans exist and the user never toggled `fd-plans-open` (board side only — respect the "rail is sacred" design comment at PlanLibrary.jsx:6-10).
- `board/src/components/SpawnForm.jsx` — one-line hint under the permission-mode select connecting `plan` to the PLANS library.
- `board/src/components/BoardLanes.jsx` + `board/src/components/Compose.jsx` — `title=` tooltips on lane headers and the "next turn boundary" footer (tooltips are a bonus only; full definitions live in the overlay glossary for touch devices).

**Acceptance checks.** Fresh profile auto-shows concept help exactly once; `fd-help-seen` suppresses on reload; empty board shows a command verified against package.json bin plus a working CTA (hidden with spawn off); PLANS empty state names the full creation path and auto-opens when populated; every jargon term in UI copy resolves to the glossary or a tooltip.

**Risks.** Modal stacking on first run (wire in App.jsx, not HelpOverlay); copy drift if concepts are added outside helpText.js (reviewers enforce the convention).

### 1.2 spawn-branch — explain the inert Spawn button

**Root cause.** The Spawn button is disabled whenever `targetReady` is false (repo mode requires repo + branch + no validation errors, SpawnForm.jsx:482-484) with no reason on the button itself, and stays disabled with no busy indicator while a spawn POST is in flight (local mode holds the request open through a 120s fetch).

**Verifier correction incorporated.** The diagnosis overstated the silence: `branchErr`/`repoOrgErr` **do** render inline as red `fd-spawnerr` text next to the fields (SpawnForm.jsx:630-631, 697-700). The genuinely silent cases are narrower: empty required fields, and busy-with-no-spinner. The fix targets the button itself.

**Fix (option 3).** `board/src/components/SpawnForm.jsx` — render the blocking reason adjacent to the button ("enter repo + branch", the field error, missing default org) and a "working…" busy label while the POST is in flight.

**Acceptance checks.** Disabled button reveals exactly which field blocks it; in-flight spawn shows a busy label.

**Risks.** None material. (The larger 202-closes-into-silence problem is Phase 2.3.)

### 1.3 term-close — per-tile ✕ on the Terminals wall (view-only detach)

**Root cause.** TermGrid tiles have no close affordance — the single header ✕ closes the entire wall (App.jsx:580) — and the grid is a static array from `openGrid()` with no removeTile path. All daemon-side teardown already exists: unmounting a TermPane closes its /ws/term socket and termbridge reaps the viewer (termbridge.mjs:~405-425, close at 546-548), releasing the shared tmux control client when the last viewer leaves.

**Verifier corrections incorporated.** Line refs for the viewer object were a few lines off (substance confirmed), and the focus re-aim is an improvement rather than a strict requirement — `cycle()` (TermGrid.jsx:41-45) already falls back to index 0 on a stale focused id — but we do it explicitly anyway since it is cheap and deterministic.

**Fix (option 1, view-only):**
- `board/src/hooks/useTermWindows.js` — add `closeGridTile(identity)` filtering the tile out of `grid`, nulling the grid when the last tile closes, and removing the session from the `watch` Set so a watch-seeded grid doesn't resurrect the tile.
- `board/src/components/TermGrid.jsx` — per-tile ✕ ghost button (aria-label `Close <callsign>`, stopPropagation like the ⤢ at line 94), re-aim `focused` to the first remaining tile, prune the removed tile's `notes` entry.
- `board/src/App.jsx` — pass `onRemoveTile={closeGridTile}`.

**Hard kill stays exclusively behind the existing KillConfirm door** (App.jsx:498 — "the ONLY door to killSpawn"). No new daemon endpoint, no kill-from-tile shortcut.

**Acceptance checks.** ✕ on every tile with ≥3 agents; removing a tile leaves others streaming untouched (no re-seed flash); focused-tile removal moves ⌨ focus; last tile closes the wall; the tmux pane stays alive (tmux list-panes); closing the last viewer releases the `tmux -C attach-session` process (verify with ps); reopening re-lists all agents; no console errors on survivors.

**Risks.** Detached tmux windows keep `window-size manual` at tile geometry (pre-existing for any viewer close — note in commit message). The view-only design is deliberately immune to the flaky %window-close probe (BUG-055/BUG-157) that a kill-based design would hit.

### 1.4 plans-tab — honest Execute copy (fast half of the fix)

**Root cause (copy half).** "Execute" spawns a **new** worker session (App.jsx:198-220) with no indication of that, even when the proposing session is live and possibly already executing — a duplicate-worker footgun.

**Fix (option 2).** `board/src/components/PlanLibrary.jsx` — relabel to "Spawn executor…", add a hint line ("starts a NEW session with this plan as its prompt"), and when `p.session_id` is in liveSessions annotate "proposed by `<callsign>` — still live". Plus the matching rail-hint copy at line 177.

**Acceptance checks.** Button copy visibly states a new session is spawned; live-proposer annotation appears.

**Risks.** Cosmetic only — the stale-'proposed' status defect is Phase 2.2; shipping copy first is intentional (stops the immediate surprise).

---

## Phase 2 — Async feedback loops (M effort, ships after Phase 1)

Theme: three places where real work happens off-screen and the board never tells the user the outcome.

### 2.1 needs-you-ttl — survivable question window: focus-terminal + re-arm, then TTL as a real setting

**Root cause.** Every hold-kind question is hard-capped at 50s (DEFAULT_HOLD_MS, questions.mjs:68) and also settles early on activity (correlated PostToolUse, session-wide UserPromptSubmit — the latter expires freeform rows too, per the verifier). On expiry the hook fails open with `{}`, the native terminal prompt owns the decision, and the board card goes dead with no recovery path.

**Verifier corrections incorporated:**
- The 60s env clamp is not an independent escape hatch — anything above ~62s races the shim's 62.6s abort/63s watchdog. Raising the ceiling **requires** the lockstep bump of hooks.json timeouts + fleet-hook.mjs WATCHDOG_MS; it is mandatory, not optional. Given the headroom math (default is only ~12.6s under the real ceiling), the re-arm mechanism and focus-terminal are the substantive fixes, not the default bump.
- Raising hooks.json timeouts has a real cost (terminal prompt blocks for the full timeout when the daemon is down) — that is why the defaults sit at 50/63/65s, and why we default conservatively.

**Fix, in two steps:**
1. **Option 4 (S, ship first): focus terminal from expired cards.** `board/src/components/Inbox.jsx`, `board/src/components/TermWindow.jsx`, and a focus endpoint in `scripts/fleetd/http.mjs` if absent — expired cards get a one-click action that selects/highlights the owning session's pane where the native prompt waits. Zero protocol risk; directly answers "need to find the terminal."
2. **Option 2 + 3 (M): re-arm with mail delivery.** `scripts/fleetd/questions.mjs`, `scripts/fleetd/events.mjs`, `board/src/components/Inbox.jsx` — on expiry, if the session shows no activity for N seconds (agent still parked on the native prompt), emit a fresh NEEDS YOU card (capped at K re-arms, stopped by any activity). Its answer routes through the existing `fleetdeck-answer` mail pipeline (the option-3 mechanism) rather than the impossible re-park of the returned hook socket. Card copy must say "sent as a message — delivered at the next turn boundary" and never claim it unblocks an agent parked on stdin.
3. **Option 1 (M): TTL as a first-class setting.** Default holdMs to ~90–120s with the lockstep raise of hooks.json (44/55/66) and fleet-hook.mjs:29; expose in `scripts/fleetd/settings.mjs` and a board Settings surface; ship an install/migration path for the per-user hooks.json (stale installs still killing at 65s means answers land on dead sockets silently).

**Acceptance checks.** Held PermissionRequest answered at T+90s returns 200 and reaches the agent; integration test answering at TTL−1s proves watchdog margin; correlated PostToolUse retires only its own hold and UserPromptSubmit still session-wide-expires (tests/needs-you.test.mjs, choice-relay.test.mjs pass); expired card's focus-terminal works; re-armed card appears (capped) and its answer flows through the mail pipeline; nothing is ever silently auto-answered; freeform questions still don't expire at SessionEnd.

**Risks.** Per-user hooks.json migration is the sharp edge; the elicitation response schema is unverified (questions.mjs:19-23) and longer holds widen exposure; re-arm needs the activity stop-condition + cap to avoid nag-loops; expireOnActivity keys on (tool_name, tool_input) only — BUG-138's twin-hold issue means re-arm must not resurrect a hold whose tool completed; MAX_HOLDS_PER_SESSION=4 eviction fail-opens the oldest hold more often under longer TTLs.

### 2.2 plans-tab — reconcile in-terminal approval with plan state

**Root cause.** Plan capture is unconditional (events.mjs:531-579), but the `proposed → approved/captured/rejected` transition lives only in the board-answer path (planAnswered called solely from questions.mjs:297). If the user approves in the terminal, the question row is retired (turn-boundary expireOnActivity) while the plan row stays `proposed` forever, still offering Execute/Assign — the two views visibly diverge.

**Verifier corrections incorporated:** The stale artifact is specifically the PLAN row (the question card does disappear from the rail) — which strengthens the "two parallel models" point. Critically, **do not rely on adding ExitPlanMode to the PostToolUse matcher**: ExitPlanMode's approval flow may never emit PostToolUse. The reliable observation path is session-activity-based settling; validate live before trusting any hook-matcher correlation.

**Fix (option 1, activity-based variant):**
- `scripts/fleetd/questions.mjs` + `scripts/fleetd/events.mjs` — when an ExitPlanMode-linked question is retired by settleExpired/expireOnActivity **without** a board answer **and** the session subsequently shows activity, settle the linked `proposed` plan to a new terminal status `handled-in-terminal` (excluded from EXECUTABLE in PlanLibrary.jsx:16).
- `scripts/fleetd/plans.mjs` + `scripts/fleetd/db.mjs` + `board/src/components/PlanLibrary.jsx` — extend the CONTRACT transition matrix, status enum comment, fd-pstatus CSS, and board rendering for the new status.
- Never settle on expiry alone: a planner killed mid-hold must not be marked executed.
- `tests/plans.test.mjs` — new case for the in-terminal approval path; existing capture/approve/deny cases must pass.
- Fold in open bugbash items sharing this transition matrix: **BUG-040** (mark-before-spawn atomicity) and **BUG-041** (execute should retire the planner hold).
- README gains the full lifecycle section including in-terminal approval behavior (option 4, folded in here rather than standalone).

**Acceptance checks.** In-terminal approval flips the plan out of `proposed` within one snapshot refresh and removes Execute/Assign; board approval still flips to `approved` (regression); expiry with no activity never marks executed; concurrent-settlement 409 renders via the existing markErr path; README documents the lifecycle.

**Risks.** A new status touches daemon, board, tests, and demo/run-accept-plan.sh; session-wide activity is a coarse signal that could mis-settle a plan the user denied — the no-activity-without-subsequent-activity gate and BUG-040/041 coordination mitigate; partial fixes could mask BUG-112.

### 2.3 spawn-branch — keep the user informed through a 202 provisioning spawn

**Root cause.** For a repo that isn't local, /api/spawn answers 202 and the real work (clone up to 600s, fetch, worktree add) runs detached (spawns.mjs:869-912). The form shows "cloning" for exactly 1400ms then auto-closes (SpawnForm.jsx:434); failure lands only as a tombstoned OFFLINE card with the actionable git stderr (SSH key, settings URL) behind a per-card expander.

**Verifier correction incorporated.** "Zero on-screen state" is overstated: the failure does persistently reach the board as the tombstone card plus a line in the persistent ticker Feed — what's missing is prominence, correlation to the closed form, and the fail_detail. **Repro first** (the trigger — auth-less private clone, dead button, or a 500 — is inferred, not confirmed): POST /api/spawn with an unreachable private repo and observe the 202 → close → tombstone path.

**Fix (options 1 + 2 + 4):**
- **Option 1 (M).** `board/src/components/SpawnForm.jsx` + `board/src/App.jsx` — on 202, don't close at 1400ms; watch the returned session_id in the existing snapshot stream: flip the note live on success, setErr with the tombstone note + fail_detail on failure; timeout + manual close as escape hatches so a missed frame can't wedge the modal.
- **Option 2 (M).** `board/src/App.jsx` + `board/src/hooks/useFeedbackStrip.js` + `board/src/useFleetState.js` — board-level banner when a card transitions to offline with a `spawn failed:` note, deduped by session_id, fail_detail one click away. Covers spawn failures from any source, not just this form.
- **Option 4 (S).** `scripts/fleetd/spawns.mjs` + `scripts/fleetd/http.mjs` — guard the repo-mode card-creation block (createSpawnedCard's synchronous branchOf at spawns.mjs:240 is the cited speculative case) and return bounded real 500 reasons instead of bare `internal` (the /api/spawn catch is specifically http.mjs:936).

**Acceptance checks.** Failed-clone spawn surfaces the distilled git fatal line without opening the offline card's expander; successful 202 closes the form as the card goes live; any unguarded repo-mode throw returns a bounded reason, never `internal`; redaction tests (tests/spawn-repo.test.mjs:344-380, tests/git-stderr-detail.test.mjs) pass and extend to the new banner/form surfaces.

**Risks.** Modal lifetime coupled to ws/poll delivery (timeout + manual close required); banner spam without per-session dedup; fail_detail carries redacted git stderr — widening its audience means the redaction tests must cover every new surface; there are no board-side tests, so the 202-close regression class stays untestable in CI (manual checklist for now).

---

## Cross-cutting interactions

- **questions.mjs is touched by 2.1 and 2.2 simultaneously** — re-arm changes settleExpired's aftermath; plan reconciliation hooks the same expiry paths. Land 2.2's plan-settle observer first (it consumes expiry events), then 2.1's re-arm (which creates new rows from them), with a shared test pass over tests/needs-you.test.mjs + tests/plans.test.mjs.
- **Inbox.jsx is touched by 1.2-adjacent copy, 2.1 (focus terminal, re-arm cards), and the plans question card** — batch the board edits per file to avoid rebasing churn.
- **SpawnForm.jsx is touched in 1.1 (permission-mode hint), 1.2 (button states), and 2.3 (202 watch)** — same batching advice.
- **2.1's TTL setting touches settings.mjs, hooks.json, and fleet-hook.mjs in lockstep**, plus a per-user install migration — the only item with a user-machine migration burden.
- **Every board change requires rebuilding board-dist** (scripts/fleetd/board-dist is the served, checked-in artifact), and BUG-083 means boards open across a daemon upgrade can't lazy-load chunks — all manual verification needs a fresh board load.
- Plans work overlaps open bugbash items BUG-040/041/112; term-close deliberately routes around BUG-055/157.

## Not doing / explicitly out of scope

- **Guided first-run tour** (onboarding option 5, L): high build/maintenance cost in a hand-rolled React app; revisit only if feedback persists after Phase 1.
- **Board-driven pane injection to cancel the native prompt** (needs-you option 5, L): the only true unblock for a stdin-parked agent, but fragile (prompt-state parsing, wrong-prompt injection risk). Deferred to a later milestone after 2.1 lands and real usage data accumulates.
- **Kill-from-tile** (term-close options 2/3): solves a different problem than the feedback (decluttering, not terminating) and raises misclick stakes; kill stays behind the single KillConfirm door.
- **Suppressing Execute when the owner is live** as a standalone fix (plans option 3): heuristic that can block legitimate re-execution; folded into 2.2's status reconciliation instead.
- **Hooks-matcher-based ExitPlanMode detection**: verifier-flagged as unreliable; replaced by activity-based settling.
- **Board-side test harness**: noted as a gap (spawn form untestable in CI); not built in this plan — flagged for a future tooling milestone.

## Suggested test/validation pass

1. `npm test` (node:test suite) — must stay green throughout; extend in-place:
   - `tests/plans.test.mjs` — in-terminal-approval settlement case (2.2).
   - `tests/needs-you.test.mjs`, `tests/choice-relay.test.mjs` — regression on correlated/session-wide expiry under the new TTL + re-arm (2.1); new integration test answering at TTL−1s.
   - `tests/spawn-repo.test.mjs`, `tests/git-stderr-detail.test.mjs` — extend redaction coverage to the new spawn-failure surfaces (2.3).
   - `tests/gateway.test.mjs` — regression for the /api/spawn 500-reason guard (2.3).
2. Live validation before Phase 2 merges: repro the 202-spawn silence with an auth-less private repo; verify whether ExitPlanMode emits any PostToolUse (settles the 2.2 observation-path question empirically).
3. Manual board checklist per phase (fresh board load, rebuilt board-dist): first-run help fires once; empty-state CTA works with spawn on and hides with `FLEETDECK_SPAWN=off`; tile close leaves survivors streaming and releases the tmux control client (`ps` before/after); expired card's focus-terminal highlights the right pane; re-armed card appears and caps out; failed clone announces via banner without expander interaction.
4. hooks.json migration dry-run on a stale install: confirm holds still fail open safely (never silently lost answers) when an old 65s timeout meets a new 120s hold.
