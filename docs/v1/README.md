# Fleet Deck v1.0 — the plan of record

*The command deck: from today's coordination membrane to a multi-agent, multi-provider dev fleet.*

*Written 2026-08-07 against v0.22.4. This folder is the **plan of record** for v1.0 — the [vision](../fleetdeck-v1.0.md) as amended by the [combined review](../fleetdeck-v1.0-review.md). Where the two disagree, this folder follows the review: every claim the review corrected (base ref "already recorded" → not recorded; Codex hooks "same vocabulary" → shell-only and opt-in; "derive.mjs becomes provider-aware" → normalize at intake; and the privilege gap) has been folded in here as the truth we build from. The vision and review stay in `docs/` as the evidence trail; these are the working specs.*

---

## How to read this folder

Each child doc is a **PRD + tech spec** for one slice of the release. PRD = the problem, who feels it, and the user-facing outcome. Tech spec = the mechanism, the data-model changes, the current-code anchors (`file:line`), and the doctrine/validation checks. Read in this order:

| # | Doc | What it covers | Kind |
|---|-----|----------------|------|
| — | **[README](./README.md)** (this file) | North star, doctrine, the map, sequencing, definition of done | Index |
| — | **[architecture](./architecture.md)** | The provider seam, canonical events, the strategy object, the DB/runtime seam, migrations — the backbone every pillar rides | Tech spec |
| F | **[foundations](./foundations.md)** | F1 TypeScript (contracts-first) and F2 Bun single binary | PRD + spec |
| F | **[ts-migration](./ts-migration.md)** | Companion to F1 — the progressive, file-by-file `.mjs`→`.ts` playbook (recipe, order, restructure) | Tech spec |
| F | **[foundations-hardening](./foundations-hardening.md)** | The Bun-primary spine — Biome, `bun:sqlite`/Kysely, `Bun.serve` + native WebSocket, Zod, Effect v4 — as a phase **after** the TS migration and **before** the pillars. **Supersedes F2** (Bun becomes the runtime, not an optional binary). | PRD + spec |
| P1 | **[p1-cc-provider](./p1-cc-provider.md)** | Claude Code as a first-class provider — the observe floor, the reference card, and the Layer-4 strategy the drive tier extends | PRD + spec |
| P1 | **[p1-codex-provider](./p1-codex-provider.md)** | Codex as a first-class *observed* provider — the Codex floor | PRD + spec |
| P2 | **[p2-harvest-surface](./p2-harvest-surface.md)** | Diff view, per-turn checkpoints, notes → one batched mail | PRD + spec |
| P3 | **[p3-issue-pr-spawning](./p3-issue-pr-spawning.md)** | Issue-driven fan-out, point-at-a-PR review, PR-scoped write | PRD + spec |
| P4 | **[p4-usage-accounts](./p4-usage-accounts.md)** | Usage & rate-limit meters, multi-account pinning | PRD + spec |
| P5 | **[p5-programmable-fleet](./p5-programmable-fleet.md)** | Waitable completions, the privilege model, the control skill | PRD + spec |
| P6 | **[p6-unified-views](./p6-unified-views.md)** | Terminal grid, the Slack-style stream, optional chat, permission ladder | PRD + spec |
| P7 | **[drive + observe](./p7-drive-and-observe.md)** | Driving Claude (Agent SDK) & Codex (app-server) while still observing via their hooks; observe-only demoted to the fail-open floor. Amends rule 1 & 5 | PRD + spec |
| — | **[validation-and-gates](./validation-and-gates.md)** | Validation proofs, migration story, test strategy, perf bars, platform statement, the security gate | Program spec |
| ★ | **[design-spec](./design-spec.md)** | The capstone — v1.0 as a finished system + the complete v0.22.4→v1.0 change manifest | Design spec |
| ★ | **[ui-spec](./ui-spec.md)** | Companion to the capstone — every board surface, control, state, modal, and keyboard path at 1.0 | UI spec |

If you read only two, read this index and **[architecture](./architecture.md)** — the seam decision there is the one that makes or breaks the rest. If you want the whole destination and the full diff in one place, read the capstone **[design-spec](./design-spec.md)**.

---

## North star — what "1.0" means

