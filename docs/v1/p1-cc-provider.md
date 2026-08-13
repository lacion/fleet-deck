# P1 — Claude Code as a first-class provider

*Part of [Fleet Deck v1.0](./README.md). Sibling to [P1 — Codex](./p1-codex-provider.md); together they are the **provider layer** of 1.0. Seam design in [architecture](./architecture.md); the drive tier that makes this provider drive-by-default is [P7 — drive + observe](./p7-drive-and-observe.md). This doc is the Claude Code reality: the observe floor that is Fleet Deck today, the card every other provider's card is measured against, and the Layer-4 strategy a driven session (P7) extends rather than replaces.*

## Why Claude Code gets its own provider doc now

Fleet Deck was born observing exactly one harness: plain `claude`, through Claude Code hooks. For most of its life "the provider" was so singular it was invisible — the coupling to Claude smeared across the codebase (nine categories, catalogued in [architecture](./architecture.md#the-nine-claude-coupling-categories-what-the-strategy-unwinds)) because nothing forced it to be *a* provider rather than *the* runtime. Two things change that at 1.0:

- **Codex arrives** ([P1 — Codex](./p1-codex-provider.md)), so "Claude" stops being ambient and becomes one entry in a provider layer — extracted behind the strategy object the seam defines ([architecture](./architecture.md#layer-4--the-provider-strategy-object)).
- **Drive arrives** ([P7](./p7-drive-and-observe.md)). Claude Code is the first provider Fleet Deck *drives* — through the Agent SDK — while still observing it through the same hooks. That makes "the Claude provider" two things at once: an **observe floor** (`claude`) and a **drive-default tier** (`claudeSdk`). This doc owns the floor and the shared card; P7 owns the drive tier.

So this is not a new-feature doc — it is the doc that finally names what was always the spine, now that there is a second provider to contrast it against and a drive tier built on top of it.

## The two roles Claude Code plays

Under the committed stance ([README rule 1](./README.md#the-doctrine-evolved)), a provider shows up in two roles. For Claude Code:

| Role | Identifier | What it is | Owned by |
|------|-----------|------------|----------|
| **Drive-default** | `claudeSdk` | Fleet Deck drives the session via the Agent SDK (`query()`) — answerable approvals, interrupt, steer, resume — **and the same hooks still fire**, so it is observed byte-identically | **[P7](./p7-drive-and-observe.md)** |
| **Fail-open floor** | `claude` | plain `claude` in a tmux pane, observed through `settings.json` hooks — **this is Fleet Deck today** — the automatic fallback when the SDK / runner / login is unavailable | this doc |

The floor is not a lesser product that got demoted; it is the whole of Fleet Deck as it ships today, kept as the guarantee that a broken or drifted SDK can never dark the fleet. The drive tier rides on top of it and falls back to it. This doc specs the floor and the card both roles render; the mechanics of driving are P7's.

## The reference card (what "honestly derived" is measured against)

Every other provider's card is described by what it *loses* relative to a Claude card — [P1 — Codex](./p1-codex-provider.md) is literally a subtraction list. So the Claude card is worth stating positively — it is the reference, not a reduction:

- **full status machine** — `prompt → working → verifying → needs-you → idle → offline`, driven by the complete hook vocabulary (`events.mjs:273-424`);
- **file chips + conflict radar** — `file-changed` / `tool-end{files}` telemetry feeds cross-session collision detection; **the one signal Codex structurally cannot supply on its floor** (P1 — Codex);
- **edit-driven "working"** — working is inferred from real edit telemetry, not only shell activity;
- **mail-by-Stop-block** — queued mail is delivered by answering the Stop hook with `decision:'block'` (`events.mjs:589-617`);
- **`/clear` succession, agents-CLI liveness, remote control** — the Claude-semantic side-channels (`derive.mjs:503-715`, `agents-poll.mjs:38`, `/rc` `spawns.mjs:1738`).

None of this is new work for the floor — it is what ships today. It is catalogued because 1.0's job is to hold this card steady behind the strategy object while a second provider renders a smaller one honestly, and while the drive tier (P7) renders a *richer* one on top.

## The observe floor — how it works today (and stays working)

The floor is the current product; 1.0 changes only where it sits, not what it does:

- **Install.** The plugin installs Claude Code hooks, unchanged — the same fail-open HTTP-POST design Codex reuses (`p1-codex-provider.md`), pointed at `/hook/:event`.
- **Intake.** `/hook/:event` maps today's Claude shapes onto the canonical event stream ([architecture Layer 3](./architecture.md#layer-3--provider-adapters-at-the-http-boundary)). This is an *extraction*, not a rewrite — the mapping already exists inline; F1a pins it against the canonical vocabulary.
- **Strategy.** Claude's Layer-4 strategy is **extracted from existing code with no behavior change** — it is the reference implementation of the strategy object, the thing Codex's strategy is a subset of.

### Claude's Layer-4 strategy fields

The non-event coupling, named ([architecture Layer 4](./architecture.md#layer-4--the-provider-strategy-object)). These are the fields the `claudeSdk` drive tier overrides (P7); everything else it inherits:

| Method | `claude` (the floor — extract from today's code) | Overridden by `claudeSdk` (P7)? |
|--------|--------------------------------------------------|--------------------------------|
| `spawnArgv(opts)` | today's `claude …` argv in a worktree | **yes** — launches the SDK runner, not bare `claude` |
| `resumeArgv(row)` | `claude --resume` | **yes** — SDK `resume` / `continue` |
| `paneCommandName` | `'claude'` — the value the hardcoded gates (`mail.mjs:358,402`, `spawns.mjs:1738`) check against | **yes** — the runner's pane command |
| `transcript{path,lastAssistantText,model}` | `~/.claude/projects/…` (fixes the `helpers.mjs:25-27` hardcode) — **and the P4 multi-account seam** | replaced by the SDK session id / stream |
| `livenessPoll?` | `claude agents --json` (`agents-poll.mjs:38`) | the `query()` runtime's own liveness |
| `needsYouAnswer(kind)→wire` | display-only on the floor (human answers in the pane) | **yes — becomes answerable** via `canUseTool` |
| `nudgeGate` | the trust-dialog regex (`spawns.mjs:319`) | n/a (the runner owns bring-up) |
| `usageReader` | `~/.claude` rollout reader (P4) | SDK `result`-message usage, alongside |

That the drive tier is *the same strategy with a handful of overrides* is the payoff of the seam: `claudeSdk` is not a second provider bolted on, it is `claude` with the process owned by Fleet Deck and the control fields lit up.

### Auth & config isolation

Both roles isolate config via **`CLAUDE_CONFIG_DIR`, never `HOME`** (overriding `HOME` breaks macOS keychain OAuth → "Not logged in"), and run on the user's existing **subscription OAuth** (or API key / Bedrock) — never a metered key Fleet Deck supplies. This is the same per-instance config-home abstraction P4 needs for multi-account pinning; `transcript{path}` and the config-home seam do double duty. No subscription/OAuth credential ever lands in `/state`, argv, or logs ([[fleetdeck-security-standing]]).

## Doctrine check

- **Observe thesis — preserved, even under drive.** The floor is pure observe (the membrane as it is today). The drive tier keeps the *same* hooks firing (the P7 linchpin), so every card stays pure derivation from canonical events regardless of role — the daemon never parses a model response to render a card.
- **No model calls.** Neither role calls a model in the core; the SDK runner (drive) is a spawned process exactly as plain `claude` (floor) is. The daemon stays arithmetic + SQL + git.
- **Drive is a rule-1 amendment, and it is P7's to make.** Driving Claude via the SDK is the *mediate* posture the project deliberately avoided; that doctrine cost is owned and argued in [P7](./p7-drive-and-observe.md#doctrine-check-the-honest-cost) and the amended [README rule 1](./README.md#the-doctrine-evolved). This doc does not re-litigate it — it specs the floor that makes the amendment safe.

## Validation & definition of done

- **Fail-open proof:** if the SDK runner or login is unavailable, a Claude session runs as the plain-`claude` floor, observed through hooks, with a full card — the fleet stays lit (the drive→floor fallback is proven in [P7](./p7-drive-and-observe.md#definition-of-done)).
- **Extraction proof:** Claude's strategy object is extracted with **zero card behavior change** — the reference card renders identically before and after the seam lands (diff the derived state for the same hook stream).
- **Determinism proof:** the card is a pure function of canonical events; zero model calls in either role.
- **Exposure proof:** no subscription/OAuth credential in `/state`, argv, or logs; config isolated via `CLAUDE_CONFIG_DIR`.
- **Acceptance:** a `claude` worktree spawns from the board and renders the full reference card (status machine, file chips, conflict radar, mail-by-Stop-block); the same session, driven as `claudeSdk`, renders the identical card **plus** the control surface (P7); and a driven session whose runner dies falls back to the floor without darking.
