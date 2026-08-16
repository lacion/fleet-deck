# The future of Fleet Deck — what to adopt from Orca

*Written 2026-08-07, revised after a full mechanism-level read of [onorca.dev/docs](https://www.onorca.dev/docs). Against v0.22.3. The analysis this rests on is [orca-lessons.md](./orca-lessons.md) — read it first.*

A roadmap, not a promise. Every item is filtered through the doctrine test, sized S/M/L (matching [`ux-feedback-plan.md`](./ux-feedback-plan.md)), and sorted **adopt / adapt / reject**. The deep read confirmed the framing and sharpened it: we already win coordination and *out-automate Orca on launch* (our `3x` batch does the fan-out Orca makes you do by hand). The real lead to chase is Orca's **programmable layer** — an agent-drivable control surface with a real orchestration lifecycle, scheduled automations, and the harvest tools (diff + batched annotation) plus usage/rate-limit awareness. The good news from the deep read: we already own the hard prerequisites for the biggest item.

## The doctrine test

Every candidate must pass all five, or it's the wrong feature for *this* tool:

1. **No wrapper.** Works over plain `claude` + the plugin's hooks. No launcher, no CLI fork.
2. **Fail-open.** Daemon down → sessions run exactly as before. Nothing new is load-bearing.
3. **No model calls in the core.** The default path is deterministic. Any LLM assist is opt-in, off by default, clearly marked. (Corollary from Orca: when a *coordinating agent* drives the fleet, the agent is the intelligence — the daemon stays deterministic. That passes.)
4. **Loopback / no phone-home.** New surfaces respect the bind/token model; nothing routes through a hosted relay.
5. **Plugin, not app.** No native deps, no Electron, no owning the editor. State stays in `node:sqlite`; the board stays a browser page.

Items that fail land in **reject**, with the reason.

---

## Tier 0 — the big idea: make the fleet agent-drivable

This is the headline lesson from the deep read, and the one place adopting Orca's idea could put us *ahead* of it — because Orca's version is experimental and we already own the hard parts.

**What Orca has:** an `orca` CLI the *agent itself* scripts, plus an **orchestration state machine** — `Run → Task → Dispatch → Decision-gate`, a coordinator that starts supervised workers and **blocks on a FIFO inbox** for typed completions (`check --wait --ack --types worker_done,escalation,question`), workers that **escalate questions upward** instead of prompting locally, and group addresses (`@idle`, `@claude`, `@worktree:<id>`). No model in the orchestrator — the coordinating agent is the brain.

**What we already have:** every noun. Mail (turn-boundary delivery, group targets, idle-waking). `assign auto:` (idle-first, least-buried routing via SQL, **zero model calls**). The needs-you rail (questions/decisions as answerable cards). The plan library (a `proposed→approved→executed` state machine). An HTTP API (`/api/spawn`, `/command`, `/mail`, `/state`). And the `fleet-doctrine` skill **already teaches agents to `curl POST /mail` and `GET /state`.** The plumbing and the agent-facing pattern both exist.

**The gap is exactly two things:**

#### T0.1 Waitable, typed completions *(M)*
Today our mail is fire-and-forget at a turn boundary — a coordinator session can send work but cannot **block until a worker reports done or asks a question**. Add a minimal, typed completion channel: a worker can post `done` / `blocked` / `question` against a task id, and a coordinator can **long-poll** for the next unacked one (`GET /orchestration/check?wait=…&types=…`, ack by id). This is the one primitive Orca has that we don't, and it's what turns "many independent agents" into "a directed team." Deterministic, SQLite-backed, no model. ✅

#### T0.2 A documented agent control skill *(S, on top of T0.1)*
Extend `fleet-doctrine` (or ship a sibling skill) from "mail + read state" to the full deterministic control surface a coordinator session needs: **query the roster** (`/state`), **spawn a worker** (`/api/spawn`), **assign/route** (`/command assign`), **send/broadcast** (`/mail` with group targets), and **wait on completions** (T0.1). Follow Orca's anti-drift trick — the skill is a thin stub that points at a live, versioned reference the daemon serves, so documented flags can't rot against the build. The **guardrail already exists**: spawn has the red asks-twice unsupervised gate, and mail-injection has the four sanctioned-keystroke rule. A coordinating agent inherits both.

> The result: one Claude session can act as a coordinator — fan work across idle workers, block on their results, gate decisions — using our *existing* deterministic core. We'd have Orca's orchestration shape with our deeper derived status and automated fan-out underneath it. Ship T0.1 first (it's useful to a human-driven board too — "wake me when this worker is done"), then T0.2 to expose it.

---

## Adopt — fits the membrane cleanly

### Tier 1 — automations, the harvest arc, and the number that governs the day

#### A1. Scheduled automations *(M)*
Orca: `orca automations create --trigger weekdays --time 09:00 --prompt "…" --provider claude --precheck "gh pr list …"`. A scheduled prompt runs in a worktree; a shell **precheck** whose non-zero exit records a *skipped* run gates it. We have **nothing** here. The daemon already runs and already spawns workers — add a small scheduler table (cron/preset + prompt + repo + optional precheck) that fires a spawn on a schedule. "Every weekday 9am, spawn a triage worker in repo X, but only if `gh pr list` is non-empty." Deterministic, fail-open (scheduler absent → nothing fires). ✅

#### A2. Diff review on the board *(M)*
The harvest half we completely lack. A read-only diff view per card/worktree: a daemon route runs `git diff` against **the spawn's recorded start-from/base ref** (we already record it) — combined across staged + unstaged + untracked — and streams a unified diff the board renders in the existing `FileViewer` surface. Optional later: hunk staging (`git add -p` visual), the `j/k` files / `n/p` hunks navigation Orca uses. No new heavy dep, deterministic, button simply absent if the route is. ✅

#### A3. Line annotation → *one batched* mail back to the agent *(S, on top of A2)*
The piece that turns the board from a monitor into a review station. Click a diff line, type a note; the note is **line-anchored**. The critical design lesson from Orca's docs: **batch every comment into ONE composed mail — do not drip.** Their stated rationale is that one-at-a-time "makes the agent swing back and forth"; a single batch gives "one round of thinking, one revision pass, higher hit rate." We already own the delivery channel (mail: turn-boundary-safe, wakes idle sessions), so a review batch is just a well-formatted message: `re: src/foo.mjs — L40: this drops the error; L88: needs a guard`. Comments stay pinned so you can verify after the revision. Near-free once A2 exists. ✅

#### A4. Usage & rate-limit awareness *(M)*
Copy Orca's mechanism almost verbatim — it's doctrine-perfect: **read the local usage state Claude Code already writes under `~/.claude`** (no API, no auth). Surface the **5-hour / daily / weekly** reset windows, a **warning chip at 80%** of a limit, per-session and fleet-total burn, sorted **tightest-window-first**, in a small header meter (we have `Sparkline.jsx`). Cost, if shown, from a **local price table** ("inferred pricing"), never a live bill. Bake in Orca's honest caveat — *"numbers update when the agent writes, not in real time."* For our real user the rate-limit reset is the binding constraint of the whole day, and the board is currently blind to it. **Version-resilience is the risk:** pin what we read under `~/.claude`, treat absence as "unknown," never as zero, so a CLI upgrade degrades the meter instead of crashing the board. ✅

### Tier 2 — friction at fifteen sessions

#### A5. A worktree / card jump palette *(M)*
Orca's `Cmd-J`: jump across every worktree and tab, match cached PR/MR numbers, offer a create row when nothing matches. Our analog: `Cmd/Ctrl-K` over **cards** — jump to a callsign, open its terminal, prefill Compose to it, match by callsign / ticket / repo. Pure client-side over the `/state` we already broadcast. `FileViewer` already covers file search, so this is specifically the *card/worktree* jump. Unnecessary at three sessions; at fifteen it's how the board scales with the fleet. ✅

#### A6. Spawn-from-issue *(M)*
Identity is already done — we fold a Jira key from the branch into the callsign. The extension: a read-only issue list (the user's own `gh` / a Jira token they provide, same credential discipline as gateway routing — never onto `/state` or a command line) and a **"spawn a worker for this issue"** button that prefills the batch-spawn form with the issue title/body and names the worktree ticket-first (`<repo>--fd-PROJ-123-<animal>`, which we already do). A concrete Orca variant worth stealing: **"fix broken checks" — hand an agent the failing CI check names + links.** This is batch spawn with a nicer front door, not a new subsystem. ✅

### Tier 3 — prototype, not commitment

#### A7. Design Mode, adapted *(L)* — we already ship the lightweight cousin (Ctrl+V screenshot paste). Orca's richer capture (outerHTML + computed CSS + cropped screenshot + source file/line, as one attachment) needs an embedded browser we don't have and don't want in the core. If pursued, an **optional, out-of-core** helper reusing the existing server-side image-upload path — never a daemon dependency.

#### A8. Remote spawn *(L)* — "spawn *this* agent on *that* host." Orca's SSH path is genuinely rich (remote relay, file-event sync, leased PTYs, port-forward) and genuinely heavy. Our answer is the Coder/LAN topology ([`CODER.md`](./CODER.md)). Defer until Tier 0–2 land; revisit only if Coder proves insufficient.

---

## Adapt with care

- **Hibernation *(S–M)*.** Orca stops long-idle *finished* agents' PTYs to reclaim memory, auto-resuming via `claude --resume` on foreground. A dozen idle Claude processes do hold memory. Our terminal grid already shares one tmux control client, so the pressure is lower — but a "sleep idle finished panes, revive on click" mode is a small, doctrine-safe reuse of the revive machinery we already have. Demand-driven.
- **Agent-authored status line *(S)*.** Orca's "worktree checkpoints" turned out to be a free-text `--comment` an agent writes to say what it's doing now (`orca worktree set --comment "reproduced the bug; testing fix"`). We *derive* status from hooks (deeper), but we have no *agent-authored* one-liner. Letting a worker post a short "current focus" that shows on its card — via the mail/command surface it already has — is a cheap, honest addition.
- **The agent-agnostic middle path *(L, speculative)*.** Full agent-agnosticism is a **reject** (it would make derived status dishonest — Orca proves the alternative is thin OSC "state dots"). But the tmux/terminal-grid/revive layer is already agent-neutral; only status derivation is Claude-specific. A non-Claude pane could appear as a **dumb card** — live terminal, revive, mail-to-pane, status shown as `unknown` — *without* polluting the derived-status core. On record, not scheduled.
- **Account hot-swap *(M)*.** Orca's is a credential-pointer rewrite over pre-existing `~/.claude` auth, affecting only new sessions. We already pin per-session identity via env at spawn (see gateway routing) — pinning a spawn to a specific `~/.claude` home is the same shape. More invasive than the read-only usage meter (A4); pair it with A4 only if multi-account demand is real.

---

## Reject — the wrong direction for us

Each is a fine choice for an *application*. Adopting it turns Fleet Deck into one.

| Orca feature | Why we reject it | Doctrine failed |
|---|---|---|
| Electron app + Monaco editor / file tree | You keep your own terminal/editor; the board is a no-native-deps membrane that fails open. | #5 plugin-not-app |
| Agent-agnostic *core* | Derived honest status *is* Claude hook telemetry; the generic alternative is thin OSC "state dots." (Middle path above is the only safe slice.) | erodes core value |
| Computer Use (drive the OS via accessibility trees, Screen-Recording perms) | Large OS-level security surface; not a fleet-coordination concern. | scope |
| Write-heavy GitHub ownership (in-app auto-merge, PR threads) | Owning the PR workflow is application territory; read + spawn-from-issue is the membrane-sized slice. | #5 |
| Native mobile apps + hosted Orca Relay (sign-in) | We steer from a phone via claude.ai handoff + LAN board — no relay to run, nothing to phone home to. | #4 |
| AI picks the winner / summarizes the diff (default path) | The core stays deterministic and free; the coordinating agent is the only intelligence. | #3 |

---

## Suggested sequencing

1. **T0.1 waitable completions** — the one orchestration primitive we lack; useful even to a human-driven board.
2. **A2 diff review** → **A3 batched annotation** — unlocks the harvest arc; A3 is near-free on A2.
3. **A4 usage & rate-limit meter** — highest day-to-day value, independent of the above; can run in parallel.
4. **T0.2 agent control skill** — expose T0.1 + spawn/assign/mail as a coordinator surface once completions exist.
5. **A1 automations** — scheduled triage/review, once there's a review loop worth scheduling into.
6. **A5 jump palette**, **A6 spawn-from-issue** — quality-of-life at scale.
7. **Adapt items / A7 / A8** — prototypes, demand-driven, after the above.

## Validation each item must clear

- **Fail-open proof:** kill the daemon mid-feature; every existing session runs unchanged and the new UI degrades to *absent*, never broken.
- **Determinism proof:** no "adopt" item makes a model call on its default path — including T0. The coordinating *agent* is the intelligence; the daemon is arithmetic and SQL. Grep the diff to confirm.
- **Exposure proof:** no new route leaks a credential into `/state` (broadcast to every board, phones included) or onto a process command line. The gateway rules (`token_set: true` and nothing else; base URL refuses credentials) are the bar.
- **Version resilience:** anything reading Claude Code internals (A4 especially, and any usage read) pins what it reads and treats absence as "unknown," so a CLI upgrade degrades the feature instead of crashing the board.