Today Fleet Deck is a **coordination membrane for Claude Code sessions**: it observes plain `claude` through hooks and puts every session on one board. No wrapper, fail-open, a deterministic core that makes zero model calls, loopback by default, a plugin and not an app.

v1.0 graduates it into **the command deck for a multi-agent, multi-provider dev fleet**:

- **more than one harness** — Claude *and* Codex, both honestly observed;
- **work that starts from issues and ends in reviewed PRs**;
- a real **diff-and-revert review surface** with per-turn checkpoints;
- **operational awareness** of usage, rate-limits, and accounts;
- a fleet a coordinator session can **drive** — with a privilege model, not just a checkbox.

**The bet of 1.0:** we can widen the scope this far and *still be a membrane*. Everything in this folder is filtered through that. When a feature can't be built without becoming a client, a wrapper, or a phone-home, it is downscoped or deferred — and the doc says so out loud.

---

## The doctrine, evolved

The five rules hold; the scope they cover grows.

1. **Claude-first → provider-pluggable; drive by default, observe as the floor.** We *drive* agents through their native protocol — Claude via the Agent SDK, Codex via `app-server` — **and keep observing them** through their own config-resident hooks, in one session. The hooks still fire, so every card is still pure derivation and the observe thesis holds; what changes is the *default posture* — from watching the tools you run to driving them, still wearing the membrane. Observe-only isn't removed: it drops to the **fail-open floor** a session falls back to when the drive path is unavailable, and the compatibility lane for any runtime with no drive protocol. Codex reaches drive-default after Claude (its hooks stabilize later). This is a substantive change from the original "observe, never drive" rule — see **[P7 — drive + observe](./p7-drive-and-observe.md)**.
2. **No model calls in the core — preserved.** Even "review this PR with my security skill" means the daemon *spawns an agent* that does the review. The agent is the intelligence; the daemon stays arithmetic, SQL, and git.
3. **Read-only forge → PR-scoped write, via the user's own CLIs.** We relax the forge stance *just far enough* for the PR workflow, authenticated by the user's own `gh`/`glab`, never a hosted service, never a stored token on the board. Write is a **verb allowlist** with a per-write human confirm and an audit line — or it drops back to read-only rather than slip the release ([P3](./p3-issue-pr-spawning.md)).
4. **Loopback / no phone-home — preserved.** The chat surface and the activity stream are served by the local board. Remote stays claude.ai-handoff + Tailscale/Coder.
5. **Terminal-authoritative; plugin-vs-app is a post-V1 call.** The terminal stays the primary, authoritative surface — but for a *driven* session that surface is a **live Fleet-Deck-rendered pane of the driven stream** (plans, diffs, the gate inline), not a scraped vendor TUI. A chat view is still a board page. Whether the result ships as a plugin or an app is settled as the **last step after V1 lands**, not now; plugin distribution looks very much still viable (see **[P7](./p7-drive-and-observe.md)**).

**Doctrine, sharpened by the review.** Two things the original vision left implicit and 1.0 must make explicit:

- **A privilege model, not just the human "asks-twice" gate.** On default config, loopback callers can already reach `POST /api/spawn` tokenless, and a spawn body can carry `setup_cmd` (runs `sh -c` before the agent starts). Fine as a same-UID trust-zone residual *today* — **not** fine once P5 makes agents first-class API drivers and P3 pipes third-party issue text into prompts. 1.0 adds token classes and spawn caps ([P5](./p5-programmable-fleet.md)).
- **An injection boundary.** Issue and PR bodies are third-party text. Issue-derived spawns default to *supervised*, and forge text is delivered fenced-and-labeled as untrusted data — never as the raw prompt ([P3](./p3-issue-pr-spawning.md)).

---

## The map — two foundations, seven pillars

