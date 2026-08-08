# What Fleet Deck can learn from T3 Code

*Written 2026-08-07 from a source read of [pingdotgg/t3code](https://github.com/pingdotgg/t3code) + [t3.codes](https://t3.codes) and its `docs/internals`. Against Fleet Deck v0.22.3.*

Sibling to [orca-lessons.md](./orca-lessons.md); the shared adoption roadmap is [fleetdeck-future.md](./fleetdeck-future.md). T3 Code is, as you said, *fundamentally* different from both Orca and us — more so than Orca is. Most of its architecture is deliberately not for us. But precisely because it made the opposite bet at the deepest layer, it validates some of our instincts and hands us a few concrete, doctrine-safe primitives. This doc separates the two.

## The clarifying frame: observe vs. host vs. mediate

All three tools sit "above" a coding agent, but they attach to it in three different places — and that placement *is* each product.

- **Fleet Deck *observes*.** Plain `claude` runs in the user's own terminal. The plugin's hooks observe it and post telemetry to a loopback daemon; a browser board coordinates many sessions. We never own the agent process. Membrane, Claude-native, fail-open, zero model calls in the core.
- **Orca *hosts*.** It embeds a Ghostty-class terminal, runs any CLI agent *inside* it, and reads a thin OSC-title status line. A terminal-IDE application, agent-agnostic by virtue of being a terminal host.
- **T3 Code *mediates*.** It does not host a terminal for the agent conversation at all. It speaks each agent's **structured protocol** — the **Claude Agent SDK** for Claude, **ACP (Agent Client Protocol)** for Cursor/Grok, a **Codex app-server** runtime for Codex, OpenCode's own — through a per-agent *adapter*, records every exchange as an **event-sourced Thread**, and renders its own UI. Its motto is literally "your agents deserve better than a terminal."

That trichotomy is the whole story. Orca and Fleet Deck both keep the agent's terminal (Orca hosts it, we leave it with the user). **T3 Code throws the terminal away and consumes the agent's structured output instead.** That is the fundamental difference — and it's why adopting T3 Code's architecture would not make us a better membrane; it would make us a different product.

## What T3 Code actually is (grounded in the source)

Open-source (MIT, ~17k stars) by T3 Tools (Theo). "The open-source control plane for coding agents." Bring-your-own-subscription — *"No keys resold. No quota caps."* Forkable as the pitch — *"if you don't like it, fork it."*

- **Shape:** a headless **server** (owns agents, owns PTYs) + thin **clients** (web, Electron desktop, native iOS/Android). Clients connect locally or over **Tailscale / SSH / a hosted relay** (Cloudflare + Postgres). Effect-based TypeScript monorepo.
- **Nouns** (`docs/internals/glossary.md`): a **Project** (a `workspaceRoot` + title) contains **Threads**. A **Thread** is *"the main durable unit of conversation and workspace history"* — it gets its own branch and an optional git **worktree**. A **Turn** is one user→assistant cycle (ends when the session leaves `running`). An **Activity** is a user-visible log item (approval, tool action, failure). A **Session** is the live provider runtime attached to a thread.
- **Provider adapters** (`docs/internals/providers.md`): five built-in drivers — `claudeAgent` (**Claude Agent SDK**), `codex` (app-server), `cursor` + `grok` (**ACP**), `opencode` — each a driver + adapter conforming to one `ProviderAdapter` interface. The headline principle: *"the orchestration layer does not know which one is behind a thread."* Adding an agent is a driver + adapter and **no core change**.
- **Event-sourced orchestration:** clients never call a provider directly. They dispatch orchestration **commands** (`thread.turn.start`, `thread.turn.interrupt`, `thread.approval.respond`, `thread.checkpoint.revert`, `thread.runtime-mode.set`, …); the engine **persists a domain event** (the source of truth); a **reactor** performs the provider call; provider output comes back as internal commands (`thread.message.assistant.delta`) that clients observe via a subscription. Command → Event → Projector → Read model. Serious, principled CQRS.
- **Per-turn checkpointing:** a `CheckpointReactor` *"captures workspace checkpoints on turn start and completion, and performs reverts."* Each turn has a captured baseline and a finalized diff; `thread.checkpoint.revert` rolls a turn back.
- **Four permission modes** (`docs/user/permission-modes.md`), per-thread from the composer: **Supervised** (ask before commands and edits), **Auto-accept edits** (edits auto, commands ask), **Auto** (routine proceeds, risky asks — Claude uses its own `auto` mode; OpenCode with no equivalent falls back to asking), **Full access** (default; unattended). Plus an interaction mode: `default` / `plan`.
- **Source control** (`docs/user/source-control.md`): clone / publish / PR across GitHub, GitLab, Bitbucket, Azure DevOps — authenticated by the **user's own CLIs on the server** (`gh auth login`, `glab`, `az`, Bitbucket env tokens), never resold, never stored by the app. Suggests PR titles/bodies from commits.
- **Terminal** (`docs/architecture/terminal-renderers.md`): the *terminal feature* (separate from the agent conversation) stays server-owned PTYs streaming raw bytes; clients render with `libghostty-vt` (a C ABI: WASM on web → Canvas, JNI on Android). Renderer choice never crosses the wire.

## The genuine lessons — doctrine-safe, ranked by value

Fleet Deck deliberately made the opposite bet at every layer: observe plain `claude` via hooks (not the SDK), keep the terminal (not replace it), SQLite with derived status (not event sourcing), Claude-native (not protocol-normalized). So this is **not** a "port these features" list — it's the handful of primitives and validations that survive the translation through our membrane.

### 1. Per-turn checkpoints (baseline → diff → revert) — the strongest transferable idea

T3 Code checkpoints the workspace on **every turn start and completion**, keeps the per-turn diff, and can **revert a turn**. Fleet Deck has no turn-level checkpoint, diff, or revert — and this is the most valuable thing in the whole codebase for us, because it *turbo-charges the harvest arc* already planned in [fleetdeck-future.md](./fleetdeck-future.md) (A2 diff view, A3 compare-and-merge). If every turn is a lightweight git checkpoint, a card could show **"what changed this turn," "diff since spawn," and "revert this turn"** — a far stronger review story than Orca's manual delete-the-losers.

And we already have the trigger: the **Stop hook fires at every turn boundary.** On Stop, tag a cheap checkpoint in the worktree (a commit on a shadow ref, or a `git stash create` object, or a per-turn tag), record `{callsign, turn, ref, base}` in SQLite, and expose diff + revert on the card. Deterministic, git-only, no model call — doctrine-clean. **This should become a new item in the harvest tier of the roadmap** (it sits naturally between A2 and A3, and makes both better).

### 2. ACP is the doctrine-safe enabler for the "agent-agnostic middle path"

Both prior docs flagged a narrow middle path: showing non-Claude panes as *dumb* cards without corrupting our honest derived status. T3 Code shows the *right* way to get **real** status from a non-Claude agent — **ACP (Agent Client Protocol)**, a structured JSON-RPC channel carrying turn/tool/permission events, which Cursor, Grok, and Gemini already speak. So if Fleet Deck ever wanted first-class non-Claude sessions, ACP — not PTY-scraping — is the clean path: an ACP client could feed the *same* status pipeline our hooks feed today. This upgrades the middle path from "dumb card" to "possible real card," but it's a large commitment and stays speculative. The point is that the enabler exists and is standardized.

**The sharp corollary — why we use hooks and not the SDK.** T3 Code drives *Claude itself* through the **Claude Agent SDK**, which is richer than hook telemetry. But the SDK requires *being the client* — owning the session process. Fleet Deck cannot use it without becoming a wrapper, which breaks our first doctrine rule (no wrapper; observe plain, user-launched `claude`). So T3 Code inadvertently validates our choice: **the SDK is for clients that own the agent; hooks are the only structured Claude telemetry available to a membrane that doesn't.** Different tool, different layer, correct for each.

### 3. A normalized four-mode permission vocabulary

`Supervised / Auto-accept-edits / Auto / Full-access` is a clean, user-legible articulation. Our spawn form has essentially a scary binary — "unsupervised (skip permissions)" behind a red asks-twice gate — plus plan mode. Claude Code itself already exposes `--permission-mode` with `plan`, `acceptEdits`, `default`, `bypassPermissions`; adopting T3 Code's **naming** (especially "auto-accept-edits" as a legible middle rung) would make the spawn form clearer with no architectural change — a rung ladder instead of one dangerous checkbox. Small, tidy, doctrine-safe.

### 4. A per-card turn timeline (a projection over telemetry we already receive)

T3 Code's read model gives each thread a durable timeline of turns and activities (approvals, tool actions, failures). Our cards show status + a ticker but no per-turn history. We already receive the raw material — PreToolUse / permission / Stop hook events — so a lightweight **turn timeline on a card** is a *projection* over data we already have, not a new subsystem or an event-sourcing rewrite. It also mirrors a principle worth stealing verbatim: T3 Code has callers *"name a thread, not an agent"* — exactly our "address a callsign, not a PID." Validation of our own design.

### 5. Bring-your-own-credential, server-side forge auth — validation of the A6 pattern

T3 Code's clone/publish/PR integration authenticates via the **user's own CLIs on the server** (`gh auth login`, `glab auth login`, `az login`, Bitbucket env tokens) — never resold, never stored by the app. That is exactly the credential discipline I proposed for **A6 spawn-from-issue** in the roadmap, now validated by a second project — and extended across **four forges** (GitHub / GitLab / Bitbucket / Azure), which matters given our [[dual-forge-github-gitlab]] standing. Reinforces A6. Note the boundary: T3 Code *also does* the write-heavy in-app PR workflow, which we reject as app territory — we take the **auth pattern**, not the PR ownership.

### 6. Validation, not adoption: server/client split, local-first, Tailscale

T3 Code = a headless server that owns agents + thin clients that connect locally or over Tailscale/SSH. That is Fleet Deck's exact shape (daemon owns tmux; browser board; loopback + LAN/Tailscale + Coder). One divergence: T3 Code adds a **hosted relay** (Cloudflare) for mobile — a phone-home component we reject on doctrine (§4), the same call we made against Orca Relay. So: architecture validated, hosted relay declined, our claude.ai-handoff + Tailscale remains the doctrine-consistent remote path. Cross-ref [[tailscale-lan-access]].

### 7. Meta: a contributor-facing `docs/internals`

T3 Code ships a real `docs/internals/` — an overview, a **glossary**, a providers guide, a remote guide — that makes an intricate event-sourced system legible to the forkers its pitch invites. We have excellent README prose and `CODER.md`, but no internals glossary. Given we already claim the open/forkable posture (own marketplace, MIT — see [[oss-repo-infrastructure]]), a short `docs/internals/` (a glossary of card/callsign/spawn/mail/plan/hook-event; the `derive.mjs` status state machine; the daemon route map) would lower the contributor barrier for near-zero cost.

## What to explicitly *not* adopt — the "fundamentally different" guardrails

Each is central to T3 Code and would dissolve our membrane if we took it.

| T3 Code choice | Why it's not us | Doctrine failed |
|---|---|---|
| Drive Claude via the **Agent SDK** / be the client that owns the process | We observe plain, user-launched `claude`; owning it is a wrapper. The SDK is for clients, not membranes. | no-wrapper |
| **Event-sourced orchestration domain** (commands/events/deciders/projectors/reactors) | A beautiful architecture and massive over-engineering for a coordination membrane. Our SQLite + *derived* status already captures the good part — "status is a projection, not a self-report." | plugin-not-app |
| **Replace the terminal** with a rendered thread UI | Our bet is the opposite: keep the user's real terminal, add a board beside it. Different product. | plugin-not-app |
| **Protocol-normalize all agents** / agent-agnostic core | Our honest status is Claude-hook-specific by design (Orca's generic alternative is thin OSC dots). ACP is noted as the *enabler if we ever change our mind*, not a plan. | erodes core value |
| **In-app write-heavy PR workflow** (one-button commit/push/PR, suggested titles, stacked PRs) | App territory. We take the auth pattern (§5) and spawn-from-issue, not PR ownership. | plugin-not-app |
| **Hosted relay** for mobile | Phone-home; we bind loopback and hand off to claude.ai for remote. | loopback / no-phone-home |
| **LLM-in-product niceties** (auto PR/thread titles via `textGeneration`) | Model calls; our naming is deterministic (animals/tickets). Opt-in extra at most. | no-model-calls |

## One-line takeaway

T3 Code is the **"mediate"** corner of *observe / host / mediate* — it normalizes agents behind structured protocols and event-sources everything, the deliberate opposite of our membrane, and mostly not for us. But it hands us three doctrine-safe gifts: **per-turn checkpoints (diff + revert)** that would turbo-charge the planned harvest arc, the fact that **ACP is the clean enabler** if we ever want real non-Claude status, and **validation** — of our credential discipline, our server/client split, our "status is a projection" instinct, and (via the SDK-vs-hooks contrast) our very decision to be a membrane rather than a client. Plus a nudge to write a `docs/internals` glossary. The only item I'd promote into [fleetdeck-future.md](./fleetdeck-future.md) is per-turn checkpoints; the rest is validation and one speculative door (ACP) left ajar.
