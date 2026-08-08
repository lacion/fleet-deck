# Fleet Deck v1.0 plan — combined review

*Written 2026-08-07 against v0.22.3 and the actual tree. Reviews [fleetdeck-v1.0.md](./fleetdeck-v1.0.md). Three inputs: a Fable deep review, an adversarial Codex pass, and an architecture deep dive (every claim below that cites `file:line` was verified in source). Companion evidence: [orca-lessons.md](./orca-lessons.md), [t3code-lessons.md](./t3code-lessons.md), [fleetdeck-future.md](./fleetdeck-future.md).*

## Verdict

The vision is right and the doctrine discipline is real — observe-not-mediate is the correct corner, the pillars are the correct six, and nothing in the plan quietly breaks fail-open/loopback/no-model-calls. But the document is **calibrated optimistically in exactly the places that will bite**:

1. **P1's central premise is overstated.** The Codex hooks engine is *not* "the same lifecycle vocabulary" — it's opt-in behind a feature flag, shell-tool-only for tool telemetry, and blind to file edits. A Codex card will be honest but **coarse** (turn-level, not tool-level), and several of Fleet Deck's marquee features (conflict radar, file chips, mail-by-Stop-block) have no Codex mechanism at all.
2. **P2's foundation claim is false.** The "recorded start-from ref" the diff view builds on **is not recorded anywhere** — no schema column, and cwd-mode worktrees branch from HEAD without noting the ref. Small fix, but it must land *first* so data accrues.
3. **P1's proposed seam is in the wrong place.** "`derive.mjs` becomes provider-aware" mislocates the work: the transition switch lives in `events.mjs`, and Claude-coupling smears across ≥6 modules and two side-channels (`claude agents --json`, transcript JSONL). The right seam is **canonical-event normalization at hook intake** plus a per-provider strategy object.
4. **P3 + P5 together create a prompt-injection → unsupervised-spawn chain** the doc never mentions. Issue bodies are third-party text; the control API is one flat bearer token that can mint an unsupervised arm server-side. v1.0 needs a privilege model, not just the human asks-twice gate.
5. The sequencing buries the highest-value, lowest-risk work (P2 harvest, T0.1 completions) behind the highest-external-risk work (Codex), and contradicts fleetdeck-future.md's own "ship T0.1 first" without explanation.

None of this sinks the plan. All of it should be folded in before F1 starts.

---

## Fact-check: document claims vs the tree

Verified in source; ✅ = claim holds, ⚠️ = holds with correction, ❌ = wrong.

