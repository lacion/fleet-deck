# Fleet Deck v1.0 — the command deck

*Written 2026-08-07 against v0.22.3. The product synthesis of three competitive analyses: [orca-lessons.md](./orca-lessons.md), [t3code-lessons.md](./t3code-lessons.md), and the [fleetdeck-future.md](./fleetdeck-future.md) roadmap. This is the vision; those are the evidence.*

## What 1.0 means

Today Fleet Deck is a **coordination membrane for Claude Code sessions** — it observes plain `claude` via hooks and puts every session on one board. v1.0 graduates it into **the command deck for a multi-agent, multi-provider dev fleet**: more than one harness, work that starts from issues and ends in reviewed PRs, a real diff-and-revert review surface, operational awareness of usage and accounts, and a fleet a coordinator session can *drive* — all without giving up what makes it worth running.

The bet of 1.0 is that we can widen the scope this far and **still be a membrane**: no wrapper, fail-open, a deterministic core that makes zero model calls, loopback by default, a plugin and not an app. Everything below is filtered through that.

## How v1.0 evolves the doctrine

The five rules hold; the scope they cover grows. Stated for the bigger surface:

1. **Claude-first → provider-pluggable, still by *observing*.** We add Codex by consuming *its* hooks — not by driving it through an SDK or app-server. We stay in the "observe" corner of *observe / host / mediate* (see [t3code-lessons.md](./t3code-lessons.md)); we never own the agent's process. The membrane widens; it does not become a client.
2. **No model calls in the core — preserved.** Even "review this PR with my security skill" means the daemon *spawns an agent* that does the review. The agent is the intelligence; the daemon stays arithmetic, SQL, and git.
3. **Read-only forge → PR-scoped write, via the user's own CLIs.** We relax the forge stance just far enough for the PR workflow, authenticated by the user's own `gh` / `glab` on the machine (the T3 Code pattern), never a hosted service, never a stored token on the board.
4. **Loopback / no phone-home — preserved.** The chat surface and the activity stream are served by the local board. No hosted relay. Remote stays claude.ai-handoff + Tailscale/Coder.
5. **Plugin, not app — preserved.** A chat view is a board page; the terminal stays the primary, authoritative surface.

---

## Foundations — the platform bets everything else rides on

Two questions to settle first, because the pillars are heavier if the ground is soft.

### F1. Move the daemon to TypeScript — **recommend: yes, incrementally, contracts-first**

The daemon is ~200 KB of plain `.mjs` today, bundled by esbuild, on Node's built-in `node:sqlite`. The v1.0 features — a provider abstraction across Claude and Codex, waitable orchestration completions, forge integrations, usage math — are exactly the kind of growth where missing types turn into runtime bugs. T3 Code is the existence proof: a multi-agent control plane of this complexity leans hard on typed contracts (it even has a `packages/contracts`), and it shows.

