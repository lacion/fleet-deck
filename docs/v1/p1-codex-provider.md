# P1 — Codex as a first-class *observed* provider

*Part of [Fleet Deck v1.0](./README.md). Sibling to [P1 — Claude Code](./p1-cc-provider.md); together they are the **provider layer** of 1.0. Seam design in [architecture](./architecture.md); the cross-provider drive tier is [P7 — drive + observe](./p7-drive-and-observe.md). This doc is the Codex **observe floor** — the reduced-but-derived card Fleet Deck renders from Codex's own hooks and rollout files. Codex reaches drive-default later than Claude (its hooks engine is still regressing), so the floor is where Codex lives first; [P7](./p7-drive-and-observe.md) is how and when it gets driven. Where the vision and review differ, the review wins.*

## Problem — one board, one provider

Today Fleet Deck observes exactly one harness: plain `claude`, through Claude Code hooks. The entire "multi-provider command deck" claim of 1.0 rests on this pillar — everything else (harvest, usage, the stream) is provider-shaped work that only pays off twice if a second provider actually lands.

The happy surprise from the research: **Codex ships an experimental event-hooks engine** — a `SessionStart / PreToolUse / PermissionRequest / PostToolUse / UserPromptSubmit / PreCompact·PostCompact / SubagentStart·SubagentStop / Stop` vocabulary over the same JSON-on-stdin protocol family Claude Code uses. That means a Codex card can be **honestly derived** through the pipeline we already own — *not* a dumb terminal tile.

But the vision oversold "the same lifecycle vocabulary." Verification changed the premise materially, and the honest version is below.

## The two roles Codex plays

