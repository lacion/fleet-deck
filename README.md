<p align="center">
  <img src="docs/assets/banner.png" alt="The Fleet Deck crew at their stations. This is a fully accurate depiction of the development team." width="100%">
</p>

# Fleet Deck ⚡

**Mission control for every Claude Code session on your machine.**

You know how it goes. You start one Claude Code session. Then one in a worktree, because you're organized. Then a third one "just to look something up," and a fourth because the second one seemed stuck. It is now 6 PM, two of them are editing `util.js` at the same time, one has been silently waiting forty minutes for you to approve `rm -rf node_modules`, and you honestly could not say which terminal tab is which.

Fleet Deck is the air-traffic control tower for that situation.

<p align="center">
  <img src="docs/assets/fleet-deck-demo.gif" alt="The board in action: sessions working, a conflict flaring, a permission answered from the rail, a plan getting captured." width="100%">
</p>

Every session on the machine appears on one local board — **http://127.0.0.1:4711** — with a callsign (`falcon-a3f2`, `otter-91c4`... yes, the banner is literally the roster), a live column derived from what it's *actually doing*, and a mailbox. Sessions get whispered at when they're about to trample each other's files. Questions land in an amber rail you can answer from across the room. And when you're feeling ambitious, the board will spawn new workers, route tasks to whoever's idle, and file their plans in a library.

## What it does

- **See everything.** `queued → working → verifying → needs-you → idle → offline`, derived from hook telemetry — never self-reported, because sessions, like people, believe they are almost done.
- **Conflict radar.** Two sessions touching the same file within 30 minutes get warned *in context* ("coordinate, don't clobber"), the board flashes hazard-red, and yes, it's worktree-aware — the same file edited in two worktrees of one repo is a merge conflict introducing itself early.
- **Needs-you rail.** Permission prompts, multiple-choice questions, MCP forms, and trailing "should I use bcrypt or argon2?" questions all become cards you answer from the board. The terminal never even asks — it just prints `⎿ Allowed by PermissionRequest hook` and carries on, slightly smug.
- **Mail with honest latency.** Message any session, a whole repo, or everyone. Delivered at the next turn boundary — idle sessions usually wake within seconds (a small watcher taps them on the shoulder). The board never promises "instant," because we don't lie to you, even about milliseconds.
- **An orchestrator with no brain, on purpose.** `assign auto: fix the flaky test` picks the best candidate — idle first, least buried, right repo — with a SQL query, not a model call. The core makes **zero model calls**. Your tokens are yours.
- **A dynamic fleet.** The `+ Spawn` button starts a fresh interactive `claude` in a daemon-owned tmux window. Watch it work on the board, attach to the pane if you're nostalgic for terminals, kill it when it's done (there's a confirm; we've all misclicked).
- **A plan library.** Spawn a planner in plan mode. Its plan lands on the board as a rendered card *before* it can act. Approve it, or capture it to the library and release the planner. Later, execute the plan with your own custom instructions — optionally in unsupervised mode, which is behind a red two-step checkbox that says exactly what it does.

## Install

```bash
claude plugin marketplace add /path/to/fleetdeck   # the repo is its own marketplace
claude plugin install fleetdeck@fleetdeck
```

That's it. Your next `claude` — any terminal, any repo, no wrapper, no launcher, no ritual — brings the fleet up and appears on the board at **http://127.0.0.1:4711**. Type `/fleet` in any session for a live summary.

**Team sharing:** push this repo to your git host, then everyone runs:

```bash
claude plugin marketplace add your-org/fleetdeck
claude plugin install fleetdeck@fleetdeck
```

## The 60-second tour

1. Install. Open the board. Launch a `claude` anywhere — a card appears in about one second.
2. Launch a second one in the same repo and have them both touch the same file. Watch the hazard ripple. Read the whisper each session receives. Feel seen.
3. Compose → ORCHESTRATOR → `assign auto: add input validation to the signup form`. The daemon picks a session; if it's idle, it wakes up and gets to work.
4. Nobody available? The board offers a **spawn a session for this** button. One click, one new worker, prefilled with the task.
5. Spawn a planner (`permission mode: plan`), give it something gnarly, and capture its plan. Execute it later with an unsupervised worker while you make coffee. The coffee is not optional; you're a fleet commander now.

## How it works

```
 terminal 1..n : plain `claude` + this plugin's hooks
      │  events (http hooks, fail-open)      ▲ whispers, blocks, decisions
      ▼                                      │ (hook responses)
   fleetd — one Node daemon, 127.0.0.1 only, SQLite state
      ▲                                      │ WebSocket push
      └────────── the board (React) ─────────┘
```

- **Zero wrapper.** The plugin's hooks make plain `claude` fleet-aware. The first session's SessionStart hook elects and launches the daemon; the port bind *is* the election.
- **Fail open, always.** Daemon down? Hooks time out silently and your sessions run exactly as before. Fleet Deck is not load-bearing; it's a tower, not a runway.
- **Zero model calls in the core.** Telemetry, conflict detection, routing, question relay: deterministic code. The only model cost added to your sessions is a ~100-token roster brief and the occasional whisper.
- **127.0.0.1 only.** No auth story, therefore no non-local bind. Your fleet does not phone home; the daemon cannot spell `0.0.0.0`.

## The fine print (read this bit)

- **Spawned sessions are real billed Claude sessions.** The spawn form says so, the cap defaults to 5, and nothing ever spawns without a human click. `assign auto` routes to *existing* sessions only.
- **Unsupervised mode means unsupervised.** `--dangerously-skip-permissions` workers never produce permission cards. The checkbox is red and asks twice. Pair it with the fresh-worktree option and sleep better.
- **The permission relay is interactive-only.** Headless `claude -p` sessions deny permission-needing tools without consulting hooks — that's CLI behavior, not ours. Spawned fleet workers are interactive (in tmux) precisely so their prompts reach your board.
- **Version pin: Claude Code CLI 2.1.206.** Fleet Deck leans on a couple of behaviors the docs don't mention (they work; we checked, repeatedly, at some cost to our dignity). A guard test fails loudly if a CLI update drops them. Contract tests replay recorded hook payloads so schema drift is caught in CI, not in your fleet.
- **Ports:** `FLEETDECK_PORT` / `FLEETDECK_HOME` env vars; hooks are pinned to 4711 by default, so a truly separate fleet needs the port swapped in a copy of `hooks/hooks.json` too. On multi-user machines give each OS user their own port — TCP ports are shared per machine, and you probably don't want to co-manage a fleet with whoever else is on that box.

## Development

```bash
npm install
node --test --test-concurrency=1     # 102 contract tests against a real daemon
npm run bundle                       # rebundle the daemon after touching scripts/fleetd/
npm run build:board                  # rebuild the React board into board-dist/
```

The `demo/` scripts are live acceptance gates that start *real* Claude sessions (i.e., they cost money): `run-smoke.sh` (two sessions colliding on purpose), `run-accept-spawn.sh` (spawn → assign → board-approved permission → kill), `run-accept-plan.sh` (plan → capture → unsupervised execution). Run them deliberately, not casually.

## Credits

Designed and built by a fleet of Claude agents coordinating through contracts, reviewed by Codex, supervised by one human with a board — which is to say: Fleet Deck was built the way Fleet Deck works. The repo is its own proof of concept.

Board design: "Console" direction — ink navy, amber means *yours-to-act*, IBM Plex Mono for anything that's data. The light theme exists and is lovely; the dark theme is correct.

## License

MIT. Fly safe. ⚡
