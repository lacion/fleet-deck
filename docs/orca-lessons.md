# What Fleet Deck can learn from Orca

*Written 2026-08-07, revised after a full read of [onorca.dev/docs](https://www.onorca.dev/docs). Against Fleet Deck v0.22.3 and public [stablyai/orca](https://github.com/stablyai/orca).*

The companion roadmap — what we do about this — is [fleetdeck-future.md](./fleetdeck-future.md). This document is the analysis; that one is the plan. Feature claims below are grounded in Orca's own docs at the mechanism level, not the README pitch.

## What Orca is

Orca calls itself an **ADE — "Agent Development Environment."** It's a YC-backed, MIT-licensed **Electron desktop app** (macOS/Windows/Linux) with **native iOS/Android** companion apps, a headless `orca serve`, brew/AUR distribution, and ~39.5k stars. Its pitch is ours from the other side: run many coding agents in parallel, each in its own git worktree, tracked in one place — then *"compare and merge the winner."*

The README undersells it. The docs reveal Orca is really **two products stacked**:

1. **An agent-agnostic terminal IDE.** WebGL terminals with splits, a per-worktree embedded Chromium browser, a Monaco editor with autosave, a git diff viewer, and native GitHub/Linear/Jira drawers. Agent-agnostic because at bottom it is a *terminal host*: "if it runs in a terminal, it runs in Orca."
2. **A programmable automation layer** — the part worth studying. An `orca` **CLI that both humans and *agents* script**, a deterministic **multi-agent orchestration** state machine, **scheduled automations**, a **skills registry**, and **Computer Use**. This is where Orca is genuinely ahead of us.

## The core divergence: application vs. membrane — now confirmed at the mechanism level

Orca and Fleet Deck attack the same problem from opposite ends, and the docs make the difference concrete rather than rhetorical:

- **How each knows an agent's status.** Orca runs Claude Code "as a first-class terminal agent" and reads status from **a status-line hook that emits OSC terminal-title events** — "state dots." That's a thin, generic signal (it's how any terminal host would do it). Fleet Deck derives `queued → working → verifying → needs-you → idle → offline` from **actual Claude Code hook telemetry** (PreToolUse, Stop, permission events). Our status is deeper and more honest *because* we are Claude-Code-specific; Orca's is shallower *because* it is agent-agnostic. This is the whole tradeoff in one data point.
- **What each is.** Orca is an application you live inside — it owns your terminal, editor, browser, and PR flow. Fleet Deck is a membrane: the plugin's hooks make *plain `claude`* fleet-aware, it fails open if the daemon is down, its core makes zero model calls, and it binds loopback. You keep your own tools.

Neither is wrong. But it means most of Orca's surface is not "catch-up" — porting it would dissolve the membrane into an app. The lessons are the few that fit *through* the membrane, and the docs show there are more of them than the README suggested — concentrated in the programmable layer.

## Where Fleet Deck already matches or beats Orca

The honest scoreboard first. On the fundamentals we are not behind, and on several we lead — some by more than I assumed before reading the docs.

| Capability | Orca (per its docs) | Fleet Deck v0.22.3 |
|---|---|---|
| Per-agent worktree isolation | worktree per task | Batch spawn `<repo>--fd-<callsign>`; isolation **forced** for batches |
| **Fan one prompt across N agents** | **Manual** — the "race 3 agents" recipe says *"paste the same prompt into all three"* by hand | **`3x` prefix automates it** — one click → N worktrees, N branches, same task |
| Live terminal, surviving scrollback | daemon owns PTYs; warm-reattach | xterm.js over `tmux -C`; **terminal grid** shares one control client |
| Daemon owns processes past UI close | background daemon owns PTYs | daemon owns tmux; **same model** |
| Resume a dead agent | re-run in tab (only layout/scrollback restore if daemon died) | **Revive ⟲ / move-to-tmux** resume the *exact session id* with full transcript — **ahead** |
| Steer from a phone | native apps + **hosted Orca Relay** (sign-in) | Remote control → claude.ai + LAN board; **no relay to run** |
| Multiple providers | account hot-swap (credential-pointer rewrite) | Per-session **LLM gateway routing** (🛰), credential kept off `ps`/board |
| Rich file previews | Monaco + md/img/pdf/mermaid viewers | `FileViewer` (rendered markdown, binary-aware) |
| Two agents editing one file | — (not addressed) | **Conflict radar** — in-context whisper + hazard board, worktree-aware. *Unique.* |
| Agent-to-agent messaging | orchestration `send`/groups (experimental) | **Mail** — turn-boundary delivery, idle sessions woken, group targets. *Shipped._ |
| Honest derived status | OSC-title "state dots" | **Hook-derived lifecycle** — deeper signal. *Ahead.* |
| Zero install, zero wrapper, fail-open | Electron app + brew/AUR | A plugin over plain `claude`; **fail-open** |

Two of these deserve emphasis. First, **we already automate the fan-out Orca does by hand** — their celebrated "race the winner" recipe is literally *create three worktrees, paste the prompt into each, split the panes.* Our `3x find the race` is one click. Second, **our status is a deeper signal than theirs.** Orca's lead is not on launch and not on coordination. It is on the two areas below.

## Where Orca genuinely leads

Re-ranked after the deep read. The programmable layer jumped to the top; it's the part the README never mentioned and the part most worth learning from.

### 1. An agent-drivable control surface + a real orchestration lifecycle

This is the headline. Orca ships an `orca` CLI that **the agent itself scripts** — `orca terminal send/read/wait --for tui-idle`, `orca worktree create`, `orca file diff`, and an **orchestration** namespace that is a deterministic state machine, not an LLM planner:

- Nouns: **`Run`** (durable inbox), **`Task`** (`pending→ready→dispatched→completed/failed/blocked`), **`Dispatch`** (one attempt on a terminal), **`Message`**, **`Decision gate`**.
- A **coordinator** creates a Run + tasks, starts supervised workers (`worker-start --task … --agent …`), then **blocks on a FIFO inbox** (`orca orchestration check --wait --ack <id> --types worker_done,escalation,question`). Workers **escalate questions upward** (`ask --to <coordinator> --options …`) instead of prompting locally. The coordinator resolves DAG gates (`gate-create` / `gate-resolve`). Group addresses exist: `@all`, `@idle`, `@claude`, `@codex`, `@worktree:<id>`.
- Crucially, **no model is baked into the orchestrator** — "any intelligence comes from whatever agent runs the commands." It's deterministic scripting; the *coordinating agent* is the brain.

**Why this matters for us:** Fleet Deck already has every noun — mail (turn-boundary, group targets), `assign auto:` (idle-first SQL routing, zero model calls), the needs-you rail (questions/decisions as cards), the plan library (a `proposed→approved→executed` state machine). We even already expose an HTTP API (`/api/spawn`, `/command`, `/mail`, `/state`) and the `fleet-doctrine` skill *already teaches agents to `curl POST /mail` and `GET /state`.* What we lack is exactly two things Orca has: **(a) waitable, typed completions** — a coordinator can't yet "block until worker N reports done or asks a question" (our mail is fire-and-forget), and **(b) a documented agent control skill** covering spawn/assign/wait, not just mail+read. Orca's own orchestration is marked *experimental* — so this is a place we can move fast, because we already own the hard parts (derived status, real routing, worktrees).

### 2. Scheduled automations

`orca automations create --trigger weekdays --time 09:00 --prompt "Triage new issues" --provider claude --repo … --precheck "gh pr list …"`. A **scheduled prompt** runs in a worktree via a provider agent; a shell **precheck** whose non-zero exit records a *skipped* run gates it ("only run if there are open PRs"). Presets (`hourly/daily/weekdays/weekly`), cron, RRULE, timezones, `--reuse-session`, missed-run grace. **Fleet Deck has nothing here** — no way to say "every weekday morning, spawn a triage worker." Clean, deterministic, doctrine-safe gap.

### 3. The harvest half of parallelism — diff review + line annotation

We nailed the launch half (better than Orca). We have no harvest half at all. Orca's, at the mechanism level:

- **Diff viewer:** compares against the worktree's **start-from ref** by default (retargetable to any commit/branch/base), a combined diff across staged + unstaged + untracked, per file. Keys: `j/k` files, `n/p` hunks, `s` stage-hunk (visual `git add -p`), `c` comment. No model.
- **Annotate AI Diff:** gutter `+` / press `c`, markdown, comments **pin to the exact line and track across edits**. "Send to agent" **composes one line-anchored prompt with *all* comments batched** and offers a "Send notes to" menu. The stated rationale for batching is the important part: dripping comments one at a time "makes the agent swing back and forth"; one batch gives "one round of thinking, one revision pass, higher hit rate." No model interprets — Orca just *composes and delivers*.

Fleet Deck has no diff view (`WorktreesModal` knows `merged`, never shows *what changed*). For an unsupervised fleet, "read the diff and push a note back" is the most common human action and the one thing the board can't do.

### 4. Usage & rate-limit awareness (and account hot-swap)

The mechanism is doctrine-perfect and we should copy it almost verbatim: Orca **reads the local usage state each agent already writes under `~/.claude` / `~/.codex`** — "No API calls, no extra auth." It surfaces the **5-hour / daily / weekly / Fable-weekly reset windows**, a **warning chip at 80%** of a limit, per-provider bars **sorted tightest-limit-first**, compact/detailed modes, and cost from a **local price table** ("inferred pricing"). Honest caveat baked in: "numbers update when the agent writes, not in real time."

Account **hot-swap** "rewrites the active credential pointer, it does not re-authenticate" — it points at pre-existing auth under `~/.claude`, only affects *new* sessions, and works "identically" for Claude and Codex. For someone running a dozen sessions, the rate-limit reset is the binding constraint of the day, and our board shows every session's *status* but not that number.

### 5. Quick-open / jump palette

Two tools: `Cmd-P` (file search in the current worktree, recency + match score) and **`Cmd-J` Worktree Jump Palette** — jump across *every worktree and tab*, match cached PR/MR numbers (`#123` / `!123`), and a **Create-worktree row** when the query matches nothing (Enter = jump, Shift-Enter = open in split). At three sessions unnecessary; at fifteen it's how the board scales.

### 6. Task → worktree (GitHub / Linear / Jira drawers)

Orca's issue drawers are read/**write** (open PRs, comment on threads, enable auto-merge, edit issues). "Create a worktree from an issue/PR opens the composer, **prefills the task name and links the issue**." A neat variant: **"fix broken checks" hands the failing check names + links to an agent.** We already fold a Jira key from the branch into the callsign — identity is done — but we can't browse issues or spawn *from* one.

### 7. Design Mode

Click an element in Orca's per-worktree browser and it captures **outerHTML + a small neighborhood, computed CSS, a cropped screenshot, and the source file/line** (if a dev source-map exists), ships all of it "into the active agent terminal as one attachment," you add the instruction, the agent edits, Orca hot-reloads, you click again to verify. We already ship the lightweight cousin — **Ctrl+V pastes a screenshot**. Design Mode is the same instinct with structure attached; a faithful port needs an embedded browser we don't have.

### 8. SSH worktrees / hibernation — noted, not chased

**SSH:** agent + worktree live on the remote host; Orca installs "a small relay," syncs *file events* so editor/diff feel local, remote terminals need `node-pty` build tools, leased PTYs survive app close, auto-reconnect, port-forwarding via `/proc/net/tcp` scan. Rich but heavy; our answer is the Coder/LAN topology ([`CODER.md`](./CODER.md)). **Hibernation:** stop long-idle *finished* agents' PTYs to reclaim memory, auto-resume on foreground via `claude --resume`. A memory-reclaim idea; our terminal grid already shares one tmux control client, so we feel the pane-count cost less.

## What Orca does that Fleet Deck should *not*

Each is a reasonable choice for an *application*. Adopting it makes us one.

| Orca feature | Why we reject it | Doctrine failed |
|---|---|---|
| Electron app + Monaco editor + file tree | Our value is you keep your own terminal/editor; the board is a no-native-deps membrane that fails open. | plugin-not-app |
| **Agent-agnostic core** | Our honest derived status *is* Claude Code hook telemetry; Orca proves the alternative is thin OSC "state dots." Going generic makes every card less honest. | erodes the core value |
| **Computer Use** (`orca computer click/set-value/type-text` over the OS accessibility tree, Screen-Recording perms) | Large OS-level security surface, not a fleet-coordination concern. | scope |
| Write-heavy GitHub ownership (auto-merge, PR threads in-app) | That's owning the PR workflow — application territory. Read + spawn-from-issue is the membrane-sized slice. | plugin-not-app |
| Native mobile apps + **hosted Orca Relay** (sign-in required) | We steer from a phone via claude.ai handoff + LAN board, with no relay to run and nothing to phone home to. | loopback / no-phone-home |
| Any "AI picks the winner / summarizes the diff" in the default path | The core stays deterministic and free; the *coordinating agent* is the only intelligence. LLM assists, if ever, are opt-in and off by default. | no-model-calls |

## One-line takeaway

The deep read didn't move the fundamentals — we still win coordination and *out-automate Orca on launch* — but it relocated Orca's real lead. It isn't the shiny app features; it's the **programmable layer**: an agent-drivable control surface with a real orchestration lifecycle, scheduled automations, and the harvest tools (diff + batched annotation) plus operational awareness (usage/rate-limits). Every one of those fits through our membrane, and we already own the hard prerequisites for the biggest of them. The plan is in [fleetdeck-future.md](./fleetdeck-future.md).