| ID | Name | 1.0 commitment | Where the risk is | Cuttable? |
|----|------|----------------|-------------------|-----------|
| **F1** | TypeScript, contracts-first | **F1a** contracts module (timeboxed) + **F1b** standing rule: new code TS, convert modules only when a pillar touches them | No `tsc` in repo today; esbuild strips types without checking → CI must add `tsc --noEmit` + runtime boundary validation | F1a no; F1b is a rule |
| **F2** | Bun single binary + brew | Standalone board server as a compiled binary, *additive* beside Node | Not sqlite (well centralized) — it's `mdns`/dgram, `ws`, tmux control pipes under Bun | **Yes — explicitly** |
| **P1** | Provider layer — Claude floor + Codex floor | Claude reference card extracted behind the strategy object (no behavior change); Codex Tier A floor card (turn-level + shell telemetry), spawn/kill/worktree, checkpoints, usage burn | Codex hooks are **experimental, opt-in, shell-tool-only** → Tier C file-chips are an honest floor gap (the drive tier, P7, softens it via `turn/diff`) | Codex Tier C floor-gap yes; floors non-negotiable |
| **P2** | Harvest surface | Base-ref recording, diff renderer, async per-turn checkpoints, notes → one batched mail | Base ref **is not recorded today** — must land first; checkpoints must run off the hook path | Core no |
| **P3** | Issue/PR spawning | Issue → parallel agents, point-at-PR review, PR-scoped write (GitHub + GitLab) | Prompt-injection chain; forge writes are new surface | Jira/Linear cut; write→read-only if security slips |
| **P4** | Usage & accounts | Claude usage meter; Codex burn with first-class "unknown" | Codex `rate_limits: null` upstream; account pinning needs a config-home refactor | Account **pinning** is a stretch |
| **P5** | Programmable fleet | Waitable completions (T0.1), privilege model, daemon-served skill (T0.2) | Privilege gap is a P0 security item | Skill polish yes; privilege no |
| **P6** | Unified views | Slack-style stream (new event subsystem), permission-ladder consolidation | Stream is a real subsystem; chat has no in-progress-turn source today | Chat → post-1.0 if needed |
| **P7** | Drive + observe | `claudeSdk` drive-default (answerable approvals, interrupt, steer, resume, runner-in-a-pane); observe-only kept as fail-open floor; Codex drive tier staged after Claude | Amends rules 1 & 5; rests on hooks-firing-under-SDK (the linchpin); most "app"-shaped surface in 1.0 | Floor non-negotiable (fail-open); Codex drive tier stageable |

A **cross-cutting tidy** rides P6: consolidate the two overlapping permission controls (the four-mode dropdown *and* the separately-armed unsupervised checkbox already both exist) into one legible **four-mode ladder** — Supervised / Auto-accept-edits / Auto / Full-access — mapped onto Claude's `--permission-mode` and Codex's approval×sandbox grid.

---

## Sequencing to 1.0 (revised)

> **Amendment (2026-08-09):** a **[Foundations-Hardening](./foundations-hardening.md)** phase now sits **between the TS migration and step 1 below** — Bun-primary runtime, Biome, `bun:sqlite`/Kysely, `Bun.serve` + native WebSocket, Zod, Effect v4. It **supersedes F2** (formerly step 7, "additive & cuttable"): Bun is now the runtime, proven behind a go/no-go spike, not an optional binary. The pillar order below is unchanged, but F2's old row and the F1/F2 validation proofs are now propagation debt tracked in [foundations-hardening §8](./foundations-hardening.md#8-doctrine-check--and-the-propagation-debt).

The vision's original order (F1 → P1+P4 → P2 → …) fronted a migration with no user-visible value, gated everything on the pillar with the most *external* risk (experimental Codex hooks), and demoted T0.1 completions to fifth after `fleetdeck-future.md` ranked them **first**. The plan of record adopts the review's revised order:

1. **Base-ref recording** (~1 day — data starts accruing immediately) **+ F1a contracts module** (timeboxed, includes the canonical event vocabulary). *F1a and the P1 intake-normalization are the same task — sequence them as one.* **+ the [P7](./p7-drive-and-observe.md) linchpin proof** — confirm Fleet Deck's existing `http` hooks fire from an SDK-driven `query()`. The whole drive default rests on it, so it's proven first and cheaply, off the critical path.
2. **P2 harvest** (diff route → renderer → async checkpoints → batched notes) **+ P5 T0.1 completions** in parallel — both pure git/SQLite/UI, zero external risk, felt daily.
3. **P1 provider floors** (intake normalization + Claude strategy extraction + Codex Tier A floor) **+ P7 `claudeSdk` drive-default** layered on the Claude floor (answerable approvals, interrupt, steer, resume, runner-in-a-pane) **+ P4 Claude usage meter** alongside.
4. **P3 issue/PR** (land `fd/git-auth` first; GitHub → GitLab; injection-hardened; PR-write allowlist) **+ P4 Codex usage**.
5. **P5 completion**: privilege model (token classes + spawn caps) + T0.2 daemon-served skill.
6. **P6**: stream (new event subsystem) → chat composer → permission-ladder consolidation **+ P7 `codexAppServer` drive tier** once Codex's hooks stabilize (its floor already shipped in step 3).
7. **F2 Bun binary + brew** — explicitly cuttable ("1.0 ships without brew if compat drags").
8. **Security-review gate** (delta audit of the new surface: forge writes, control API, multi-account env, **the drive-control surface**), then **cut v1.0**.

