# Drive + observe — Claude via the Agent SDK, Codex via app-server

*Part of [Fleet Deck v1.0](./README.md) as **P7**. Sibling to [P1 — Codex (observed)](./p1-codex-provider.md), which becomes the fail-open floor beneath this; rides the same seam in [architecture](./architecture.md). This is the plan for **how Fleet Deck runs agents**: drive them through their native protocol (Claude via the Agent SDK, Codex via `app-server`) **and** keep observing them through their own config-resident hooks, in one session. Observe-only is not removed — it drops to the automatic fail-open floor beneath the default. This amends doctrine **rule 1** and **rule 5** (both updated in the [README](./README.md)) and supersedes the old "Deferred to post-1.0 → driving via SDK/ACP stays out" line. The first half is the worked example for **Claude** (the Agent SDK); the second half — [Does this generalize? Codex](#does-this-generalize-codex-via-the-app-server-protocol) — shows the same hybrid holds for **Codex** via app-server. Read it as the destination and the design, not a proposal to weigh.*

---

## Why driving no longer costs us the observe layer

The membrane's whole reason to stay in the *observe* corner is that *observe* is all we could get without becoming a client. Hooks are one-way telemetry: they tell us a tool is about to run (`PreToolUse → needs-you`), but the board can only *display* the gate — the human answers it in the pane, because the hook process has already handed control back to `claude`. We cannot inject a turn, interrupt a runaway, steer mid-turn, or answer an approval *from the board*. Every one of those is a **control** verb, and control was the thing "mediate" cost us the membrane to buy.

The Agent SDK (`@anthropic-ai/claude-agent-sdk`) changes the accounting, because of one documented behaviour:

> **The SDK fires the user's `settings.json` hooks by default.** "Filesystem hooks: shell commands defined in `settings.json`, loaded when `settingSources` includes the relevant source… If you already have hooks in your project's `.claude/settings.json` and you set `settingSources: ["project"]`, those hooks run automatically in the SDK with no extra configuration." Omitting `settingSources` is equivalent to `["user","project","local"]`. Filesystem hooks support the `http` kind (POST to an endpoint) and "fire in the main agent and any subagents it spawns."¹

Fleet Deck's telemetry hooks are exactly that: a fail-open HTTP POST to the daemon (the same design P1 reuses for Codex, `p1-codex-provider.md:63`). So a session driven through `query()` **keeps posting the identical canonical events to `/hook/:event`** that a plain `claude` session posts today — *while* the SDK hands us the control surface hooks never could. This is the first provider where *mediate* buys control **without giving up *observe***— the two ride the same session instead of trading off.

That is the headline, and the rest of this doc is how we cash it in — and the honest doctrine cost of doing so. It is not Claude-specific: the same shape — *drive via the native protocol, observe via the agent's own config-resident hooks* — holds for Codex too (see [below](#does-this-generalize-codex-via-the-app-server-protocol)). Claude is the worked example because Track A asked to "upgrade Claude Code to be a provider"; Codex is the confirmation that the pattern is general.

---

## What it's better at — the UX and DX win

This is the part that matters most, and the reason to do it: the tool becomes dramatically better to *use* and to *build on*. Everything else in this doc is plumbing in service of the following.

**For the human on the board (UX):**

- **Answer an approval from anywhere.** `needs-you` stops being a display-only badge you walk to a pane to clear. You tap allow/deny from the board — or your phone over Tailscale — and the session proceeds. The single most common interruption in a day of agent work becomes a one-tap action from wherever you are.
- **Interrupt a runaway across the fleet.** Stop a turn that's heading the wrong way without switching into its pane and mashing Esc — one control, any session, including from a coordinator.
- **Steer without pane-switching.** Drop a mid-turn "actually, use the other API" as a first-class injected message instead of racing the agent's prompt in a terminal.
- **A structured live turn view.** The in-progress turn renders as plans, diffs, and the pending gate *inline* — not a scraped TUI. You see *what the agent is about to do* as data, and act on it in place.
- **Resume without the pane dance.** Revive a session with one action; no `claude --resume` menu, no hunting for the right transcript.
- **Review a plan inline.** `ExitPlanMode` is captured and shown as a first-class plan-review surface on the board, not something you approve blind in a pane.

**For whoever builds on the fleet (DX):**

- **A coordinator that actually drives.** P5's "a fleet a coordinator can drive" becomes real verbs — answer, interrupt, steer, inject — under a privilege model, not `spawn` + paste-mail-into-a-pane heuristics.
- **One control vocabulary across providers.** The same answer/interrupt/steer surface works for Claude (`canUseTool`) and Codex (`item/*/requestApproval`); a skill written once drives both.
- **A real event feed for P6.** The live per-delta turn stream P6 needs is delivered by the same channel that drives the session — no separate transcript-tailer to invent.

That is the product bet: an agent fleet you *operate* from one deck — approvals, interrupts, steering, plan review, resume — instead of a wall of terminals you tab between. The observe layer keeps every card honest; the drive layer makes the deck something you can actually fly.

## The primitive P5 and P6 are already reaching for

Two committed pillars are already reaching for control the observe model can't supply:

- **P5 (programmable fleet)** wants "a fleet a coordinator session can **drive**" — waitable completions, a privilege model, a control skill. Today "drive" bottoms out at *spawn* and *paste mail into a pane*; it cannot answer an approval, interrupt a turn, or steer one. The SDK's `canUseTool` / `interrupt()` / streaming-input are those verbs, first-class.
- **P6 (unified views)** notes chat with in-progress-turn rendering "needs a live transcript tailer that doesn't exist" (README:125). The SDK message stream (`includePartialMessages: true`) **is** that tailer — a structured, per-delta event feed — delivered for free by the same channel that drives the session.

So this isn't control for its own sake; it's the missing primitive under two things the plan already committed to build the hard way.

## The improved way — control and observe in one session

One long-lived `query()` per session (the pattern t3code uses, `ClaudeAdapter.ts`): a streaming-input prompt queue feeds the agent loop, and the async message stream feeds back out. On top of that single channel:

| Control verb | SDK mechanism | What it unlocks for the board |
|---|---|---|
| **answer an approval** | `canUseTool` callback (invoked only when the permission flow falls through to a prompt) → resolve allow/deny with `updatedInput` | `needs-you` becomes **answerable from the board**, not display-only — Tier B for Codex, native here |
| **interrupt** | `Query.interrupt()` | stop a runaway turn from the board / a coordinator agent |
| **steer mid-turn** | offer another `SDKUserMessage` onto the streaming input | inject guidance without a new process |
| **inject a turn** | same streaming input | P5 "drive the fleet" without tmux paste heuristics |
| **capture a plan** | intercept `ExitPlanMode` (capture markdown, deny with "wait for the user") | plan review surface without scraping the pane |
| **resume / fork** | `resume` / `forkSession` / `continue` options | revive without the `claude --resume` pane dance |
| **live turn stream** | message stream + `includePartialMessages` | P6's missing in-progress-turn source |

And simultaneously, unchanged: the existing `settings.json` hooks POST `session-start / prompt / tool-start / tool-end / needs-you / turn-end / session-end / file-changed / cwd-changed` to `/hook/:event`. The board's derive/state machine (`events.mjs:273-424`) renders the card with **zero new code** — the SDK session is observed by the exact pipeline a plain session is.

## The default becomes drive; observe becomes the floor

Earlier drafts framed this as a second, opt-in provider sitting *beside* observed `claude` — a thing you pick off a menu. That undersells it and gets the arrow backwards. Display-only approvals, no interrupt, no steer, a transcript we can only tail after the fact — that is a real ceiling, and the drive UX clears it. So the stance is: **drive+observe (`claudeSdk`) is the default path a session gets.** Observe-only (`claude`) does not go away; it is demoted to the **fail-open floor** — the automatic fallback when the SDK, its runner, or the login is unavailable, and the compatibility lane for any provider that exposes no drive protocol. Two *roles* (default + floor), not two peers on equal footing.

That distinction is the whole point of the reframe. Keeping the floor is not hedging against the idea — it is the **fail-open guarantee doctrine already requires**: a broken or version-drifted SDK must never dark the fleet. What changes is which one a session reaches for first.

| | **`claudeSdk` — the default (drive + observe)** | **`claude` — the fail-open floor (observe-only)** |
|---|---|---|
| **when it runs** | whenever the SDK + runner + login are healthy — the normal case | automatic fallback: SDK absent/broken/drifted, runner dead, or a provider with no drive protocol |
| **process owner** | Fleet Deck (`query()` owns the `claude` process) | the user (plain `claude` in a tmux pane) |
| **telemetry** | `settings.json` hooks → `/hook/:event` | **identical hooks, identical route** |
| **control** | answerable approvals, interrupt, steer, resume, plan capture | display-only; human acts in the pane |
| **terminal** | a Fleet-Deck-rendered live pane of the driven session (see terminal model) | the real `claude` TUI |
| **doctrine** | *mediate + observe* (rules 1 & 5 evolve — see doctrine check) | pure observe; membrane intact |

The observe story is byte-identical across both columns — the same hooks, the same `/hook/:event`, the same derive pipeline (`events.mjs:273-424`) render the card either way. The floor exists so the default can fail safe, not so users have to opt in to the good path. If you want the floor *gone entirely* — no observe-only lane at all — that is a separate and heavier call; the doctrine check argues against it (fail-open forbids it), but it's yours to make.

## How it slots into the seam (tech spec)

Do not re-derive the seam — [architecture](./architecture.md) owns it. `claudeSdk` rides it like any provider:

**Layer 3 (intake) — nothing new.** No `/claude-sdk-hook/:event` route. The SDK fires the existing Claude hooks, so events land on the existing `/hook/:event` adapter and normalize to the canonical stream already. This is the cheap half and the whole argument for the hybrid: we do **not** rebuild the SDK-message → canonical-event translation layer (the bulk of t3code's ~4,600-line adapter). We let the hooks do it, as they do today.

**Layer 4 — a `claudeSdk` strategy object.** Most methods are shared with `claude`; the diffs:

| Method (from `architecture.md#layer-4`) | `claudeSdk` |
|---|---|
| `spawnArgv(opts)` | launch a Fleet Deck **SDK runner** (a small `query()` host process), not bare `claude` |
| `resumeArgv(row)` | SDK `resume` / `continue` option, not `claude --resume` |
| `paneCommandName` | the **runner's** pane command (Option 2, committed below) — the runner renders the driven stream into the pane |
| `transcript{path,…}` | the SDK session id / stream, not `~/.claude/projects/…` (already the P4 multi-account seam) |
| `livenessPoll?` | the `query()` runtime's own liveness, not `claude agents --json` |
| `needsYouAnswer(kind)→wire` | **now genuinely supported** — routes the board's answer back through `canUseTool`'s allow/deny |
| `usageReader` | SDK `result`-message usage (`total_cost_usd`, token counts) instead of / alongside the rollout reader |

**The new control channel.** Approvals, interrupt, steer, resume are *control*, not *telemetry* — they don't belong in the canonical **event** vocabulary (which stays observe-only and provider-blind). Model them as a separate provider-control surface the strategy exposes (a natural home for P5's control skill + privilege model). The board's existing `needs-you` **display** is reused; only the **answer** path is new, and it's gated by the same permission ladder P6 consolidates (Supervised / Auto-accept-edits / Auto / Full-access → the SDK's `permissionMode`: `default` / `acceptEdits` / `bypassPermissions`).

**The runner & the terminal-authority decision (now on the critical path).** Doctrine rule 5: "the terminal stays the primary, authoritative surface." A `query()` loop is headless — there is no vendor `claude` TUI to put in the grid. When drive was an opt-in sidecar this could be deferred; as **the default** it cannot be, because it decides what every pane shows. The three candidates:

1. **Headless + board-rendered only.** The SDK session lives as a stream/card (P6 surface), no pane. Cleanest control story; **breaks rule 5** — there is no terminal at all. → the **fallback** for genuinely paneless contexts (a coordinator driving under P5), not the default.
2. **Runner-in-a-pane — the committed default.** A Fleet Deck runner renders the SDK stream *inside* a tmux pane, so the grid still shows a live pane per session. The pane shows Fleet Deck's rendering of the driven turn — plans, diffs, and the `needs-you` gate inline — rather than a scraped vendor TUI. That is not a downgrade: it is a *structured* view of the session the raw TUI can't give us, and it keeps the board a board.
3. **Observed-primary, SDK-as-control-sidecar.** Keep the real `claude` TUI authoritative and bolt SDK control alongside — the most doctrine-preserving, but it keeps *observe* primary, which is exactly the posture we're moving off of, and it's only viable if one session can be both TUI-interactive and SDK-driven at once (**doubtful**). Off the table as the default; it survives only as the shape of the observe-only floor.

**Decision: Option 2 is the default.** Rule 5 evolves with it — from "the real vendor TUI is authoritative" to "**a live Fleet-Deck-rendered pane of the driven session is authoritative.**" The grid stays the home; what fills a pane changes from a scraped TUI to a rendered stream. What's left is not *which option* but tuning *how well Option 2 renders* — latency, fidelity, the `needs-you` gate inline.

**Auth & config isolation.** The SDK runs on the user's existing **subscription OAuth** (or API key / Bedrock) — not a metered key we supply — and isolates config via **`CLAUDE_CONFIG_DIR`, never `HOME`** (overriding `HOME` breaks macOS keychain OAuth). This is the same per-instance config-home abstraction P4 already needs for multi-account pinning — the `transcript{path}`/config-home seam does double duty.

## What we build

The full path, stated plainly — no tiers, no "decide later." This is the build:

- **`claudeSdk` as the default provider** — a session gets drive+observe unless the floor is needed; existing `settings.json` hooks observed unchanged (zero new intake code).
- **Answerable `needs-you`** — board / phone / coordinator answers route back through `canUseTool` allow/deny, under the four-mode permission ladder.
- **Interrupt, mid-turn steer, and turn injection** — `Query.interrupt()` and streaming-input, exposed as first-class control verbs.
- **SDK-native resume / fork** — revive and branch sessions without the `claude --resume` pane dance.
- **The runner-in-a-pane terminal model (Option 2)** — a Fleet-Deck-rendered live pane of the driven session (plans, diffs, the `needs-you` gate inline); the authoritative surface under the evolved rule 5.
- **The live in-progress-turn stream** feeding P6's Slack-style view (`includePartialMessages`).
- **Plan capture** — `ExitPlanMode` intercepted into a first-class plan-review surface.
- **Usage from `result` messages** — `total_cost_usd` + token counts, into P4's meters.
- **Coordinator-driven turns** under P5's privilege model — the control surface is where the privilege classes bite.
- **The observe-only floor retained** — plain `claude` / `codex` observed via hooks, the automatic fail-open fallback.
- **The Codex drive tier (`codexAppServer`)** — the same shape over app-server JSON-RPC, staged behind Claude because its hooks stabilize later (see [Codex](#does-this-generalize-codex-via-the-app-server-protocol)).

### Not in scope

- **Deleting the observe-only floor.** Fail-open doctrine forbids it — a broken/drifted SDK must never dark the fleet.
- **A bespoke SDK-message → canonical-event translator.** The hybrid exists precisely so we *don't* rebuild the bulk of a ~4,600-line adapter; the hooks already emit canonical events.
- **Runtimes with neither a drive protocol nor portable hooks.** They ride the observe-only floor; that's what the floor is for.

## What this rests on

The design has a small number of load-bearing assumptions. They're grounded in official docs and in a working reference implementation (t3code), and the build confirms each in Fleet Deck's own environment as it lands — not as a timeboxed gate, just the ordinary "make it true here" of building it.

- **The linchpin: our real hooks fire under `query()`.** The Agent SDK fires filesystem `settings.json` hooks by default (`settingSources` includes user/project/local unless narrowed).¹ Fleet Deck's `command`/`http` telemetry hooks are exactly those, so a driven session POSTs the identical canonical events to `:4711` while we drive it. This is the whole reason drive keeps observe, so the build proves our specific hooks POST fail-open with the same bodies from an SDK-driven session **first** — everything else assumes it.
- **Option 2 renders well enough to be authoritative.** The terminal *model* is decided (runner-in-a-pane); what remains is tuning fidelity and latency so the rendered pane of the driven turn — deltas, diffs, the `needs-you` gate inline — is a genuine authoritative surface, not a lossy scrape.
- **Answering round-trips under every permission mode.** A board answer → `canUseTool` allow/deny → the session proceeds correctly, across Supervised / Auto-accept-edits / Auto / Full-access.
- **Resume / fork is as faithful as the pane path**, including across a daemon restart.
- **Auth isolates cleanly.** Subscription OAuth via `CLAUDE_CONFIG_DIR` (never `HOME`), multi-account keyed, no credential in `/state`, argv, or logs.

## Doctrine check (the honest cost)

This is where the doc has to be straight, because the plan crosses a line the project drew on purpose.

- **It is the *mediate* model, and it's the default — so rule 1 is amended, substantively.** Rule 1 no longer says "add providers by *observing*… not by driving it through an SDK." It now reads: ***drive+observe is how Fleet Deck runs agents; observe-only is the fail-open floor beneath it*** (see the [README](./README.md)). Name what that repositions: Fleet Deck moves from "a membrane over the tools you run" toward "the runtime that runs them, still wearing the membrane." The observe thesis survives — the hooks still fire, every card is still pure derivation — but the *default posture* flips from watching to driving. That is the project's thesis moving, and the README states it in plain text rather than slipping it in.
- **Terminal authority (rule 5) is *decided*, and this doc decides it.** With drive as the default there is no vendor TUI in the grid by default, so rule 5 evolves from "the real TUI is authoritative" to "**a live Fleet-Deck-rendered pane of the driven session is authoritative**" — the runner-in-a-pane model (Option 2). The grid stays the home; what fills a pane changes. Option 1 (headless card) remains only the fallback for paneless contexts and is the one place rule 5 genuinely lapses; the doc refuses to pretend otherwise.
- **No model calls in the core — preserved.** The daemon still makes zero model calls; the SDK runner is a spawned process, the same way P1's agents are the intelligence and the daemon stays arithmetic + SQL + git (rule 2).
- **Loopback / no phone-home — preserved.** The runner talks to the local daemon; the SDK talks to Anthropic on the user's own login, exactly as plain `claude` does today. Nothing new phones home (rule 4).
- **Plugin vs. app — deferred to a post-V1 packaging step, on purpose.** Driving the agent process is more "app" than observing one you didn't start, and it's fair to ask whether the result still ships as a plugin. We are **not resolving that here.** V1 is built for UX and DX; how it's packaged and distributed is the *last* step, after V1 lands — and plugin distribution looks very much still on the table, because the observe layer is still the user's own filesystem hooks, the login is still the user's, and the fail-open floor is still a plain `claude` in a pane, so the "app" surface is thin. The maintainer owns that call when the time comes; it is not a blocker for the work.

## Does this generalize? Codex, via the app-server protocol

Track B asked whether we can get "a similar experience using Codex." **Yes — and by a strikingly parallel mechanism.** t3code drives Codex not by scraping a CLI but over the **`codex app-server` JSON-RPC protocol** (newline-delimited JSON on stdin/stdout, one long-lived `codex app-server` child per thread: `initialize` → `initialized` → `thread/start`; turns via `turn/start`; `turn/interrupt`; native `thread/rollback`). Crucially for us, Codex has its **own config-resident hooks subsystem that fires while the protocol drives** — the exact Codex analog of "settings.json hooks fire under the SDK."²

**Control — native, in-protocol approvals (a real `canUseTool` analog).** Codex sends the host server→client JSON-RPC *requests* — `item/commandExecution/requestApproval` and `item/fileChange/requestApproval` — and **holds the JSON-RPC reply open** until the host answers (t3code parks on an Effect `Deferred`, woken by `respondToRequest` → `Deferred.succeed`; same park/await shape as the SDK's `canUseTool`). Posture is set **per turn** from a runtime-mode, not a static file — `approvalPolicy`/`sandbox`/`approvalsReviewer` ranging `untrusted`+`read-only` → `on-request`+`workspace-write` → `never`+`danger-full-access` — which maps straight onto P6's permission ladder, exactly like the SDK's `permissionMode`.

**Observe — the same hook, literally.** Codex hooks (opt-in via `[features].codex_hooks = true`) are declared in `~/.codex/hooks.json` or inline `[hooks]` in `config.toml`, and share Claude Code's **lifecycle event names** (`PreToolUse`/`PostToolUse`/`SessionStart`/`SessionEnd`/`UserPromptSubmit`/`Stop`/…) **and the same stdin-JSON payload** (`session_id`, `transcript_path`, `cwd`, `hook_event_name`, `model`, `turn_id`, `permission_mode`). Per OpenAI's docs, "a hook script written for one agent runs unchanged on the other." So a Fleet Deck observe hook is **portable across both providers** — the same daemon-POST script, two config homes.

### The symmetry

| | Claude (`claudeSdk`) | Codex (`codexAppServer`) |
|---|---|---|
| **drive transport** | Agent SDK `query()` | `codex app-server` JSON-RPC over stdio |
| **process model** | one long-lived `query()` | one long-lived `app-server` child per thread |
| **answerable approval** | `canUseTool` callback | `item/{commandExecution,fileChange}/requestApproval` (reply held open) |
| **posture** | `permissionMode` (default/acceptEdits/bypassPermissions) | per-turn `approvalPolicy`/`sandbox`/`approvalsReviewer` |
| **observe half** | `settings.json` hooks (incl. `http` POST) | `~/.codex/hooks.json` / `config.toml` `[hooks]` — **`command` only** |
| **observe hook payload** | stdin JSON | **same** stdin JSON shape + event names |
| **config isolation** | `CLAUDE_CONFIG_DIR` | `CODEX_HOME` (+ shadow-home symlink overlay for multi-account) |
| **native rollback** | via SDK resume/fork | first-class `thread/rollback` |

### The two honest asymmetries (Codex is *not* free parity)

1. **No `http` hook kind — `command` only.** Codex hooks run a shell command; there is no direct HTTP-POST hook. So the observe hook must be a `command` that curls the daemon. This is **precisely the "shell-only, opt-in" Codex hook the v1.0 plan already scoped** — Track B confirms and explains that constraint rather than contradicting it.
2. **Tool-level observe is Bash-only today.** `PreToolUse`/`PostToolUse` currently fire for the **Bash** tool only — `apply_patch`, Edit/Write/Read, and MCP tool calls don't trigger them, and `PreToolUse` can deny but not modify input. So a `command` hook alone under-covers file edits. Two fillers exist on the **drive** side, both already visible in t3code: the app-server stream itself carries `turn/diff/updated` and `item/fileChange/*` (so *driven* Codex surfaces file changes over the protocol regardless of hooks), and t3code injects its own MCP server as a tools/observe seam. Under app-server, hook activity is *also* surfaced as protocol notifications (`hook/started`/`hook/completed`) — a belt-and-suspenders observe path t3code receives but leaves unmapped (`return []`).

### Reconciliation with p1 and the seam

`p1-codex-provider.md` describes the observe-only Codex lane — plain `codex` observed via shell hooks, membrane intact. Under this stance that lane becomes Codex's **fail-open floor**, the exact role observe-only `claude` plays for `claudeSdk`. On top of it sits the `codexAppServer` **drive** tier: same Layer-3 intake (`/codex-hook/:event`), same canonical events, plus a Layer-4 strategy whose `spawnArgv` launches an app-server runner and whose control channel answers approvals / interrupts / steers. The intake-normalization plan the v1.0 review pinned as the real seam updates from "shell-only, observe-only" to "**drive by default; the shell observe hook keeps firing beneath it.**"

**The two providers don't flip to drive-default at the same speed, and pretending otherwise would be dishonest.** Claude's observe half rests on a mature, stable hook protocol, so `claudeSdk` takes drive-default now. Codex's observe half is an **experimental, opt-in subsystem with live regressions** (#17532, #21639 — see [what the Codex drive tier depends on](#what-the-codex-drive-tier-depends-on)). Driving Codex works; making drive its *default* means betting the floor on that subsystem, which isn't safe yet. So: **Codex stays observe-*default* with drive opt-in until its hooks stabilize** — same destination as Claude, deliberately later. p1 is left in place as that floor; it needs no rewrite to play the role.

### What the Codex drive tier depends on

- **Config `command` hooks firing under *app-server* drive** (not just interactive sessions), and from which declaration site. Two upstream bugs shape this: repo-local `.codex/config.toml` hooks don't fire in interactive sessions ([openai/codex#17532](https://github.com/openai/codex/issues/17532)), and a Desktop update regressed hooks entirely ([openai/codex#21639](https://github.com/openai/codex/issues/21639)). We prefer user-level `~/.codex/hooks.json` (trust-independent) and pin the `codex` build. This is exactly why Codex flips to drive-default *after* Claude.
- **Hooks firing under sandboxed/`read-only` policies**, not only `danger-full-access`.
- **A typed app-server client.** t3code ships a generated schema/client (`effect-codex-app-server`, ~1.7 MB generated schema); Fleet Deck vendors or generates an equivalent JSON-RPC client for the evolving schema. It's a real piece of the build — and with the observe-default floor beneath it, it lands on its own clock.
- **Auth / isolation:** `codex login` writes `auth.json` (API key *or* ChatGPT subscription) inside `CODEX_HOME`; adopt t3code's shadow-home overlay (symlink shared state, keep `auth.json`/`models_cache.json` private) for multi-account without duplicating history.

## Open questions

- **Confirming the linchpin in our environment.** Hooks-fire-under-SDK is documented and demonstrated in t3code; we still confirm our specific `command`/`http` hooks POST fail-open from a driven session before drive-default is the path a session gets. It's the first thing the build proves, because everything rests on it (see [what this rests on](#what-this-rests-on)).
- **SDK churn is why the floor stays, not a reason to stall.** The SDK surface (`canUseTool`, `settingSources`, streaming input) is younger than the hook protocol, so we pin the version — and when a pinned SDK breaks or drifts, sessions fall back to plain `claude` automatically and the fleet stays lit. Drive-default is safe *because* the floor is there.
- **Double-telemetry / identity.** A driven session that *also* fires hooks must not be double-counted or mis-attributed — the run-nonce identity work (`schema_version`, `provider` column) must cover the `claudeSdk` source cleanly, and must distinguish a driven session from its own fallback so a mid-session drop to the floor doesn't read as two sessions.
- **Codex flips to drive-default later than Claude.** Its observe half is an experimental, opt-in subsystem with live regressions (#17532, #21639); Claude's hook protocol is older and steadier. So `claudeSdk` is drive-default now; `codexAppServer` stays drive-opt-in over an observe-default floor until its hooks stabilize. Same destination, staged — a correctness call, not a hedge.

## Definition of done

- **Fail-open proof:** if the SDK runner dies, the SDK is absent, or the login is unavailable, the session **falls back to the observe-only floor** (plain `claude`) automatically — the fleet stays lit and the card keeps deriving. This is the proof that makes drive-default safe; it is the first gate, not the last.
- **Observe-parity proof:** a `claudeSdk` session produces the *same* canonical events as a floor session (the hooks are identical) — verified by diffing the event stream of the same task run both ways.
- **Control proof:** a board (or coordinator agent, under the P5 privilege model) answers an approval, interrupts a turn, and steers a turn — each reflected correctly in the session.
- **Terminal proof (Option 2):** the runner-in-a-pane renders the driven turn — assistant deltas, diffs, and the `needs-you` gate inline — with acceptable latency and fidelity, so the pane is a genuine authoritative surface, not a lossy scrape.
- **Determinism proof:** the daemon still makes zero model calls; the card is pure derivation from the same hook events.
- **Exposure proof:** no subscription/OAuth credential in `/state`, argv, or logs; config isolated via `CLAUDE_CONFIG_DIR`.
- **Doctrine proof:** the observe-only floor still works as fail-open; the README amendments to rule 1 (drive-default + observe-floor) and rule 5 (rendered pane authoritative) are in place and linked from this doc.

---

¹ Anthropic Agent SDK — `settingSources`, filesystem vs. programmatic hooks, `canUseTool`, streaming input, permission modes, and `CLAUDE_CONFIG_DIR`/subscription-OAuth behaviour, per the official docs: [Use Claude Code features in the SDK](https://code.claude.com/docs/en/agent-sdk/claude-code-features) (the "Hooks" and "What settingSources does not control" sections), [Intercept and control agent behavior with hooks](https://code.claude.com/docs/en/agent-sdk/hooks), and the [TypeScript SDK reference](https://code.claude.com/docs/en/agent-sdk/typescript). Control-model shape cross-checked against pingdotgg/t3code's `apps/server/src/provider/Layers/ClaudeAdapter.ts` (single long-lived `query()`, `canUseTool`↔`respondToRequest` Deferred bridge, `ExitPlanMode` capture-and-deny).

² Codex — the app-server drive model and per-thread lifecycle are cross-checked against pingdotgg/t3code's `Drivers/CodexDriver.ts`, `Layers/CodexAdapter.ts`, `Layers/CodexSessionRuntime.ts`, and the vendored `packages/effect-codex-app-server` client (JSON-RPC over stdio; `item/{commandExecution,fileChange}/requestApproval` held-open replies; per-turn `approvalPolicy`/`sandbox` from runtime-mode; `CODEX_HOME` + shadow-home overlay in `Drivers/CodexHomeLayout.ts`). The hook subsystem — event names, `command`-only handler, stdin-JSON payload (`session_id`/`transcript_path`/`cwd`/`hook_event_name`/`model`/`turn_id`/`permission_mode`), opt-in `[features].codex_hooks`, Bash-only `PreToolUse`/`PostToolUse`, and Claude-portable hook scripts — is per OpenAI's official docs: [Codex Hooks](https://developers.openai.com/codex/hooks) and [Configuration Reference](https://developers.openai.com/codex/config-reference). Stability caveats: [openai/codex#17532](https://github.com/openai/codex/issues/17532), [openai/codex#21639](https://github.com/openai/codex/issues/21639).