| # | Doc claim | Reality |
|---|---|---|
| 1 | ❌ F1: daemon is "~200 KB of plain `.mjs`" | **18,250 lines across 35 modules** (~1 MB source) + a committed 630 KB bundle (`package.json:44`). Zero TS, zero `@typedef` payload shapes; the board hand-mirrors the `/state` snapshot in `board/src/useFleetState.js:25-41` with contract rules living in comments. F1's *motivation* is thus stronger than claimed; its *size* is ~5× understated. |
| 2 | ❌ P2/A2: diff "against the spawn's recorded start-from ref (we already record it)" | **No base ref/SHA is recorded.** `spawns` table has `origin_url/requested_branch/branch_mode` but no base column (`db.mjs:154-185`); cwd-mode `git worktree add -b` passes no start ref (`spawns.mjs:1086`). `worktrees.mjs:145-225` computes a base (origin/HEAD) at *inspection* time — a fallback, not a record. |
| 3 | ⚠️ P2: diff "rendered in the existing FileViewer" | FileViewer is a plain-text tree viewer: no diff mode, **no syntax highlighting, no image rendering**, 5,000-line DOM cap (`FileViewer.jsx:150-184`). A diff surface is a new renderer (hunks, anchors for notes), even if it reuses the modal shell. |
| 4 | ⚠️ P2: "on Stop, tag a cheap git checkpoint" | The daemon deliberately runs **zero git per turn** today (LRU-cached identity, one fresh exec "per session lifetime", `events.mjs:258-271`) and the Stop hook has a 5 s timeout (`hooks/hooks.json:97-102`). Checkpoints must run **async off the hook-response path**, debounced, fleet-owned-worktrees-only. |
| 5 | ❌ P1: Codex hooks = "the same lifecycle vocabulary Claude Code exposes" | The engine is experimental **and off by default** (`[features].codex_hooks = true` in `config.toml`); `PreToolUse` fires for the **shell tool only** — `apply_patch`/edits/MCP calls emit nothing; Windows support disabled. No FileChanged/CwdChanged equivalents. See "External premises" below. |
| 6 | ⚠️ P1: "spawning `codex` instead of `claude` is a small step" | The argv/env layer generalizes (generic env map → tmux `-e`, `spawns.mjs:556-578`). But: owned-pane mail requires `pane_current_command === 'claude'` (`mail.mjs:358,402`), `/rc` likewise (`spawns.mjs:1738`), the bring-up nudge parses Claude's trust-dialog copy (`spawns.mjs:319`), revive is `claude --resume`, liveness rides `claude agents --json` (`agents-poll.mjs:38`), and `/clear` succession is Claude-semantic (`derive.mjs:503-715`). "First-class" Codex is a matrix of features, each needing its own answer. |
| 7 | ❌ P1: "derive.mjs becomes provider-aware" (as the refactor) | The status switch lives in **`events.mjs:273-424`**, and column writes smear across `events/derive/ingest/retention/spawns` + `questions` (holds) + `transcript` (freeform). Nine distinct Claude-coupling categories were catalogued. The seam belongs at **hook intake**, not inside derive. See architecture section. |
| 8 | ⚠️ P3: "built on batch spawn (which already gives N worktrees in one click)" | Batch is **board-side**: `SpawnForm` parses `3x` and POSTs `/api/spawn` sequentially (`SpawnForm.jsx:527-564`; `util.js:291-314`). Fine for a UI front door; an *agent-drivable* or issue-driven fan-out wants a server-side batch (or documented sequential) endpoint. |
| 9 | ⚠️ P4: "read the local usage state each agent already writes" | The daemon reads exactly one thing under `~/.claude` today: session transcripts (`transcript.mjs`, `helpers.mjs:25-27`). Usage reading is greenfield, and the Codex half is degraded upstream (rate_limits null — below). |
| 10 | ❌ P4: account pinning "via CLAUDE_CONFIG_DIR … the same per-session-env discipline" | Mechanically yes at launch — but `CLAUDE_CONFIG_DIR` appears nowhere in the daemon, and the transcript probe **hardcodes `~/.claude/projects/...`** (`helpers.mjs:25-27`). Pin a session elsewhere and revive-eligibility, adopt, and freeform-question detection silently break (`spawns.mjs:1249`, `helpers.mjs:77-85`, `events.mjs:622-624`). Pinning requires making the config-home a per-session property consulted by every transcript-path consumer. |
| 11 | ✅ P5: mail is fire-and-forget (sender-side) | Confirmed — no recipient ack/completion. Nuance in our favor: transport is already **leased + acked** (`mail.mjs:470-495`, `/mail/ack`), `questions` are typed durable records with `status` + `answer_json` (`db.mjs:142-153`), and `/api/watch` is a 25 s long-poll with a waiter registry (`http.mjs:684-720`). T0.1 has three in-house precedents to copy. |
| 12 | ⚠️ P6: permission ladder "replacing the single scary 'unsupervised' checkbox" | Stale: the spawn form **already has** the four-mode dropdown (`default/acceptEdits/plan/bypassPermissions ⚠`, `SpawnForm.jsx:945-948`) *plus* the separately-armed unsupervised checkbox. The real work is consolidating two overlapping controls and porting the vocabulary to Codex's approval×sandbox grid. |
| 13 | ❌ P6: the stream is "the board's Feed + mail, promoted" | The ticker is a bare `(id, at, msg)` string table (`db.mjs:123-127`), classified client-side by emoji prefix (`util.js:515-522`), 40 served/500 kept, **and tool actions never land in it** (`events.mjs:285-307`). A Slack-style stream with channels needs a new structured event model + retention + read cursors — a real (worthy) subsystem, not a promotion. |
| 14 | ⚠️ P5: extend fleet-doctrine into a "versioned skill the daemon serves" | Today the skill is static, shipped in the plugin cache, **not served by the daemon** and not even in the npm tarball (`package.json:10-15`). "Daemon-served, versioned" is the right call and is net-new plumbing. |
| 15 | ✅ Everything else spot-checked | `/api/spawn`, `/command`, `/mail`, `/state` real (`http.mjs`); ticket-first worktree naming real (`spawns.mjs:1076-1088`); gateway credential discipline real and rigorous (`env -u` scrub + `token_set` masking, `env-scrub.mjs`, `settings.mjs:386-406`); unsupervised gate **is server-side** (single-use 60 s arm token, also gating revive & adopt, `spawns.mjs:112-141,1150-1158,1501-1508`); four sanctioned keystrokes real (`spawns.mjs:303-307`); skill teaches `curl /mail` + `GET /state` real (`SKILL.md:95-122`). |