**Why this order:** it puts the highest-value, lowest-risk work (harvest, completions) first and behind nothing; it proves the P7 linchpin cheaply and up front so the drive default rests on a confirmed fact rather than an assumption; it ships Claude's drive tier early while staging Codex's behind its stabilizing hooks (the floor carries Codex until then); and it restores consistency with `fleetdeck-future.md`.

---

## Definition of done

Fleet Deck 1.0 is:

- **multi-provider** — Claude + Codex, both *honestly* observed (a Codex card is reduced-but-derived and labeled as such, never claiming parity);
- **issue-to-parallel-agents** and **point-at-a-PR review**, using the user's own skills and credentials;
- a real **diff-and-revert review deck** with per-turn checkpoints;
- **operational awareness** of usage, rate-limits, and accounts, with a first-class "unknown — never guess" state;
- an **agent-drivable fleet** with waitable completions **and a privilege model** (token classes + spawn caps);
- a **Slack-style stream** over the terminal grid with an optional, explicitly-secondary chat surface;

— and all of it still a **fail-open, loopback, deterministic-core plugin**, optionally installable as a single **brew binary** for the standalone board.

**Plus the gates the vision omitted** (see [validation-and-gates](./validation-and-gates.md)): per-pillar validation proofs, a numbered/transactional migration story with `PRAGMA user_version`, a named per-pillar test strategy, performance bars for 15 sessions × per-turn checkpoints, an honest platform statement (Codex hooks Windows-disabled; macOS CI advisory), and a security-review delta gate before the cut.

---

## Deferred to post-1.0

- **Design Mode** — liked, but needs an embedded browser bridge we keep out of the core. Post-1.0 as an optional out-of-core helper reusing the screenshot-upload path.
- **Driving agents via SDK / app-server (the "mediate" model)** — **no longer deferred.** Earlier drafts said "stays out"; it is now the **default path** — drive+observe is how the fleet runs agents, with observe-only as the fail-open floor — and it is the substance of the amended rule 1 above. See **[P7 — drive + observe](./p7-drive-and-observe.md)**.
- **Hosted relay / native mobile apps** — rejected on doctrine (loopback).
- **Remote spawn (agent on another host)** — Coder/LAN covers the need; revisit if insufficient.
- **Issue trackers beyond forges (Jira/Linear)** — a Jira token is a stored credential; either it rides the exact gateway-token discipline or it waits. **1.0 is GitHub + GitLab only.**
- **LLM-in-product niceties** (auto PR/thread titles) — deterministic naming stays the default; any model-written text is opt-in, never core.
- **Full chat with in-progress-turn rendering** — needs a live transcript tailer that doesn't exist; 1.0 ships at most a turn-level thread, or demotes chat entirely.

---

## Provenance

This folder synthesizes, in order of authority:

1. [`fleetdeck-v1.0-review.md`](../fleetdeck-v1.0-review.md) — the combined review (Fable deep review + adversarial Codex pass + architecture deep dive; every `file:line` claim verified in source). **The corrections here win.**
2. [`fleetdeck-v1.0.md`](../fleetdeck-v1.0.md) — the original vision. Structure and framing.
3. [`fleetdeck-future.md`](../fleetdeck-future.md) — the roadmap the vision was built from (Tier/A-item IDs).
4. [`orca-lessons.md`](../orca-lessons.md), [`t3code-lessons.md`](../t3code-lessons.md) — the competitive evidence.
