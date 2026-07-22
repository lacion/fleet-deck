<p align="center">
  <img src="docs/assets/banner.png" alt="The Fleet Deck crew at their stations." width="100%">
</p>

# Fleet Deck ⚡

[![CI](https://github.com/lacion/fleet-deck/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/lacion/fleet-deck/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Node ≥ 22.5](https://img.shields.io/badge/node-%E2%89%A5%2022.5-brightgreen.svg)](https://nodejs.org)

**One board for every Claude Code session on your machine.**

Run more than two or three Claude Code sessions and you lose track of them: which terminal is which, which one is blocked on a permission prompt, and which two are editing the same file. Fleet Deck puts all of them on one local board at **http://127.0.0.1:4711**, with a callsign, a live status column, and a mailbox each.

<p align="center">
  <img src="docs/assets/fleet-deck-demo.gif" alt="The board: four agents working one repo in separate worktrees, cards moving between columns as each turn lands, and the conflict radar flaring when two of them touch the same file." width="100%">
</p>

## What it does

- **Live status, derived not self-reported.** `queued → working → verifying → needs-you → idle → offline`, computed from hook telemetry. The model badge is read from the session transcript, so it follows a mid-session `/model` switch instead of freezing on whatever the session launched with.
- **Conflict radar.** Two sessions touching the same file within 30 minutes get warned in context, and the board flashes hazard-red. Worktree-aware: the same file edited in two worktrees of one repo is a merge conflict surfacing early.
- **Needs-you rail.** Permission prompts, multiple-choice questions, MCP forms and trailing questions become cards you answer from the board. The terminal prints `⎿ Allowed by PermissionRequest hook` and continues.
- **Mail between sessions.** Message one session, a repo, or everyone. Delivered at the next turn boundary; idle sessions are woken by a small watcher, usually within seconds.
- **Routing without a model call.** `assign auto: fix the flaky test` picks a candidate — idle first, least buried, right repo — with a SQL query. The core makes zero model calls.
- **Spawning.** `+ Spawn` starts a fresh interactive `claude` in a daemon-owned tmux window. Watch it on the board, attach to the pane, or kill it.
- **A live terminal in the browser.** Click a spawned session's tmux chip and its pane opens as an xterm.js terminal bridged over WebSocket to `tmux -C attach`. No PTY, no native deps. Typing sends real keystrokes to the agent's pane. **Shift+Enter inserts a newline** rather than submitting — the board is the emulator, so it sends the `ESC CR` that Claude Code's keybinding expects, with nothing to configure. **Ctrl+V pastes a screenshot**: the board uploads the image, writes it server-side, and types the path into the composer; you press Enter yourself. (PNG/JPEG/GIF/WebP, ≤10 MB, written owner-only under `FLEETDECK_HOME`, pruned after 24 h or 50 files.)
- **A terminal grid.** `▦ Terminals` opens every live agent at once. All tiles stream; exactly one accepts input — the focused tile wears an amber ring, and every other tile is stdin-disabled at the terminal itself, not filtered on the way out. The grid shares one tmux control client, so the tenth tile costs a WebSocket, not a tenth tmux process.
- **Revive.** Worktrees and transcripts outlive panes. One `⟲` click resumes a dead agent in its own worktree with full history — same callsign, same card. Whole columns at a time.
- **Remote control.** Hand a session to claude.ai and drive it from a phone. The card grows a 📱 chip named after the callsign.
- **A plan library.** Spawn a planner in plan mode; its plan lands on the board as a rendered card before it can act. Approve it, or capture it and release the planner. Execute it later with your own instructions.

<p align="center">
  <img src="docs/assets/live-terminal.gif" alt="Opening a live agent's tmux pane in the board's terminal modal and typing into the running session." width="100%">
</p>

## Install

```bash
claude plugin marketplace add lacion/fleet-deck   # the repo is its own marketplace
claude plugin install fleetdeck@fleetdeck
```

Your next `claude` — any terminal, any repo, no wrapper — brings the fleet up and appears on the board. Type `/fleet` in any session for a live summary.

> A marketplace install tracks the repo's **default branch**, not a pinned release: every push to `main` runs at your next SessionStart. The pipeline is gated (CODEOWNERS, hook-integrity CI, human-approved npm publishes), but if you want releases only, use the npm channel — `npm i -g fleetdeck` for [standalone mode](#standalone-mode) — or pin your marketplace clone to a tag.

**Requirements.** Node 22.5+. Nothing to `npm install`: the daemon ships as one bundled file and keeps state in Node's built-in `node:sqlite`. Add **tmux 3.4+** to spawn workers or open their panes in the browser — Fleet Deck relies on 3.4's no-start probe to avoid attaching to a replacement server. Everything else works without tmux. Linux, WSL2 and macOS; Windows-native is untested.

<details>
<summary>Working on Fleet Deck itself, or running from a fork</summary>

```bash
claude plugin marketplace add /path/to/fleet-deck   # a local clone
claude plugin marketplace add your-org/your-fork    # or your fork
claude plugin install fleetdeck@fleetdeck
```

After changing anything under `scripts/`, run `npm run bundle` — the daemon runs the bundle, not the source. After changing the board, run `npm run build` in `board/`. Then restart the daemon, or bump the version and let the upgrade takeover do it.
</details>

## Quick start

1. Open the board and launch a `claude` anywhere. A card appears in about a second.
2. Launch a second session in the same repo and have both touch the same file. Watch the hazard ripple and read the whisper each session receives.
3. Compose → ORCHESTRATOR → `assign auto: add input validation to the signup form`. The daemon picks a session and wakes it if idle.
4. If nobody is available, the board offers **spawn a session for this** — one click, prefilled with the task.
5. Tick **batch** on the spawn form and paste three tasks, one per line: three agents, three worktrees, three branches. Then hit **▦ Terminals**.
6. Spawn a planner (`permission mode: plan`), capture its plan, and execute it later with an unsupervised worker.

## Batch spawn

Tick **batch** and the prompt box becomes a task list — one agent per line:

```
fix the flaky worktree test
update the README install section
3x find the race in the spawn path
```

That is five agents in one click. Each gets **its own git worktree** (`<repo>--fd-<callsign>`, or `<repo>--fd-<TICKET>-<animal>` when the source branch carries a Jira ticket) on **its own branch** (`fd/<callsign>` / `fd/<TICKET>-<animal>`), so they share a repo without standing on each other's edits — isolation is forced for a batch, not offered. The `3x` prefix runs a line several times, which is how you race independent attempts at one bug.

Before anything launches the form shows the exact list and count. **That preview is the guardrail**: there is deliberately no cap on how many agents may be live. It is your machine and your token budget.

<p align="center">
  <img src="docs/assets/terminal-grid.gif" alt="Four agents in one repo, each in its own worktree, all four live terminals open in a grid." width="100%">
</p>

## Architecture

```
 terminal 1..n : plain `claude` + this plugin's hooks
      │  events (http hooks, fail-open)      ▲ whispers, blocks, decisions
      ▼                                      │ (hook responses)
   fleetd — one Node daemon, loopback by default, SQLite state
      ▲                                      │ WebSocket push
      └────────── the board (React) ─────────┘
```

- **No wrapper.** The plugin's hooks make plain `claude` fleet-aware. The first session's SessionStart hook elects and launches the daemon; the port bind *is* the election.
- **Fail open.** If the daemon is down, hooks time out silently and sessions run exactly as before. Fleet Deck is not load-bearing.
- **No model calls in the core.** Telemetry, conflict detection, routing and question relay are deterministic code. The only added model cost is a ~100-token roster brief and the occasional whisper.
- **Loopback by default.** The daemon binds `127.0.0.1`. `FLEETDECK_BIND=0.0.0.0` opens it to your network, and a token then becomes mandatory ([LAN mode](#lan-mode)). Either way it does not phone home.

## Revive

Panes die; the work does not. Each agent's git worktree and Claude transcript stay on disk, and the daemon knows both paths.

An OFFLINE card whose worktree **and** transcript both survive grows a **⟲ revive** chip. Clicking it relaunches that spawn in the same worktree with `claude --resume <session-id>` (plus `--dangerously-skip-permissions` if it had it). Because `--resume` keeps the session id, the revived agent's first hook un-tombstones the card it already had. When several cards qualify, the OFFLINE column head offers **⟲ Revive all (N)**.

It refuses rather than guessing:

- **409** — the pane is still alive, or that session already has a live spawn.
- **410** — the worktree or the transcript is gone; there is nothing to resume into.

**If the tmux server itself dies**, a watchdog notices. That is a fleet-wide event rather than a per-pane one, so it is handled as one: the board says so once, settles the affected cards to offline, and starts a fresh fleet session so the next revive has somewhere to land. Settling the cards is what makes ⟲ revive available instead of a 409. The watchdog never relaunches an agent on its own — resuming one spends money, so that stays a human click.

<p align="center">
  <img src="docs/assets/revive.gif" alt="An offline agent revived from the board, keeping its callsign and conversation history." width="100%">
</p>

## Move to tmux: adopt a session the board didn't spawn

Sessions you started yourself appear on the board via hooks, but the board doesn't own their pane: no terminal chip, no revive, no mail-to-pane. **⇥ move to tmux** resumes the session into a board-owned tmux window (`claude --resume <session-id>` — same session id, same card, full history), after which it is a first-class fleet worker.

Two entry states:

- **The session already ended** (card in OFFLINE with a hook-proven end): the move happens immediately.
- **The session is live in your terminal:** two processes cannot drive one conversation, so the click **arms** the card. Exit the session whenever you like and the daemon resumes it into a managed pane within a second. The chip reads `⧗ armed — exit CLI to move`; clicking again disarms, and a forgotten arm expires after ~30 minutes (`FLEETDECK_ADOPT_ARM_MS`). A `/clear` is not an exit, and the arm survives it.

The arm is **durable intent, consumed once**: it lives in SQLite, not a timer. Disarm any time, including while a move is settling — the cancel wins. If the session comes back to life first, the move cancels rather than ambushing your next exit. If the daemon dies in that window, the next boot's sweep finishes the move.

The dialog offers the same red, asks-twice **unsupervised** gate as the Spawn form. Left unchecked, the resumed session's permission prompts land on the board.

It refuses like revive:

- **409, "board-owned"** — the session already has a spawn lineage, alive or dead. **⟲ revive** is that button; a second lineage would fight the first over the tmux window and worktree.
- **409, "no hook-proven end"** — the card is offline, but nothing proved the CLI exited: retention presumed it dead after 3 h of silence, the agents registry stopped reporting it, or it predates 0.7.0. A session that is quietly alive would be resumed into a *second billed session*, so absence of proof is not proof. Arm it instead.
- **410** — the working directory or transcript is gone.

Remote (claude.ai/code) sessions can be adopted, but resuming a web session's transcript locally is untested; the transcript check is the gate, not the session's origin.

## Remote control (`/rc`)

Claude Code can hand a session to claude.ai. Fleet Deck names the session after its callsign, so what you find in the claude.ai session list matches your board.

- **From birth.** Tick **📱 remote control** on the Spawn form; the worker launches with `--remote-control <callsign>`.
- **On a running agent.** A live, idle spawned agent shows **📱 enable remote**. The daemon types `/rc <callsign>` into its pane, waits for the TUI to render, and harvests the `https://claude.ai/code/session_…` link from the pane's scrollback — that URL is written to no file, so reading the screen is the only source. If the capture misses it, the chip says so and the live terminal shows the link.
- **On revive.** A revived agent inherits the setting; the link is harvested fresh, since the old URL died with the old session.

**The guard:** enabling remote control is refused (409) unless the session is at a turn boundary — queued or idle. An agent mid-turn, or sitting on a permission dialog, is not waiting for a slash command; typing one there would answer the dialog.

## LLM gateway routing

Claude Code talks to anything speaking the Anthropic wire format — [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI), a corporate gateway, your own proxy — via `ANTHROPIC_BASE_URL`. Fleet Deck makes that a **per-session** choice, so some agents route through a proxy and others go straight to Anthropic, and the board shows which is which.

Configure once on the Spawn form: **set up** next to 🛰 gateway.

| Setting | What it is |
| --- | --- |
| `gateway_base_url` | Where the gateway lives — `http://127.0.0.1:8317` for a stock CLIProxyAPI. `http://` and `https://` only, and **no credentials in the URL**: this value is shown on the board and travels in `/state`, so `user:pass@host` and `?api_key=…` are refused. |
| `gateway_token` | The credential. For CLIProxyAPI, any entry from its `api-keys:` list. |
| `gateway_auth_style` | `bearer` (default) sends `Authorization: Bearer …`; `api-key` sends `x-api-key`. **A 401 is almost always this** — the credential is fine, it is arriving in a header the gateway doesn't read. |
| `gateway_model_discovery` | On by default. Asks the gateway for its model list at startup so gateway-only models appear in `/model`. |
| `gateway_default` | Off by default. When on, a spawn that says nothing routes through the gateway. |

Then tick **🛰 route through …** on any spawn. Gateway-routed cards carry a 🛰 chip, and a revived agent keeps its lineage's routing — resuming a conversation against a different provider than the one that wrote its transcript should not happen quietly.

**The token never comes back.** It is stored on this machine and handed to the pane through tmux's own environment (`new-window -e`), keeping it out of the pane's command line and out of `ps`. The board is told `token_set: true` and nothing else, because `/state` is broadcast to every connected board, phones included. The base URL is not secret and deliberately is in there — which is why it refuses to carry a credential.

**Upgrade note.** Spawned panes no longer inherit `ANTHROPIC_*` from the daemon's environment; that inheritance let a single `export` in one terminal silently reroute every session on the machine. If you authenticate with an `ANTHROPIC_API_KEY` exported in your shell, move it to `~/.claude/settings.json` under `env`. Sessions you start yourself are unaffected.

**Two refusals.** A half-configured gateway (URL without token, or the reverse) fails the spawn with a 400 rather than quietly billing your Anthropic account. And **remote control is unavailable on a gateway-routed session** — Claude Code disables it whenever `ANTHROPIC_BASE_URL` points somewhere that isn't Anthropic, because claude.ai has no route to a session it isn't serving. The form greys out whichever you didn't pick.

> Sessions you start yourself are outside this; Fleet Deck only steers panes it launches. To put *everything* on a gateway, use `~/.claude/settings.json`.

## Jira tickets in callsigns

A callsign is `<animal>-<4 hex>` by default (`raven-4b7f`). When a session sits on a branch carrying a Jira key (`feature/PROJ-123-checkout`, `fd/PROJ-123-otter`), Fleet Deck swaps the hex for the ticket: **`raven-PROJ-123`**. The card, the mailbox target and the tmux window all read as the ticket.

- **Auto-detected from the branch**, no config. Read once at birth, and again the first time a ticketless session checks out a ticket branch; that rename happens once and is announced in the ticker.
- **`ticket <callsign> <PROJ-123>`** from Compose → ORCHESTRATOR pins one by hand. A manual pin wins over auto-detection and is never overwritten. `ticket <callsign> clear` restores the birth name.
- **One animal per ticket**, so every session on `PROJ-123` gets a different animal. When all twelve are taken, the thirteenth falls back to the hex suffix and says so in the ticker.
- **Spawns name artifacts ticket-first**: worktree `<repo>--fd-PROJ-123-<animal>` on branch `fd/PROJ-123-<animal>`, so worktrees and branch lists group by ticket.

## Naming a session

The animal is the fleet's; the ID is yours. Rename `wren-a9e1` to **`wren-docs-review`** with the **✎** chip on the card or `name wren-a9e1 docs-review` in Compose. `name <callsign> clear` restores the automatic name — the ticket name if the card has one, otherwise its birth name.

The animal never changes: twelve animals rotating through the fleet is what makes cards recognizable at a glance, and it keeps one-animal-per-ticket intact. Names are letters, digits and dashes — a space or dot would break the card's timeline filter and its tmux window name.

**A hand-typed name wins.** Branch auto-detection never renames over a name you chose. An explicit `ticket` command still does, because that is also you. Mail addressed to the old name keeps arriving.

## `/clear` keeps your card

Claude Code does not keep a session id across a `/clear` — it ends the old session and starts a fresh one. Fleet Deck follows the handoff: the new session continues the same card, callsign, tmux pane, ticket, mailbox and armed move-to-tmux. One ticker line notes the context was cleared; nothing else changes.

**Upgrading an existing fleet** is automatic as of 0.7.0: a new session's SessionStart hook notices an older running daemon, asks it to step down (SIGTERM, graceful; state is SQLite, nothing is lost), and boots its own newer build. Strictly newer, never a downgrade, and it fails open onto the running daemon if the handoff looks uncertain. A manual restart is always safe.

## Retention

Cards do not pile up, and nothing is deleted:

- A hook session **silent for 3 hours** is presumed ended and lands in OFFLINE. A late hook resurrects it — the tombstone is a timestamp, not a grave.
- An **offline card older than 24 hours** is archived off the board. The row stays in SQLite.
- **⌫ Clear** does both now: archives every offline card, expires undelivered mail and open questions, kills dead panes it owns — and **lists** orphaned worktrees rather than removing them. Deleting a git worktree is your decision.

Knobs: `FLEETDECK_PRESUME_DEAD_MS`, `FLEETDECK_RETAIN_OFFLINE_MS`.

## Standalone mode

Fleet Deck is a Claude Code plugin whose daemon is booted lazily by a `SessionStart` hook. That is fine on a laptop and useless on a remote dev box, where there may be no Claude Code session at all and the only way in is a browser tab. So it also runs as a service:

```bash
npm install -g fleetdeck
fleetdeck doctor            # Node 22.5+? tmux? claude? the plugin?
fleetdeck service install   # a systemd user unit, or a supervised wrapper without systemd
fleetdeck service start
```

The board is now always on, and you can **spawn an agent with no Claude Code session anywhere**: type a repo path, click, and it comes up in a tmux pane you can watch, type into, and answer prompts for. What standalone adds is a daemon that exists without a session to boot it, and a way to reach it from somewhere other than localhost.

To reach it from another machine, put it behind a reverse proxy and name the origin:

```bash
export FLEETDECK_TRUSTED_ORIGINS="https://board.example.com"
export FLEETDECK_PROXY_AUTH="trust"   # only if the proxy really authenticates; default is `token`
```

The same-origin wall is what stops any website you visit from driving your fleet over loopback, so it does not switch off — you widen it by exactly the origin you name. An unnamed origin is still refused, and a typo is a startup refusal rather than a board that mysteriously 403s.

The board is prefix-agnostic: it resolves assets, API calls and WebSockets relative to where it was loaded, so it works at a domain root or under a path prefix (`/apps/fleetdeck/`) behind nginx, Traefik, or a Coder path-based app.

Keep the plugin installed. The board can launch an agent without it, but status, model, edits and permission prompts all arrive through the plugin's hooks — without it, cards appear and never move.

**→ [docs/CODER.md](docs/CODER.md)** is the full guide for [Coder](https://coder.com) workspaces.

A managed daemon is never evicted by a plugin hook. Normally the newest installed plugin takes the port by SIGTERMing an older daemon; against a supervised service that would fight the supervisor, so the service wins and the version drift is reported in your session brief.

## The fine print

- **Spawned sessions are real billed Claude sessions.** Nothing spawns without a human click. (An *armed* move-to-tmux fires at SessionEnd, but the click that armed it was the decision — one-shot, cancellable, visible, and it expires.) `assign auto` routes to existing sessions only.
- **Unsupervised means unsupervised.** `--dangerously-skip-permissions` workers never produce permission cards. The checkbox is red and asks twice. Pair it with a fresh worktree.
- **The permission relay is interactive-only.** Headless `claude -p` sessions deny permission-needing tools without consulting hooks — CLI behavior, not ours. Spawned workers are interactive precisely so their prompts reach the board.
- **Version pin: Claude Code CLI 2.1.206+ (tested through 2.1.207).** Fleet Deck relies on a few undocumented behaviors; a guard test fails loudly if a CLI update drops them, and contract tests replay recorded hook payloads so schema drift is caught in CI.
- **Ports.** `FLEETDECK_PORT` / `FLEETDECK_HOME`. Hooks default to 4711, so a truly separate fleet also needs the port swapped in a copy of `hooks/hooks.json`. On multi-user machines give each OS user their own port.

### tmux isolation and the one-port rule

- **`FLEETDECK_TMUX_SOCKET`** runs every tmux command against a named server (`tmux -L <socket>`) instead of your default one. Tests and `demo/` scripts always set it, for a concrete reason: tmux bakes the **first client's environment** into a new server's global env, and every window created later inherits it. An acceptance run once started the default tmux server from inside a test session, and that evening's production spawns inherited the test `FLEETDECK_PORT`/`FLEETDECK_HOME` and reported to a daemon nobody was watching. The demo scripts now use a per-run socket and `kill-server` it on exit. Leave this unset in production — or set it to move your fleet off the shared default socket, where any unscoped `tmux kill-server` on the machine would take it down.
- **4711 is the supported production port.** Since 0.16.0 every hook event runs through a command shim (`scripts/fleet-hook.mjs`) that honors `FLEETDECK_PORT`, so a custom port no longer splits hook traffic. The shims also authenticate hooks: they read `$FLEETDECK_HOME/token` and attach it, which is why a tokenless `/hook/*` curl gets a 401.

### LAN mode

By default fleetd listens on `127.0.0.1`. Set **`FLEETDECK_BIND=0.0.0.0`** and it binds every interface, printing a ready-to-paste URL per address:

```
fleetd up on http://0.0.0.0:4711 (pid 12345, …)
fleetd LAN http://192.168.8.223:4711/?t=2a62f3c9…
fleetd LAN http://fleetdeck.local:4711/?t=2a62f3c9…   (mDNS — needs a resolver on the peer)
```

Open one on the other machine; the key is consumed at boot and scrubbed out of the address bar. The header's **⇄ Share** panel shows the same links with a QR code.

<p align="center">
  <img src="docs/assets/share-lan.gif" alt="The board's Share panel: a QR code and the LAN URLs, each carrying the key." width="100%">
</p>

**The `?t=` is a password.** This API can spawn agents with `--dangerously-skip-permissions` and type keystrokes into their terminals: unauthenticated, it is remote code execution for anyone on the network. LAN mode therefore *requires* a token — there is no insecure switch, and fleetd refuses to start rather than open an unauthenticated listener.

- **Loopback needs no token for ordinary routes** — browsing the board, watching sessions, hook traffic from the fleet's own shims. Since 0.16.0 the daemon mints a token every boot and the **powerful** routes demand it even locally: typing into terminals (`/ws/term`), `POST /mail`, `gateway_*` settings writes, and unsupervised spawns. Hooks authenticate automatically, and the daemon prints the credentialed local link at startup. On a shared box, other local users sit inside the loopback trust zone; `FLEETDECK_REQUIRE_TOKEN=on` closes every route behind the token — though it cannot protect you from processes running as *your* user, which can read the token file. See SECURITY.md.
- **Everything else must present the token**, as `Authorization: Bearer <token>` or `?t=<token>`. Wrong or missing → 401.
- **The static shell is public; fleet data is not.** The HTML and JS bundle contain no sessions, callsigns or key — only an empty board that knows how to ask for one. A browser cannot put a key on the `<script>` tag inside the page it is already loading, so gating the shell would serve a blank board rather than hide it. The printed link carries the key in the query string for the same reason: no `Authorization` header exists on a first navigation.
- **A bearer token, not a cookie.** Cookies ride along automatically, so any page you visit could make your browser POST to your board. A bearer token cannot be forged that way.
- The token is generated into `FLEETDECK_HOME/token` (mode `0600`), or set **`FLEETDECK_TOKEN`** yourself (16+ chars). Rotate by deleting the file and restarting.
- On untrusted networks use **Tailscale** or an SSH tunnel instead.

### Discovery (mDNS)

In LAN mode the daemon advertises itself over multicast DNS / DNS-SD with a dependency-free responder (no Avahi or Bonjour install): an A record for **`fleetdeck.local`**, plus `_fleetdeck._tcp` and `_http._tcp` services. `FLEETDECK_MDNS=off` disables it; `FLEETDECK_MDNS_NAME` renames it — two fleets on one network will collide on `fleetdeck.local`.

It is a convenience, never a dependency: if port 5353 is taken or multicast is blocked, mDNS degrades to a no-op and the printed IP URLs keep working. Resolving `.local` needs a resolver on the *other* machine — macOS and iOS have one, most Linux boxes need `avahi-daemon`, Windows is unreliable without Bonjour. If `fleetdeck.local` doesn't resolve for a peer, use the IP URL.

For moving between your own machines, **Tailscale beats mDNS**: a stable private IP that works off-LAN, with MagicDNS for names. Bind LAN mode, open the board at the tailnet address, and the token still guards it.

## Configuration

All optional; the defaults are what we run.

| Variable | Default | What it does |
| --- | --- | --- |
| `FLEETDECK_PORT` | `4711` | Daemon port. The hook shims honor it — read the one-port rule above before changing it. |
| `FLEETDECK_HOME` | `~/.fleetdeck` | State directory: SQLite db, LAN token, watcher pid files. |
| `FLEETDECK_BIND` | `127.0.0.1` | Bind address. `0.0.0.0` is LAN mode, which makes a token mandatory. |
| `FLEETDECK_TOKEN` | generated into `$FLEETDECK_HOME/token` | Bearer token, 16+ characters. Generated on every boot since 0.16.0, because hooks, `/ws/term`, `/mail`, gateway writes and unsupervised spawns all present it. |
| `FLEETDECK_REPOS_DIR` | `~/projects` (`/workspace` on Coder) | Where repo-mode spawns clone repositories that aren't local yet. The dialog's destination field can override and persist a different root. |
| `FLEETDECK_BROWSE_ROOT` | home (`/workspace` on Coder) | Root of the ⌸ Files explorer and the spawn form's folder picker. The `browse_root` setting wins over this; always resolved server-side. |
| `FLEETDECK_TRUSTED_ORIGINS` | unset | Comma-separated origins allowed to reach the daemon behind a reverse proxy — `https://board.example.com`, or one leading wildcard label (`https://*.coder.example.com`). Scheme required. Without it, a proxied board loads and then 403s. |
| `FLEETDECK_PROXY_AUTH` | `token` | Who authenticates a proxied browser. `token`: it must present the bearer token. `trust`: the proxy already authenticated it (only sane when it really does). |
| `FLEETDECK_TRUST_LOOPBACK` | off | `on` waives the 0.16.0 loopback power gates so a plain-loopback board needs no key — for single-user machines and port-forwarded Coder workspaces. Hooks stay authenticated. Refuses to combine with LAN mode or `FLEETDECK_REQUIRE_TOKEN`. |
| `FLEETDECK_REQUIRE_TOKEN` | off | `on` requires the token even over loopback on every route; only `/health` and the public shell stay exempt. Closes the loopback trust zone against other OS users. Does **not** protect against processes running as your own user. |
| `FLEETDECK_MANAGED` | unset | Set to `1` by `fleetdeck serve`. Marks the daemon supervisor-owned so a plugin hook never SIGTERMs it. Not set by hand. |
| `FLEETDECK_MDNS` | on (LAN only) | `off` disables the mDNS/DNS-SD responder. |
| `FLEETDECK_MDNS_NAME` | `fleetdeck` | The advertised name, i.e. `fleetdeck.local`. |
| `FLEETDECK_SPAWN` | on | `off` disables spawning; the board hides every spawn control. |
| `FLEETDECK_STALE_MS` | `600000` (10 min) | How long a working card runs without telemetry before it's badged stale. |
| `FLEETDECK_HOLD_MS` | `50000` (50 s) | How long a question hook is held open awaiting a board answer. Clamped 250 ms–60 s, under the 65 s hook timeout. |
| `FLEETDECK_NUDGE_MS` | `8000` (8 s) | Grace before a silent new pane gets its one bring-up Enter. Exactly once, and never into a folder-trust or MCP-approval dialog. |
| `FLEETDECK_SPAWN_REGISTER_MS` | `90000` (90 s) | How long a spawned pane may run without phoning home before it's flagged `stalled` — loudly, never auto-respawned. |
| `FLEETDECK_PANE_MAIL_GRACE_MS` | `1500` (1.5 s) | Head start given to the watcher before mail is typed into an owned pane. |
| `FLEETDECK_PRESUME_DEAD_MS` | `10800000` (3 h) | Silence after which a session is presumed ended. A late hook undoes it. |
| `FLEETDECK_RETAIN_OFFLINE_MS` | `86400000` (24 h) | How long an offline card stays on the board before archiving (never deleted). |
| `FLEETDECK_RETAIN_LEDGER_MS` | `86400000` (24 h) | How long file-touch, command, conflict and settled-mail rows live before aging out. |
| `FLEETDECK_CAPTURE_PAYLOADS` | off | `on` writes every hook payload to `$FLEETDECK_HOME/hook-payloads.jsonl` (`0600`) to debug schema drift. Secret-looking keys and known token shapes are redacted, but a secret in free text is only best-effort caught — leave it off unless chasing a hook bug. |
| `FLEETDECK_RC_HARVEST_MS` | `2500` (2.5 s) | Delay before reading a pane's scrollback for the claude.ai remote-control link. |
| `FLEETDECK_ADOPT_ARM_MS` | `1800000` (30 min) | How long an armed **move to tmux** waits for you to exit the CLI. |
| `FLEETDECK_ADOPT_DELAY_MS` | `750` | Grace after exit before the armed move resumes, so the CLI can flush its transcript. |
| `FLEETDECK_CLEAR_SUCCESSION_MS` | `30000` (30 s) | How long after a `/clear` a new session id in the same directory reads as that session continuing. |
| `FLEETDECK_TERM` | on | `off` disables the live terminal entirely. |
| `FLEETDECK_TERM_REPAINT_MS` | `80` | Repaint coalescing window for the terminal bridge. |
| `FLEETDECK_TMUX_SOCKET` | unset | Run every tmux command against a named server (`tmux -L`). See the one-port rule above. |
| `FLEETDECK_AGENTS_CMD` | the `claude agents` CLI | Override the agents-listing command. Whitespace-split into argv and run with no shell, so quotes, pipes and `$()` are never interpreted — wrap a pipeline in a script. `false` or blank disables that poller. |
| `FLEETDECK_AGENTS_POLL_MS` | `10000` (10 s) | Agents-poll cadence, which also drives owned-pane liveness. Floor 100 ms. |
| `FLEETDECK_WATCH_POLL_MS` | `25000` (25 s) | The idle-session watcher's long-poll hold per request. Clamped 50 ms–25 s. |
| `FLEETDECK_WATCH_MAX_MS` | `7200000` (2 h) | Lifetime cap on a watcher process before it exits and waits for the next turn. |

Deep-tuning knobs that rarely need touching: `FLEETDECK_TERM_CMD_TIMEOUT_MS` (10 s, the terminal bridge's tmux command timeout), `FLEETDECK_TERM_INPUT_MAX_BYTES` (256 KiB, the ceiling on queued terminal stdin), `FLEETDECK_WS_BUFFER_MAX` (1 MiB, per-peer WebSocket send-buffer cap before that peer is dropped and resynced), and `FLEETDECK_AGENTS_IDLE_POLL_MS` (60 s, how often an empty agents registry is re-polled).

`FLEETDECK_SPAWN_CMD`, `FLEETDECK_TERM_CMD` and the `FLEETDECK_TEST_*` family replace tmux and the daemon with fixtures for the test harness. They are not a supported way to run a fleet.

## Development

```bash
npm install
npm test                             # 600+ contract tests against a real daemon
npm run bundle                       # rebundle the daemon after touching scripts/fleetd/
npm run build:board                  # rebuild the React board into board-dist/
```

`npm test` runs serially (`--test-concurrency=1`) by necessity, not preference: every test boots a real daemon that binds a real port and drives a real tmux server. Run them in parallel and they contend for both, producing one or two failures a run — a different one each time, each passing in isolation.

The `demo/` scripts are live acceptance gates that start *real* Claude sessions and therefore cost money: `run-smoke.sh` (two sessions colliding on purpose), `run-accept-phase3.sh` (a permission and a trailing question answered from the board), `run-accept-spawn.sh` (spawn → assign → board-approved permission → kill), `run-accept-plan.sh` (plan → capture → unsupervised execution). Run them deliberately.

## Credits

Built by a fleet of Claude agents coordinating through contracts, reviewed by Codex, supervised by one human with a board — which is to say Fleet Deck was built the way Fleet Deck works.

Board design: "Console" direction — ink navy, amber means *yours to act*, IBM Plex Mono for anything that's data.

## License

MIT.