## External premises: what verification changed

**Codex hooks engine (P1's spine).** Real, shipping, same JSON-on-stdin protocol family — but: **opt-in** via `[features].codex_hooks = true` (silent no-op otherwise), experimental with API churn, Windows-disabled, and `PreToolUse`/`PostToolUse` intercept **the shell tool only** — `apply_patch`, file edits, web fetch, and MCP calls fire nothing. `SessionStart/UserPromptSubmit/PermissionRequest/Stop/Subagent*/Compact*` exist; turn-scoped events carry a `turn_id` (useful for checkpoints parity). Consequences the doc must absorb:

- The "fallback" path (notify + pane liveness) is actually the **default** path; first-class hooks require Fleet Deck to edit the user's `config.toml` feature flag *and* `hooks.json` — mutating another tool's config, which wants an explicit consent step and an uninstall story.
- A Codex card loses: **conflict radar** (no FileChanged, no edit telemetry), file chips, edit-driven "working", and possibly mail-by-Stop-block (Claude delivers queued mail by answering Stop with `decision:'block'`, `events.mjs:589-617`; whether Codex's Stop hook supports an equivalent continue-the-turn response must be spiked, not assumed).
- What still works honestly: prompt→working, shell commands (including the test-runner regex → `verifying`, since Bash-shaped telemetry *is* what Codex exposes), PermissionRequest → needs-you (shell approvals), Stop → idle, per-turn boundaries.

**Honest framing for the doc:** a Codex card is a *reduced but derived* card — turn-level lifecycle + shell telemetry — clearly labeled, never claiming parity. That's still far better than a dumb tile, and still ahead of Orca's OSC dots. Sources: [openai/codex#14882](https://github.com/openai/codex/issues/14882), [Codex hooks guide](https://codex.danielvaughan.com/2026/04/15/codex-cli-hooks-complete-guide-events-policy-patterns/), [hooks reference](https://agenticcontrolplane.com/blog/codex-cli-hooks-reference), [community docs](https://github.com/shanraisshan/codex-cli-best-practice/blob/main/best-practice/codex-hooks.md).

**Codex usage files (P4).** Rollout JSONL carries `token_count` events, but recent builds write **`rate_limits: null`** ([openai/codex#14880](https://github.com/openai/codex/issues/14880)) — local files often lack reset windows entirely, or lag hours. Community tools fall back to `auth.json`-based or API-backed reads ([codex-auth](https://github.com/loongphy/codex-auth), [codex-check](https://github.com/Leask/codex-check)). The meter needs a first-class **"unknown — never guess"** state per window, and files reportedly balloon (20 MB+ observed; tail, never slurp — the doc already says this).

**CLIProxyAPI (P4).** Confirmed: v6.10.0 removed built-in usage stats — but *replaced* them with a consumable **usage queue** (RESP, and `/v0/management/usage-queue` over HTTP in v6.10.8+), explicitly designed for companion tools ([README](https://github.com/router-for-me/CLIProxyAPI), [management API](https://help.router-for.me/management/api)). The doc frames Fleet Deck as "adding the missing half" *beside* CPA; the stronger move is to also **consume the queue** as an optional richer usage source for sessions already gateway-routed — Fleet Deck becomes the companion dashboard CPA's own README says to pair it with.

---

## Pillar-by-pillar

### F1 TypeScript — right call, wrong size, needs a stopping rule
The motivation is *stronger* than the doc claims (the /state contract literally lives in comments on both sides of the wire), but at 18k daemon lines + a JSX board, "migrate the daemon, contracts-first, before the surface grows" is a boil-the-ocean invitation. **Amend to:**
- **F1a (do first, timebox ~1–2 weeks):** a `contracts/` module — typed shapes for the snapshot/`/state`, hook payloads (canonical events, below), `/api/spawn` body, mail/command/questions wire formats — consumed by daemon *and* board (and the future Bun binary). The Codex pass lands a real jab here: esbuild **strips types without checking them**, and no `tsc` exists anywhere in the repo — so F1 must include `tsc --noEmit` (or `checkJs`) as a required CI lane, *plus* runtime validation of hostile boundary JSON (hook bodies, spawn bodies) — static types don't validate wire input.
- **F1b (standing rule, not a phase):** all *new* v1.0 code is TS against the contracts; existing modules convert **only when a pillar touches them** (P1 will naturally convert `events/derive/ingest`; P2 converts `worktrees`). No big-bang conversion, tests green throughout.
- Add the missing pieces: `tsconfig` strategy, board typing (`.jsx→.tsx` opportunistic), and a schema-version field in the contracts from day one.

### F2 Bun binary — feasible, but sqlite is not the risk
The deep dive is good news: DB access is strongly centralized (`db.mjs`/`statements.mjs`, no other module imports `node:sqlite`) and the API surface used maps ~1:1 to `bun:sqlite` (no `backup()`, no UDFs, raw `BEGIN IMMEDIATE` strings — portable). The adapter seam is one import site + a class alias. **The actual Bun risks the doc doesn't name:** `mdns.mjs` (1,012 lines of `node:dgram` multicast — Bun's dgram/multicast support is the weakest node-compat corner), the `ws` package under Bun (native WebSocket server vs `ws` semantics), `child_process` + tmux control-mode long-lived pipes, and embedding `board-dist` assets in a compiled binary. Keep F2 last and **explicitly cuttable** from 1.0 ("1.0 ships without brew if compat drags") — the release shouldn't hostage on Bun's dgram.

### P1 Codex provider — right pillar, wrong seam, needs a spike gate
Beyond the external corrections above, the architecture correction (detailed in the next section): normalize at intake, don't parameterize derive. And the scope needs explicit tiers, because "first-class observed provider" today silently implies a dozen Claude-specific features:

- **Tier A (commit for 1.0):** spawn/worktree/kill, status card from Codex hooks (reduced machine), turn boundaries + checkpoints (turn_id exists), shell telemetry incl. verifying, PermissionRequest → needs-you *display*, usage-burn from rollout tails.
- **Tier B (spike, then decide):** mail injection into a Codex pane (the tmux paste primitive is generic, `spawn.mjs:959-1030`, but every eligibility gate says `'claude'`), needs-you *answering* via held hook responses, Stop-block mail delivery, resume/revive (`codex resume`?).
- **Tier C (explicitly out for 1.0, say so):** conflict radar, file chips, `/clear` succession, agents-CLI liveness, remote control.

Gate the pillar on a one-week spike that answers Tier B empirically against a pinned Codex version. The doc's own risk note ("start P1 with a design spike") is right — promote it into the sequencing as a hard gate.

### P2 Harvest — the best pillar; fix the foundation lie and the mechanics
- **Record the base now.** Add `base_ref`/`base_oid` to the `spawns` table and stamp it in `materializeBranch`/worktree-add; fall back to `merge-base HEAD origin/HEAD` (already computed in `worktrees.mjs`) for adopted/legacy sessions, labeled "inferred base". This is a 1-day change that should land *before anything else in v1.0* so every future spawn accrues review-ready data.
- **Checkpoints: async, off the hook path, owned-worktrees-only.** `hookStop` (`events.mjs:589-617`) is the single choke point — but answer the hook first, checkpoint after. Mechanics: temp index + `git add -A` (captures untracked) + `git commit-tree` onto `refs/fleetdeck/<callsign>/turn-<n>` — no commits on the working branch, no stash pollution. Add: a per-session turn counter (doesn't exist; `sessions.events` counts all hooks) — and note the Codex-pass catch that **Stop recurs within one logical turn** when mail blocks it (`events.mjs:592-606`), so checkpoint only on *passing* Stops (or dedupe on the turn id) or every mail delivery mints a phantom turn. Plus: retention/GC for checkpoint refs on despawn (`retention.mjs` is the home), a size guard (skip checkpoint if repo > N or `git add` > T ms, degrade honestly), and **revert gated on idle** — never while the agent is mid-turn.
- **Diff view:** new renderer (unified diff, hunk model, line anchors) in the FileViewer *shell*; `git diff --no-color base..HEAD` + `git status --porcelain` for untracked; cap + paginate large diffs. Notes→one-batched-mail then rides existing mail verbatim.

### P3 Issue/PR — killer feature, plus the finding the doc must not ship without
- **Injection surface (new, must be in the doc):** issue titles/bodies are **third-party text**. P3 prefills them into prompts; P5 makes spawning agent-drivable; the bearer token can mint an unsupervised arm server-side. Chain: hostile issue body → coordinator pastes into spawn → unsupervised worker executes it. Mitigations to write in: issue-derived spawns default to *supervised* (no bypass from issue-flow, period); issue text delivered fenced-and-labeled as untrusted data, not as the raw prompt; and the P5 privilege model below.
- Today's forge integration is **URL composition only** (`repos.mjs:120-160`) — no `gh`/`glab` calls exist anywhere. The fd/git-auth Tier 0 work (built, uncommitted, on the git-auth roadmap) is the natural substrate — land it first; keep its rules (never auto-install CLIs, never store forge tokens).
- Jira contradicts the doctrine line as written: "a Jira token" *is* a stored credential on the board unless it rides the same settings discipline as the gateway token (`token_set: true`, never on /state, never argv). Either commit to that explicitly or cut Jira/Linear from 1.0 (recommend: **GitHub + GitLab only for 1.0**; trackers post-1.0).
- PR-scoped write: keep, but define the scope as an allowlist of verbs (create PR, post review, comment) each requiring a board confirm + feed audit line; never batch, never auto.

### P4 Usage/accounts — split the halves; they're different sizes
- **Claude usage meter:** greenfield but well-understood (Orca's mechanism); pin what's read under `~/.claude`, absence = "unknown". Ship it early — it needs nothing from P1.
- **Codex usage:** burn yes, limits often null (above) — design the "unknown" state first-class, consider `auth.json` as a secondary source, and optionally consume CPA's usage queue for gateway-routed sessions.
- **Multi-account pinning:** bigger than "same env discipline." Requires: per-session `config_home` column consulted by *every* transcript-path consumer (revive, adopt, freeform detection — all hardcode `~/.claude` today), per-account usage attribution, and hooks reporting `CLAUDE_CONFIG_DIR` back (the hook runs inside the session env — it can self-report the home; the daemon can't infer it). Recommend: usage meter in 1.0; account pinning **stretch**, cut cleanly if it slips.

### P5 Programmable fleet — small primitive, big missing chapter: privilege
- T0.1 completions: build as a sibling of `questions` (typed rows: `task_id, session_id, kind done|blocked|question, payload, status, acked_at`), long-poll `GET /orchestration/check?wait&types` copying the `/api/watch` waiter registry. **Add the two failure semantics the doc skips:** dead-worker synthesis (worker goes offline/tombstoned with an open task → synthesize `blocked`, or the coordinator hangs forever) and idempotent ack by id.
- **Privilege model (the gap — sharpened by the Codex pass and verified):** it's worse than "one flat token." On default config (`REQUIRE_TOKEN` off), plain-loopback callers get the historical exemption for everything except three named power routes (`/ws/term`, POST `/mail`, `arm-unsupervised` — `http.mjs:336-348`); **POST `/api/spawn` itself is tokenless on loopback**, and a spawn body may carry `setup_cmd`, which runs `sh -c "$cmd"` *before* claude starts (`spawns.mjs:20-25`). So on the default config, any same-UID process — or any permission-gated agent allowed one `curl` to localhost — gets arbitrary shell execution outside every permission mode, no token, no human gate. The code comment even names "a fleet agent itself" as the attacker the power gates exist for — but spawn-with-setup_cmd isn't gated. Acceptable as a known same-UID-trust-zone residual *today*; **not** acceptable once P5 makes agents first-class API drivers and P3 pipes forge text into prompts. Minimum for 1.0: add spawn (at least with `setup_cmd`/bypass) to the token-gated power routes, two token classes (worker: mail/state/completions; operator: spawn/kill/arm — held by humans and explicitly-blessed coordinator sessions), and agent-initiated spawns capped (`spawnCapability` currently has **no cap**, `spawns.mjs:232-236`) with a per-hour quota. One column + one middleware check, not a rewrite.
- T0.2 skill: daemon-served versioned reference is right; note it's net-new (today the skill is static plugin cargo).

### P6 Stream/chat — right instinct, honest sizing
- The stream is a **new event subsystem**: structured table (`at, session_id, repo_id, type, payload`) written alongside the ticker, channel = derived view, retention policy, read cursors, and *selective* tool-action events (PostToolUse currently doesn't tick — full tool firehose would swamp both the table and the full-snapshot WS broadcast; the 60 ms-coalesced broadcast-everything model, `http.mjs:1370-1382`, will need a delta or per-channel fetch for this one surface).
- Chat composer: the Drawer already has an outbound per-card composer (this-tab-only, `Drawer.jsx:423-451`, `App.jsx:58`) — but the Codex pass is right that the doc's "projected from the hook/rollout events we already receive" oversells: the transcript reader extracts *final* assistant text only (`transcript.mjs:69-104`); rendering an **in-progress** turn needs a live tailer subsystem that doesn't exist. Two honest options: **downscope** to a turn-level thread (final assistant text per turn + outbound mail — feasible from current reads) or **demote chat to post-1.0**. Either way it stays explicitly secondary.
- Permission ladder: reframe as *consolidation* of the existing dropdown + arm flow (fact-check #12), plus the Codex approval×sandbox mapping.

---

## Architecture deep dive — how to actually build the spine

**The provider seam goes at intake, not in derive.** Concretely:

1. **Canonical event vocabulary** (in `contracts/`): `session-start, prompt, tool-start, tool-end{tool, files?, command?, failed?}, needs-you{kind, payload}, turn-end, session-end, file-changed, cwd-changed` + `provider`, `schema_version`. This is roughly what `events.mjs:273-424` already switches on, renamed and frozen.
2. **Provider adapters at the HTTP boundary:** `/hook/:event` (Claude) keeps today's shapes and maps → canonical; a new `/codex-hook/:event` maps Codex's vocabulary → the same canonical stream (PostToolUse-shell → `tool-end{command}`, PermissionRequest → `needs-you`, Stop+turn_id → `turn-end`, …). `applyEvent` then consumes canonical events only and stops knowing provider names. The `events` table gains a `provider` column (sessions already have `source`, `db.mjs:49`).
3. **Provider strategy object** for the non-event coupling — the piece the doc misses entirely: `spawnArgv(opts)`, `resumeArgv(row)`, `paneCommandName` (fixes the `'claude'` gates in `mail.mjs:358` / `spawns.mjs:1738`), `transcript{path, lastAssistantText, model}` (fixes the `~/.claude` hardcode *and* is the P4 multi-account seam — same abstraction), `livenessPoll?` (agents-CLI for Claude, pane-only for Codex), `needsYouAnswer(kind)→wire`, `nudgeGate` (trust-dialog regex), `usageReader`. Claude's strategy is extracted from existing code; Codex's implements the subset (Tier A/B above) and returns "unsupported" elsewhere — cards render what the strategy supports, honestly.
4. **What stays provider-free:** the SQLite store, callsigns/tickets, worktrees, mail queue + transport, questions, plans, completions, the board. That's the membrane holding.

**Why intake-normalization wins:** hook payloads are the *only* churn-exposed boundary (both vendors'); pinning the canonical vocabulary in typed contracts makes upstream churn a mapping bug, not a state-machine bug — and it's precisely the contracts-first F1 work, so **F1a and the P1 refactor are the same task**. Sequence them as one.

**Runtime/DB seam (F2):** `db.mjs` is already the adapter — alias `DatabaseSync`↔`Database` behind one factory, CI matrix `node:sqlite`×`bun:sqlite`. Watch items: dgram/mdns, `ws`, tmux control pipes under Bun (above).

---

## Sequencing — revised

The doc's order (F1 → P1+P4 → P2 → P3 → P5 → P6 → F2) has three problems: it fronts a migration with no user-visible value and no stopping rule; it gates everything on the pillar with the most *external* risk (experimental, opt-in Codex hooks); and it demotes T0.1 to fifth after fleetdeck-future.md ranked it **first** — with no stated reason. Revised:

1. **Base-ref recording** (1 day; data starts accruing) + **F1a contracts module** (timeboxed) — with the canonical event vocabulary in it.
2. **P2 harvest** (diff route → renderer → async checkpoints → batched notes) **+ T0.1 completions** in parallel — both pure git/SQLite/UI, zero external risk, and the two things every user of the fleet feels daily. **+ the P1 Codex spike** (1 week, answers Tier B questions against a pinned version) running alongside.
3. **P1 Codex provider** (intake normalization + Claude strategy extraction + Codex Tier A) — informed by the spike; **P4 Claude usage meter** alongside (independent of P1).
4. **P3 issue/PR** (land fd/git-auth first; GitHub→GitLab; injection-hardened; PR-scoped write allowlist) + **P4 Codex usage**.
5. **P5 completion**: privilege model (token classes + spawn caps) + T0.2 daemon-served skill.
6. **P6** stream (new event subsystem) → chat composer → permission-ladder consolidation.
7. **F2 Bun binary + brew** — explicitly cuttable.
8. Security-review gate (delta audit — new surface: forge writes, control API, multi-account env), then cut v1.0.

## What a credible 1.0 still needs (omitted from the doc entirely)

- **A validation section.** fleetdeck-future.md has one (fail-open proof, determinism proof, exposure proof, version resilience); the v1.0 doc dropped it. Port it, per pillar — e.g. P1's fail-open proof is "Codex hooks disabled → card degrades to notify+liveness, labeled"; P3's exposure proof is "no forge token in /state, argv, or logs."
- **Upgrade/migration story.** ~30 ad-hoc `ALTER TABLE` migrations exist as column-introspection branches with **no `user_version` and no transaction around the migration block** (`db.mjs:261-386`) — workable at today's churn, not at v1.0's, which adds ≥4 tables/columns (base ref, checkpoints, completions, event stream, accounts). Adopt numbered, transactional migrations with `PRAGMA user_version`, and state the compatibility rule (old daemon + new board? new hooks + old daemon? the SessionStart shim already prefers a committed bundle) and a downgrade answer.
- **Versioning of hook payloads and the control API** — `schema_version` in canonical events and in the daemon-served skill, so agents and hooks can detect skew (the run-nonce work was exactly this class of bug).
- **Test strategy named per pillar** — the suite is the repo's trust anchor; two-runtime CI (F2), hook-shape fixtures per provider version (P1), git-fixture repos for checkpoints/diff (P2).
- **Performance bars** — 15 sessions × per-turn checkpoints (git cost budget), event-stream write volume, WS full-snapshot broadcast pressure (`BROADCAST_COALESCE_MS=60`).
- **Platform statement** — Codex hooks are Windows-disabled; macOS CI is advisory-only today; a brew-distributed binary implies promises the CI doesn't currently back.
- **docs/internals** — the t3code lesson the v1.0 doc dropped; a glossary + route map + state machine doc is near-free and 1.0 is its natural moment.

## Adversarial Codex pass

Codex reviewed the doc against the tree independently and returned 15 ranked findings (3× P0, 10× P1, 2× P2). Where they overlap with the Fable review above (Codex hooks parity ❌, wrong provider seam, unrecorded base ref, size claim, T0.1 demotion, missing DoD gates, missing internals docs, CODEX_HOME/CLAUDE_CONFIG_DIR absent from the env machinery) they've been folded into the sections above. What Codex found that the Fable pass missed, **verified before adoption**:

- **[P0, verified ✓] Tokenless loopback spawn + `setup_cmd` shell.** Default-config loopback exempts everything except three power routes; `POST /api/spawn` isn't one of them, and `setup_cmd` runs `sh -c` pre-claude (`http.mjs:336-348`, `spawns.mjs:20-25`). The asks-twice gate protects the *bypassPermissions flag*, not code execution. Folded into P5 above; this is the single most actionable security finding of the whole review.
- **[P1, verified ✓] Stop recurs within a turn** when mail blocks it — checkpoint turn-identity must key on passing Stops. Folded into P2.
- **[P1, verified ✓] No type-checker exists**; esbuild strips without checking — F1 needs `tsc --noEmit` + runtime boundary validation. Folded into F1.
- **[P1, verified ✓] Migrations are unversioned, untransacted** column-introspection branches. Folded into the upgrade story.
- **[P1, verified ✓] The chat surface has no in-progress-turn source** — transcript reads are final-text-only. Folded into P6 (downscope or demote).

Where Codex **overstated and the Fable pass pushes back**:

- *"188 direct `db.prepare/exec/close` uses across nine modules"* — recounted: 113 live in the centralized `statements.mjs` map and 35 in `db.mjs` itself; only **28 direct uses across 7 other modules** (mostly `BEGIN/COMMIT` strings + `questions.mjs`'s 9). DB access *is* strongly centralized; the bun:sqlite adapter seam stands. Codex's adjacent point (migration versioning) survives; its "not thin" framing doesn't.
- *"Keep 1.0 forge operations read-only"* — defensible, but the harder line isn't obviously right: the v1.0 doc *explicitly* revises doctrine rule 3 for PR-scoped write, and the T3 evidence shows the BYO-CLI pattern works. The Fable position: keep PR-scoped write **iff** the security design Codex demands (verb allowlist, identity preview, per-write human confirm, audit line, injection boundaries) ships with it — otherwise drop to read-only rather than slip the release.
- *Verdict calibration.* Codex says "reject as an execution plan." Too strong: the pillars, doctrine analysis, and most mechanism choices survive scrutiny; what fails is calibration (Codex parity, sizes, "already record it") and two omissions (privilege model, release gates). The Fable verdict stands: **right vision, apply the amendment list before execution** — which subsumes everything Codex's alternative sequencing asks for (its proposed order and the revised sequencing above agree almost line for line, independently derived).

## The amendment list for fleetdeck-v1.0.md

1. F1: correct the size (18k lines / 35 modules / 630 KB bundle); split into F1a contracts (timeboxed) + F1b standing rule; state that F1a and the P1 intake-normalization are one piece of work.
2. P1: rewrite the hooks-engine paragraph — opt-in flag + consent step for editing `~/.codex/config.toml`; shell-only tool telemetry; no conflict radar/file chips for Codex; "reduced but derived" card framing; Tier A/B/C scope table; spike as a hard gate.
3. P1: replace "`derive.mjs` becomes provider-aware" with intake normalization + provider strategy object (the coupling is in `events.mjs` + five other modules).
4. P2: delete "we already record it"; add base-ref recording as the first shippable change of v1.0; specify async-off-hook-path checkpoints, turn counter, ref namespace, GC, size guard, idle-gated revert.
5. P3: add the injection-surface paragraph (issue text = untrusted; issue-flow spawns never bypass; fenced delivery); scope 1.0 to GitHub+GitLab via fd/git-auth; define the PR-write verb allowlist + confirm + audit.
6. P4: add the "unknown, never guess" state (Codex rate_limits null); add CPA usage-queue as optional source; downgrade account *pinning* to stretch with the transcript-path/config-home refactor named as its prerequisite.
7. P5: add the privilege model (token-gate `/api/spawn` — today tokenless on loopback with `setup_cmd` shell; token classes; spawn caps — no cap exists today) and dead-worker completion synthesis; note the doctrine skill is static today.
8. P6: size the stream honestly (new structured event subsystem; ticker is untyped strings; tool actions don't tick today); reframe the ladder as consolidation.
9. Sequencing: adopt the revised order (or explain why Codex-before-harvest survives the spike-risk argument); restore consistency with fleetdeck-future.md's "T0.1 first".
10. Append the validation-proofs section (port from fleetdeck-future.md), the migration/versioning story, per-pillar test strategy, performance bars, platform statement, docs/internals, and a security-review gate before the cut.
