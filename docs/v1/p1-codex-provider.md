# P1 — Codex as a first-class *observed* provider

*Part of [Fleet Deck v1.0](./README.md). The architectural spine of 1.0. Seam design in [architecture](./architecture.md); this doc is the Codex reality, the tiered scope, and the spike gate. Where the vision and review differ, the review wins.*

## Problem — one board, one provider

Today Fleet Deck observes exactly one harness: plain `claude`, through Claude Code hooks. The entire "multi-provider command deck" claim of 1.0 rests on this pillar — everything else (harvest, usage, the stream) is provider-shaped work that only pays off twice if a second provider actually lands.

The happy surprise from the research: **Codex ships an experimental event-hooks engine** — a `SessionStart / PreToolUse / PermissionRequest / PostToolUse / UserPromptSubmit / PreCompact·PostCompact / SubagentStart·SubagentStop / Stop` vocabulary over the same JSON-on-stdin protocol family Claude Code uses. That means a Codex card can be **honestly derived** through the pipeline we already own — *not* a dumb terminal tile.

But the vision oversold "the same lifecycle vocabulary." Verification changed the premise materially, and the honest version is below.

## The Codex reality (what verification changed)

The vision claimed parity; the review verified otherwise. Build against **this**, not the optimistic framing:

- **Experimental and OFF by default.** The engine is opt-in via `[features].codex_hooks = true` in `config.toml` — a silent no-op otherwise. **Windows support is disabled.** Expect **API churn**: pin what we read, adapt.
- **Shell-tool-only telemetry.** `PreToolUse`/`PostToolUse` intercept **the shell tool only**. `apply_patch`, file edits, web fetch, and MCP calls fire **nothing**. There are **no `FileChanged`/`CwdChanged` equivalents** at all.
- **The "fallback" is actually the default.** Because the hooks engine is off until someone flips the flag, the coarse path — Codex's `notify` (`agent-turn-complete` JSON: `type`, `last-assistant-message`, `cwd`, `input-messages`) plus tmux pane liveness — is the **default** experience. First-class hooks require Fleet Deck to **edit the user's `config.toml` feature flag *and* `hooks.json`** — i.e. mutate another tool's configuration. That demands an explicit **consent step** and a clean **uninstall story** (see Doctrine check).
- **Turn scoping exists.** Turn-scoped events carry a `turn_id` — directly useful for P2 checkpoint parity (checkpoint on turn boundaries without inventing our own counter for Codex).
- **Rollout files balloon.** Session rollout JSONL can reach hundreds of MB. Anything that reads them (P4 usage) must **tail, never slurp**.

## The honest card: reduced-but-derived, never parity

A Codex card is a **reduced but derived** card — turn-level lifecycle plus shell telemetry, clearly labeled. It never claims parity with a Claude card. It is still far better than a dumb tile, and still ahead of Orca's OSC dots.

**What a Codex card loses** (no mechanism upstream):

- **conflict radar** — no `FileChanged`, no edit telemetry, so no cross-session file-collision detection;
- **file chips** — same root cause;
- **edit-driven "working"** — working can only be inferred from shell activity and turn boundaries, not from edits;
- **possibly mail-by-Stop-block** — Claude delivers queued mail by answering the Stop hook with `decision:'block'` (`events.mjs:589-617`). Whether Codex's Stop hook supports an equivalent *continue-the-turn* response **must be spiked, not assumed**.

**What still works honestly:**