- **Cost is low.** esbuild already bundles; it strips TS with no new runtime dependency and no change to the "one bundled file, `node:sqlite`, no `npm install`" story.
- **Do it contracts-first.** The highest-value types are at the boundaries: the HTTP payloads (`/api/spawn`, `/command`, `/mail`, `/state`), the hook-event shapes, the provider-status interface, and the orchestration lifecycle. Define those as a typed contracts module (T3 Code's model), then convert modules incrementally behind them. Keep the test suite green the whole way ([[test-suite-is-trust]] — no quarantining to make a migration "pass").
- **Verdict:** a foundation for 1.0, not a rewrite. It directly serves the stated goal — fewer bugs through strong typing.

### F2. A Bun-compiled single binary as an *additive* standalone channel — **recommend: yes, alongside Node (not instead of it)**

The appeal is real: `brew install fleetdeck`, one static binary, and an end to the Node-version dance (the `node:sqlite` floor already bites — 22.5–22.12 can't boot the daemon; see [[local-dev-018-testing]]). But the plugin channel must stay Node:

- **Keep Node + `node:sqlite` for the plugin-embedded daemon.** The SessionStart hook launches the daemon under the Node that Claude Code already runs. That path is the doctrine-critical one — fail-open, no native deps, nothing to install. A per-platform Bun binary there would regress "it just works with the Node already here."
- **Add a Bun single binary for the *standalone board server*** — the `npm i -g fleetdeck` / Coder / LAN-board use, which is precisely where a one-command `brew` install and no Node floor are a clean win. `bun build --compile` produces the binary; `bun:sqlite` replaces `node:sqlite`.
- **The seam:** put DB access behind a thin adapter so the same TS source targets `node:sqlite` (plugin bundle, via esbuild) and `bun:sqlite` (standalone binary, via Bun). CI both runtimes. The hooks themselves are runtime-agnostic — they just POST HTTP — so only the daemon runtime is in question.
- **Verdict:** Bun is an additive *distribution* win (brew, single binary), not a core runtime swap. Pairs naturally with F1: write once in TS, ship two ways.

---

## The v1.0 pillars

### P1. A second harness: Codex, as a first-class *observed* provider

The architectural spine of 1.0. The happy surprise from the research: **Codex now has an experimental event-hooks engine** (`~/.codex/hooks.json` / `[hooks]` in `config.toml` / repo `.codex/hooks.json`) emitting `SessionStart / PreToolUse / PermissionRequest / PostToolUse / UserPromptSubmit / PreCompact·PostCompact / SubagentStart·SubagentStop / Stop` — **the same lifecycle vocabulary Claude Code exposes.**¹ That means a Codex card can be *honestly derived*, not a dumb terminal tile, through the pipeline we already have.

- **Refactor status derivation into a provider interface.** `derive.mjs` becomes provider-aware: a Claude adapter (Claude Code hooks, as today) and a Codex adapter (Codex hooks engine), mapping each event vocabulary onto the shared `queued → working → verifying → needs-you → idle → offline` lifecycle.
- **Install hooks per provider.** For Claude, the plugin already does it. For Codex, Fleet Deck writes its telemetry hooks into `~/.codex/hooks.json` (the same fail-open HTTP-post design).
- **Graceful fallback.** Where the hooks engine is off or unavailable, fall back to Codex's `notify` (`agent-turn-complete` JSON: `type`, `last-assistant-message`, `cwd`, `input-messages`)¹ plus tmux pane liveness — coarser status, honestly labeled as such.
- **Spawning already generalizes.** Fleet Deck spawns tmux workers today; spawning `codex` instead of `claude` in a worktree is a small step once the provider interface exists.
- **Caveats to design around:** the hooks engine is *experimental* (expect churn — pin and adapt), and Codex session rollout files can balloon to hundreds of MB,¹ so anything reading them (usage, P4) must **tail, never slurp**.

> This is the "observe" model from [t3code-lessons.md](./t3code-lessons.md) extended to a second agent — explicitly *not* the "mediate" model. We consume Codex's hooks; we do not drive it through its app-server.

### P2. The harvest surface: diff view + notes + per-turn checkpoints

The review deck we completely lack today. Three pieces, one arc:

- **Diff on the card** (roadmap A2): a daemon route runs `git diff` against the spawn's recorded start-from ref, rendered in the existing `FileViewer`. Deterministic, no new heavy dep.
- **Notes → one batched mail** (roadmap A3): annotate diff lines; on send, compose **one line-anchored mail** to the agent — *not* a drip. (Orca's own lesson: dripping makes the agent swing back and forth; one batch gives one revision pass and a higher hit rate.)
- **Per-turn checkpoints** (the T3 Code gift): the **Stop hook fires at every turn boundary** — on Stop, tag a cheap git checkpoint in the worktree, record `{callsign, turn, ref, base}`, and expose **"diff since spawn," "what changed this turn," and "revert this turn"** on the card. Git-only, deterministic, and it makes both the diff view and the compare-the-winner flow dramatically stronger than Orca's manual delete-the-losers.

### P3. Issue- and PR-driven spawning — the killer feature

Where work *starts* and where it gets *checked*. Built on batch spawn (which already gives N worktrees / N branches in one click) plus the BYO-credential forge pattern.

- **Spawn parallel agents from issues.** Browse GitHub / Linear / Jira issues (read via the user's own `gh` / `glab` / a Jira token, same credential discipline as gateway routing — never onto `/state` or a command line), then **launch a fleet from selected issues**: one worker per issue, each in its own worktree, the issue title/body prefilling the prompt and the worktree named ticket-first (`<repo>--fd-PROJ-123-<animal>`, which we already do). This is batch spawn with an issue tracker as its front door.
- **Point at a PR → spawn a reviewer.** Give a PR URL; Fleet Deck checks out the branch in a worktree and spawns an agent to review it — **optionally invoking a skill the user already has installed globally** (e.g. `security-review`, `ai-code-review`). The daemon spawns and checks out; the *agent* reviews (core stays model-free).
- **PR-scoped write is in scope for 1.0.** Create the PR, post the review/comments, from the card, via the user's own `gh`/`glab`. Write is deliberately *PR-scoped* — not owning the whole hosting workflow (no in-app auto-merge queues; that's application territory). Multi-forge matters ([[dual-forge-github-gitlab]]): GitHub first, GitLab close behind.

### P4. Usage, rate-limits, and multi-account — the operations deck

The number that governs a 100x day, plus the ability to spread load across accounts.

- **Usage & rate-limit awareness** (roadmap A4): read the local usage state each agent already writes — `~/.claude` for Claude, `~/.codex` rollout JSONL for Codex¹ — with **no API and no extra auth**. Surface the reset windows (5-hour / daily / weekly), a warning chip at 80%, per-session and fleet-total burn, sorted tightest-window-first, cost from a local price table ("inferred pricing"). Honest caveat baked in: numbers update when the agent writes, not in real time. Tail the (potentially huge) Codex logs.
- **Multi-account.** Manage several Claude/Codex OAuth homes; **pin a spawn to a specific account** (via `CLAUDE_CONFIG_DIR` / `CODEX_HOME`, the same per-session-env discipline as gateway routing), and show **per-account** limit bars. The operator move this unlocks: *"account A is 15 minutes from reset — launch this one on account B."*
- **Relationship to CLIProxyAPI.** Fleet Deck already routes per-session through a gateway, with CLIProxyAPI as the reference. CLIProxyAPI does the OAuth multi-account rotation/load-balancing — but **removed built-in usage stats in v6.10.0**.² So the layers are complementary: lean on CLIProxyAPI (or native homes) for the *routing*, and let Fleet Deck add the **visibility and per-agent account pinning** it dropped. Not redundant — the missing half.

### P5. A programmable fleet: agent control skill + waitable completions

Turn the coordination nouns we already own into a substrate a coordinator *session* can drive (roadmap Tier 0). Fleet Deck already exposes `/api/spawn`, `/command`, `/mail`, `/state`, and the `fleet-doctrine` skill already teaches agents to `curl /mail` and `/state`. Two additions complete it:

- **T0.1 — waitable, typed completions:** a worker posts `done / blocked / question` against a task id; a coordinator long-polls for the next unacked one and blocks until it lands. The one orchestration primitive we lack (mail is fire-and-forget). Useful to a human-driven board too ("wake me when this worker is done").
- **T0.2 — a documented agent control skill:** extend `fleet-doctrine` from "mail + read state" to the full deterministic surface — roster, spawn, assign, mail, **wait** — as a thin, versioned skill the daemon serves (so documented flags can't rot). The guardrails already exist: spawn's red asks-twice unsupervised gate, the four-mode permission ladder (below), the sanctioned-keystroke rule.

### P6. Unified views — terminal grid, a Slack-style stream, and optional chat

- **The "see every terminal at once" desire is half-shipped.** We already have the **Terminal Grid** (`▦ Terminals`) — every live agent streaming, one focused tile accepting input. That's the tiled view. *(Orca's is split/tiled panes, not a merged stream — so the Slack-style idea is a Fleet Deck synthesis, not a port.)*
- **Add a Slack-style activity stream.** One merged, human-readable feed with a **channel per session (and per repo)**: turn boundaries, tool actions, needs-you prompts, and mail, rendered as messages — and you can **post into a channel**, which is just mail to that session. It's the board's `Feed` + mail, promoted into a first-class unified stream. This is the view that scales to "watch fifteen agents like a Slack workspace."
- **A chat interface — secondary, terminal stays primary.** An *optional* per-card composer that renders the agent's current turn as chat (projected from the hook/rollout events we already receive) and sends prompts as mail-to-pane. It is explicitly the secondary surface: the real terminal remains the authoritative one, and the chat view never becomes load-bearing. This is the one place we borrow T3 Code's "better than a terminal" instinct — as an *addition beside* the terminal, not a replacement for it.

**A cross-cutting tidy:** adopt a normalized **four-mode permission vocabulary** — Supervised / Auto-accept-edits / Auto / Full-access (T3 Code's ladder, mapping onto Claude's `--permission-mode` and Codex's approval policy) — replacing the single scary "unsupervised" checkbox on the spawn form with a legible ladder.

---

## Deferred to post-1.0

- **Design Mode** (roadmap A7) — genuinely liked; needs an embedded browser bridge we don't want in the core. Post-1.0, as an optional out-of-core helper reusing the screenshot-upload path.
- **Driving agents via SDK / ACP (the "mediate" model)** — stays out. ACP is noted as the enabler *if we ever change our mind*; 1.0 stays in the observe corner.
- **Hosted relay / native mobile apps** — rejected on doctrine (loopback). claude.ai-handoff + Tailscale remains the remote path.
- **Remote spawn (agent on another host)** — Coder/LAN covers the need for now; revisit if it proves insufficient.
- **LLM-in-product niceties** (auto PR/thread titles) — deterministic naming (animals/tickets) stays the default; any model-written text is an opt-in extra, never core.

---

## Sequencing to 1.0

1. **F1 — TypeScript, contracts-first.** Unblocks everything; do it before the surface grows.
2. **P1 — Codex provider abstraction** (the spine) **+ P4 usage** (rides on reading `~/.claude` + `~/.codex`).
3. **P2 — harvest surface** (diff + notes + per-turn checkpoints).
4. **P3 — issue/PR spawning + PR review** (GitHub first, then GitLab; Linear/Jira read + spawn).
5. **P5 — programmable fleet** (T0.1 completions, then T0.2 control skill).
6. **P6 — unified stream + optional chat**, and the four-mode permission ladder.
7. **F2 — Bun single binary + brew** as the standalone packaging for the release.
8. **Cut v1.0.**

## What "1.0" asserts (definition of done)

Fleet Deck 1.0 is: **multi-provider** (Claude + Codex, both honestly observed); **issue-to-parallel-agents** and **point-at-a-PR review** with the user's own skills and credentials; a real **diff-and-revert review deck** with per-turn checkpoints; **operational awareness** of usage, rate-limits, and accounts; an **agent-drivable fleet** with waitable completions; a **Slack-style stream** over the terminal grid with an optional chat surface — and all of it still a **fail-open, loopback, deterministic-core plugin**, now installable as a **single `brew` binary** for the standalone board.

## Risks & open questions

- **Codex hooks engine is experimental** — API churn is likely. Pin what we read, keep the `notify` + pane-liveness fallback, and start P1 with a design spike.
- **TS migration on a large `.mjs` codebase** — do it incrementally, contracts-first, tests green throughout ([[test-suite-is-trust]]).
- **Node vs Bun DB divergence** (`node:sqlite` vs `bun:sqlite`) — a DB adapter seam, CI on both runtimes; don't let the standalone binary and the plugin bundle drift.
- **Multi-account credential handling** — reuse the gateway discipline exactly: never on the board, in `/state`, or on a process command line.
- **PR-scoped write** — keep it PR-scoped and confirm-before-write; do not drift into owning the hosting workflow.

---

¹ Codex CLI observability — `notify` hook, the experimental event-hooks engine, and `~/.codex/sessions/**/rollout-*.jsonl` with `token_count` usage — per the Codex configuration reference and community documentation (see chat sources).
² CLIProxyAPI removed built-in usage statistics in v6.10.0, deferring to companion tools; it provides OAuth multi-account load-balancing across Claude/Codex/Gemini/Grok behind Anthropic/OpenAI/Gemini-compatible endpoints (see chat sources).