Like [Claude Code](./p1-cc-provider.md#the-two-roles-claude-code-plays), Codex shows up in two roles — but in the opposite order of readiness:

| Role | Identifier | What it is | Status |
|------|-----------|------------|--------|
| **Fail-open floor** | `codex` | `codex` in a tmux pane, observed through its config-resident hooks + `notify` + rollout tails — a **reduced-but-derived** card | **ships now — this doc** |
| **Drive tier** | `codexAppServer` | Fleet Deck drives the session over the `codex app-server` protocol — in-protocol approvals, interrupt, steer, resume — with the same hooks still firing | **[P7](./p7-drive-and-observe.md), staged after Claude** |

Claude goes drive-default immediately ([P1 — Claude Code](./p1-cc-provider.md)); Codex stays floor-first and earns drive-default once its hooks engine stops regressing. This doc owns the floor and its honest limits; [P7](./p7-drive-and-observe.md) owns the drive tier.

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
- **mail-by-Stop-block** — Claude delivers queued mail by answering the Stop hook with `decision:'block'` (`events.mjs:589-617`); the Codex floor has no equivalent Stop-continuation, so **floor mail is display-only**. Control-grade delivery arrives with the drive tier ([P7](./p7-drive-and-observe.md)), where a mid-turn `turn/start` steers the session directly — so this stops being a floor question at all.

**What still works honestly:**

- `prompt → working` (UserPromptSubmit);
- **shell commands**, including the test-runner regex → `verifying` — Bash-shaped telemetry is *exactly* what Codex exposes, so this classifier ports cleanly;
- `PermissionRequest → needs-you` (shell approvals — **display** on the floor; **answerable** via the drive tier, [P7](./p7-drive-and-observe.md));
- `Stop → idle`;
- **per-turn boundaries** via `turn_id`.

## Scope tiers

The vision's phrase "first-class observed provider" silently implied a dozen Claude-specific features. Make the scope explicit:

| Tier | Meaning | Contents |
|------|---------|----------|
| **A** | **The floor — ships for 1.0** | spawn / worktree / kill; status card from Codex hooks (reduced machine); turn boundaries **+ checkpoints** (`turn_id` exists); shell telemetry incl. `verifying`; `PermissionRequest → needs-you` **display**; usage-burn from rollout **tails** |
| **B** | **The drive tier ([P7](./p7-drive-and-observe.md))** | needs-you **answering**, **interrupt**, **steer** (mail-as-turn), resume — delivered by driving the `codex app-server` protocol (`codexAppServer`), **not** by pasting into a pane. Staged **after** Claude ([P7](./p7-drive-and-observe.md#does-this-generalize-codex-via-the-app-server-protocol)) |
| **C** | **Out on the floor** | conflict radar & file chips (no file telemetry from Codex hooks — but the **drive tier** surfaces file changes over protocol via `turn/diff` + `item/fileChange`, so this is a *floor-only* gap, not a Codex-forever one); `/clear` succession, agents-CLI liveness, remote control (Claude-only side-channels — out) |

**On Tier B — why it's the drive tier, not paste.** The old plan was to paste mail into a Codex pane; [P7](./p7-drive-and-observe.md) supersedes it. Codex control comes from *driving* the app-server: its native in-protocol approvals (`item/{commandExecution,fileChange}/requestApproval`) are a real `canUseTool` analog, `turn/interrupt` is a real interrupt, and a mid-turn `turn/start` is a steer. The `paneCommandName` gate fixes (owned-pane mail `mail.mjs:358,402`, `/rc` `spawns.mjs:1738`, hardcoded `'claude'`) still matter for the **floor's** display and for keeping downstream provider-blind — but the *control verbs* are P7's, and they don't depend on whether a paste reaches the turn loop.

**On Tier C — why it's out on the floor.** File chips and conflict radar need file-level edit telemetry, which Codex's hooks don't emit. On the **floor** that's a hard gap — a card that faked it would violate the reduced-but-derived contract. The **drive tier** ([P7](./p7-drive-and-observe.md)) narrows it: `turn/diff` + `item/fileChange` expose file changes over protocol, so a *driven* Codex card can grow real file chips. `/clear` succession, `claude agents --json` liveness, and remote control stay Claude-only side-channels with no Codex equivalent.

## How it works (tech spec)

The seam is defined in [architecture](./architecture.md) — intake normalization plus a provider strategy object. Do **not** re-derive it here. P1 delivers, on top of that seam:

- **A Codex intake adapter** at `/codex-hook/:event` mapping Codex's vocabulary → the canonical event stream: `PostToolUse`(shell) → `tool-end{command}`, `PermissionRequest` → `needs-you`, `Stop`+`turn_id` → `turn-end`, `SessionStart`/`UserPromptSubmit` → `session-start`/`prompt`. **No** `file-changed`/`tool-end{files}` emitted — the honest gap.
- **A Codex provider strategy** implementing the Tier A floor subset (`spawnArgv`, `paneCommandName='codex'`, `transcript{path}` = rollout reader, `usageReader` = rollout tail, `livenessPoll` = pane-only) and returning `"unsupported"` for Tier C. The Tier B control verbs are the `codexAppServer` override on top of this same object ([P7](./p7-drive-and-observe.md)) — exactly the Claude floor→`claudeSdk` relationship ([P1 — Claude Code](./p1-cc-provider.md#claudes-layer-4-strategy-fields)). Cards render exactly what the active strategy supports.
- **The `provider` column** on `events`/`sessions` (sessions already carry `source`, `db.mjs:49`), so downstream stays provider-blind.
- **Per-provider install.** Claude: the plugin already installs hooks, unchanged. Codex: Fleet Deck writes its telemetry hooks into `~/.codex/hooks.json` (the same fail-open HTTP-post design) **and** sets the `[features].codex_hooks` flag in `config.toml` — behind consent, with uninstall.
- **Spawn generalizes.** The argv/env layer already takes a generic env map → tmux `-e` (`spawns.mjs:556-578`); spawning `codex` instead of `claude` in a worktree is the `spawnArgv` strategy method plus the gate fixes above.

## The drive tier lands after Claude — not a spike, not a gate

The floor above (Tier A) ships now. Driving Codex — answering approvals, interrupt, steer, resume — is [P7](./p7-drive-and-observe.md)'s `codexAppServer` tier, and it is **staged after Claude on purpose**, because Codex's *observe* half is still regressing: repo-local `config.toml` hooks don't fire interactively ([openai/codex#17532](https://github.com/openai/codex/issues/17532)), and there's a Desktop hooks regression ([#21639](https://github.com/openai/codex/issues/21639)). The linchpin that makes drive + observe free — the driven process keeps firing the *same* config-resident hooks — is load-bearing, and on Codex that half isn't stable yet. So:

- **The floor is Codex's default** until the hooks engine stabilizes — reduced-but-derived, fail-open, no drive dependency.
- **Drive is opt-in on Codex** while Claude is drive-default, and flips to default once the observe half holds under an `app-server`-driven turn. This is a **stability call, not a timebox** — Tier A never waits on it.
- The old "one-week spike against a pinned version" framing is retired: we build the `codexAppServer` tier as part of [P7](./p7-drive-and-observe.md) and turn it on for Codex when Codex is ready, not on a clock.

## Doctrine check

- **The floor observes; the drive tier mediates ([P7](./p7-drive-and-observe.md)).** The Codex **floor** is pure observe — it consumes Codex's hooks and reads its rollout files, driving nothing; the membrane widens but does not become a client. The **drive tier** *does* drive Codex, over the `codex app-server` protocol — the rule-1 amendment argued and owned in [P7](./p7-drive-and-observe.md#doctrine-check-the-honest-cost) — but the same config-resident hooks keep firing under it, so even a driven Codex card stays pure derivation from canonical events. On Codex that amendment is staged after Claude; until then, the floor is what ships. This doc does not re-litigate the amendment — it specs the floor that makes it safe.
- **The one mutation.** Editing `config.toml` + `~/.codex/hooks.json` is the single place 1.0 writes another tool's configuration. It is gated behind an **explicit consent step**, ships with a **one-command uninstall**, and is **fail-open**: if the flag is off or the engine unavailable, the card degrades — it never blocks or breaks Codex.
- **No model calls.** Nothing in the Codex floor calls a model; the card is pure derivation from hook/notify/rollout data. Under the drive tier the `app-server` child is a spawned process, not a daemon model call — the daemon stays arithmetic + SQL + git ([P7](./p7-drive-and-observe.md#doctrine-check-the-honest-cost)).

## Risks & open questions

- **Experimental engine / churn.** Pin the read version; keep the `notify` + pane-liveness fallback live as a first-class (not emergency) path.
- **Mail delivery on the floor.** The floor has no answer-the-Stop-hook continuation the way Claude does (`events.mjs:589-617`), so floor mail to a Codex session is **display-only**. Control-grade delivery — steering a Codex turn — is the drive tier ([P7](./p7-drive-and-observe.md)): a mid-turn `turn/start` *is* the steer, so mail-by-paste and the old Stop-block question both dissolve rather than needing a verdict.
- **Mutating `config.toml`.** Consent + uninstall are mandatory; never silent.

## Validation & definition of done

- **Fail-open proof:** with `codex_hooks` disabled (or the engine absent), a Codex card degrades to `notify` + pane-liveness, **labeled as reduced**, and nothing crashes — the daemon and board carry on.
- **Determinism proof:** the Codex path makes zero model calls; a card is a pure function of the events/notify/rollout it received.
- **Exposure proof:** no Codex credential or rollout content lands in `/state`, argv, or logs (usage reads are tail-only and numeric).
- **Acceptance (floor):** a real `codex` worktree spawns from the board; its card shows a reduced-but-derived status machine (prompt→working→verifying→needs-you→idle) with shell telemetry; per-turn checkpoints tag on `turn_id` (P2); usage burn accrues from rollout tails (P4); Tier C features are **honestly absent**, never faked; and if `codex_hooks` is off the card degrades to `notify` + pane-liveness without crashing.
- **Acceptance (drive tier, [P7](./p7-drive-and-observe.md)):** the same session driven as `codexAppServer` answers shell approvals from the board/phone, interrupts and steers over the protocol, and grows real file chips from `turn/diff` — while the same hooks keep firing so the card stays byte-identical in its observed fields. Shipped when Codex's observe half is stable, not on a clock.

---

¹ Codex hooks engine, `notify` protocol, shell-only tool telemetry, opt-in feature flag, and rollout `token_count`/size behavior — per [openai/codex#14882](https://github.com/openai/codex/issues/14882), the [Codex CLI hooks complete guide](https://codex.danielvaughan.com/2026/04/15/codex-cli-hooks-complete-guide-events-policy-patterns/), the [hooks reference](https://agenticcontrolplane.com/blog/codex-cli-hooks-reference), and [community docs](https://github.com/shanraisshan/codex-cli-best-practice/blob/main/best-practice/codex-hooks.md).