- `prompt → working` (UserPromptSubmit);
- **shell commands**, including the test-runner regex → `verifying` — Bash-shaped telemetry is *exactly* what Codex exposes, so this classifier ports cleanly;
- `PermissionRequest → needs-you` (shell approvals — display works even if answering doesn't; see Tier B);
- `Stop → idle`;
- **per-turn boundaries** via `turn_id`.

## Scope tiers

The vision's phrase "first-class observed provider" silently implied a dozen Claude-specific features. Make the scope explicit:

| Tier | Meaning | Contents |
|------|---------|----------|
| **A** | **Commit for 1.0** | spawn / worktree / kill; status card from Codex hooks (reduced machine); turn boundaries **+ checkpoints** (`turn_id` exists); shell telemetry incl. `verifying`; `PermissionRequest → needs-you` **display**; usage-burn from rollout **tails** |
| **B** | **Spike, then decide** | mail **injection** into a Codex pane; needs-you **answering** via held hook responses; **Stop-block** mail delivery; resume/revive (`codex resume`?) |
| **C** | **Explicitly OUT for 1.0** (say so) | conflict radar; file chips; `/clear` succession; agents-CLI liveness; remote control |

**On Tier B — why it's a spike, not a commitment.** The tmux paste primitive is generic (`spawn.mjs:959-1030`), so *mechanically* pasting into a Codex pane is trivial. What blocks it is that **every eligibility gate hardcodes `'claude'`** — owned-pane mail (`mail.mjs:358,402`), `/rc` (`spawns.mjs:1738`). Those become `paneCommandName` strategy checks (see [architecture](./architecture.md#layer-4--the-provider-strategy-object)). But *whether the delivered mail actually reaches the Codex turn loop* — and whether Stop-block works — depends on Codex internals we must observe empirically.

**On Tier C — why it's out.** Each item depends on a Claude-only side-channel (edit telemetry, `claude agents --json`, Claude-semantic `/clear`). No honest Codex mechanism exists; a card that faked them would violate the reduced-but-derived contract.

## How it works (tech spec)

The seam is defined in [architecture](./architecture.md) — intake normalization plus a provider strategy object. Do **not** re-derive it here. P1 delivers, on top of that seam:

- **A Codex intake adapter** at `/codex-hook/:event` mapping Codex's vocabulary → the canonical event stream: `PostToolUse`(shell) → `tool-end{command}`, `PermissionRequest` → `needs-you`, `Stop`+`turn_id` → `turn-end`, `SessionStart`/`UserPromptSubmit` → `session-start`/`prompt`. **No** `file-changed`/`tool-end{files}` emitted — the honest gap.
- **A Codex provider strategy** implementing the Tier A/B subset (`spawnArgv`, `paneCommandName='codex'`, `transcript{path}` = rollout reader, `usageReader` = rollout tail, `livenessPoll` = pane-only) and returning `"unsupported"` for everything in Tier C. Cards render exactly what the strategy supports.
- **The `provider` column** on `events`/`sessions` (sessions already carry `source`, `db.mjs:49`), so downstream stays provider-blind.
- **Per-provider install.** Claude: the plugin already installs hooks, unchanged. Codex: Fleet Deck writes its telemetry hooks into `~/.codex/hooks.json` (the same fail-open HTTP-post design) **and** sets the `[features].codex_hooks` flag in `config.toml` — behind consent, with uninstall.
- **Spawn generalizes.** The argv/env layer already takes a generic env map → tmux `-e` (`spawns.mjs:556-578`); spawning `codex` instead of `claude` in a worktree is the `spawnArgv` strategy method plus the gate fixes above.

## The spike gate (a hard gate, not a soft note)

The vision buried this as a risk note ("start P1 with a design spike"). Promote it to a **hard gate in the sequencing**:

- **A one-week spike against a *pinned* Codex version** answering the Tier B questions **empirically**: does mail injected into a Codex pane reach the turn loop? Does Stop support a continue response? Does `codex resume` restore a session cleanly? Does `PermissionRequest` allow a held/answered response, or only observation?
- It runs in **phase 2**, in parallel with P2 harvest and P5 T0.1 completions (see [README sequencing](./README.md#sequencing-to-10-revised)) — none of which touch Codex, so the external risk is quarantined.
- **P1 proper (phase 3)** is *informed by* the spike: Tier A ships regardless; each Tier B item ships only if the spike says the mechanism exists.

## Doctrine check

- **Observe, not mediate.** We consume Codex's hooks and read its rollout files; we do **not** drive Codex through its app-server or an SDK. The membrane widens; it does not become a client.
- **The one mutation.** Editing `config.toml` + `~/.codex/hooks.json` is the single place 1.0 writes another tool's configuration. It is gated behind an **explicit consent step**, ships with a **one-command uninstall**, and is **fail-open**: if the flag is off or the engine unavailable, the card degrades — it never blocks or breaks Codex.
- **No model calls.** Nothing in P1 calls a model; the card is pure derivation from hook/notify/rollout data.

## Risks & open questions

- **Experimental engine / churn.** Pin the read version; keep the `notify` + pane-liveness fallback live as a first-class (not emergency) path.
- **The Stop-block question.** The single biggest unknown; if Codex's Stop can't continue a turn, mail-by-Stop-block is Codex-impossible and mail must arrive by paste-only — the spike decides.
- **Mutating `config.toml`.** Consent + uninstall are mandatory; never silent.

## Validation & definition of done

- **Fail-open proof:** with `codex_hooks` disabled (or the engine absent), a Codex card degrades to `notify` + pane-liveness, **labeled as reduced**, and nothing crashes — the daemon and board carry on.
- **Determinism proof:** the Codex path makes zero model calls; a card is a pure function of the events/notify/rollout it received.
- **Exposure proof:** no Codex credential or rollout content lands in `/state`, argv, or logs (usage reads are tail-only and numeric).
- **Acceptance:** a real `codex` worktree spawns from the board; its card shows a reduced-but-derived status machine (prompt→working→verifying→needs-you→idle) with shell telemetry; per-turn checkpoints tag on `turn_id` (P2); usage burn accrues from rollout tails (P4) — and every Tier B feature is present or absent **exactly as the spike decided**, never faked.

---

¹ Codex hooks engine, `notify` protocol, shell-only tool telemetry, opt-in feature flag, and rollout `token_count`/size behavior — per [openai/codex#14882](https://github.com/openai/codex/issues/14882), the [Codex CLI hooks complete guide](https://codex.danielvaughan.com/2026/04/15/codex-cli-hooks-complete-guide-events-policy-patterns/), the [hooks reference](https://agenticcontrolplane.com/blog/codex-cli-hooks-reference), and [community docs](https://github.com/shanraisshan/codex-cli-best-practice/blob/main/best-practice/codex-hooks.md).
