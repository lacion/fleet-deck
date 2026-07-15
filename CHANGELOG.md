# Changelog

All notable changes to Fleet Deck are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

A security / reliability / docs audit of the daemon and board.

### Security

- **Proxy-auth token bypass closed.** Behind a reverse proxy, a request arriving over loopback could be auto-authorized even when `FLEETDECK_PROXY_AUTH` was configured to require the bearer token. Proxied requests now present the token as configured.
- **Mail-to-pane keystroke injection hardened.** Mail delivered by typing into an owned tmux pane can no longer be crafted to inject control sequences beyond the message itself.
- **State files are owner-only.** Files and directories under `FLEETDECK_HOME` are created `0600`/`0700`, so a co-tenant on a shared host can't read your fleet state.
- **Spawn prompts are never parsed as flags.** A spawn prompt beginning with `-`/`--` could be swallowed as a `claude` option; the prompt is now always passed as data, never as an option.

### Fixed

- Several async races along the spawn, adopt and terminal paths.
- Dead code removed, and the docs (README, SECURITY, this changelog, and the plugin manifests) reconciled with the shipped behavior.

## [0.9.1] - 2026-07-15

### Fixed

- **Image pastes are written under `FLEETDECK_HOME`, not a fixed name in shared `/tmp`.** The 0.9.0 paste dir was `os.tmpdir()/fleetdeck-pastes` — a world-known path in sticky `/tmp` — and the pruner followed a directory symlink planted there. On a multi-user host a local co-tenant could pre-create that symlink and turn a victim's paste into an arbitrary-file delete/overwrite as the victim's user (and, as a lesser variant, squat the name to break the feature for everyone else on the box). A single-user laptop or a single-owner Coder workspace was never exposed — this bites shared login/build hosts. The directory now lives under `FLEETDECK_HOME`, which no other user can write, so the symlink cannot be planted at all; the pruner additionally `lstat`s every entry and never follows a symlink, and the dir is refused if it is not a real directory this user owns.
- **A burst of pastes can no longer clobber an earlier one.** Filenames were a per-second timestamp plus 24 random bits; names are now a 128-bit `randomUUID`, and the staging write is exclusive (`wx`), so two pastes in the same instant get distinct files instead of one overwriting the other.
- **The paste directory is bounded.** Age-pruning (>24 h) only ran on the *next* paste and had no ceiling, so a flood — or a wedged agent that never submits — could grow it without limit. It now also keeps at most the 50 most-recent files.
- **The feature is no longer silent.** Every failure path (too-large image, unreachable daemon, unsupported bytes, a pane that lost focus mid-upload) now shows a transient status over the pane instead of only a console warning — a 12 MB screenshot that was silently dropped in 0.9.0 now says "image too large (max 10 MB)". Success shows "image added — press Enter to send".
- Reject an oversized paste by its `Content-Length` before buffering it; tighten the transport cap to what a 10 MB image actually needs; validate base64 up front instead of letting malformed input decode to garbage.

## [0.9.0] - 2026-07-15

### Added

- **Paste an image into the board terminal.** Ctrl+V a screenshot into a live pane and it lands in the agent's composer — something no terminal connection can do by itself, because the image lives in the *browser's* clipboard and the wire only carries text (and Claude Code has no Linux clipboard-image read to hand it to anyway). The board now does what a terminal cannot: it lifts the blob off the clipboard, ships it to the daemon (`POST /api/paste-image`, base64-in-JSON so both CSRF walls keep standing), the daemon sniffs the magic bytes (png/jpeg/gif/webp — the client's mime claim is never trusted), writes the file owner-only under `tmp/fleetdeck-pastes/`, and the board *types the path* into the pane through the same stdin gate as every keystroke. Which means the grid's one-tile-types discipline governs pastes too, and the paste never submits on its own — you read the path, you press Enter. Text paste is untouched: a clipboard with no image falls through to xterm exactly as before. Pasted files are pruned after 24 hours; the image is read the moment you submit, so nothing of value lives there.
- **`fleetdeck` publishes to npm automatically on a version tag**, via GitHub's OIDC trusted publishing — no long-lived npm token stored in CI. This is the pipeline behind `npm i -g fleetdeck`: a tagged release reaches the registry on its own, so the standalone CLI ships in lockstep with the plugin.

## [0.8.0] - 2026-07-14

### Added

- **Standalone mode: run the board as an always-on service.** Fleet Deck has only ever been a Claude Code plugin, and the daemon it needs was booted for it — lazily, detached — by a `SessionStart` hook. That is right on a laptop and useless on a remote dev box, where there may be no Claude Code session at all and the only way in is a browser tab. So the same daemon now also runs as a supervised service, installed from npm: `npm i -g fleetdeck && fleetdeck service install && fleetdeck service start`. The board is then simply *on*, and you can spawn an agent with no Claude Code session anywhere — type a repo path, click, and watch it come up in a pane you can type into and answer prompts for, all from the browser. The plugin is unchanged and the two cooperate.
- **A `fleetdeck` CLI**, published to npm as a dependency-light package that carries the same daemon bundle the plugin ships (so there is exactly one daemon implementation, and CI's drift gate covers both entry points). `serve` runs it in the foreground for a supervisor; `doctor` preflights the box (Node ≥ 22.5, tmux, the `claude` CLI, and — the one people miss — whether the plugin is installed, without which spawned cards appear and then never move); `status`, `token`, and `service install|start|stop|restart|uninstall`. `service install` writes a systemd user unit when systemd is there and a supervised wrapper when it is not, because a container often has no init system at all. Both restart a daemon that dies, both decline to restart one that exited cleanly or lost the port election, and `service start` returns as soon as the board actually answers — never before, and never blocking.
- **`FLEETDECK_TRUSTED_ORIGINS` — run the board behind a reverse proxy.** A proxy does not rewrite `Host`, so the daemon saw the browser-facing hostname and refused every POST, both WebSocket upgrades and the mutating GETs: the board's shell would load and then everything inside it failed. Name the origin (`https://board.example.com`, or one leading wildcard label like `https://*.coder.example.com`) and the same-origin wall widens by exactly that and not one inch more. A scheme is required, an origin you did not name is still refused, and a malformed entry is a startup refusal rather than a board that mysteriously 403s. Configure nothing and every wall behaves byte-for-byte as before.
- **`FLEETDECK_PROXY_AUTH`** — a proxy reaches the daemon over loopback, and loopback auto-authorizes, so left alone that would hand the fleet (spawn included) to anyone who can reach the proxy. `token` (the default) makes a proxied browser present the bearer token anyway; `trust` says the proxy is the authenticator. Whether that is true is not something the daemon can infer, so you say it out loud. A local CLI hook sends no `Origin` and is never dragged into either.
- **The board is now prefix-agnostic.** It resolves its assets, API calls and WebSockets relative to wherever it was loaded from, so one build works at a domain root, under a path prefix (`/apps/fleetdeck/`), and behind nginx, Traefik or a Coder path-based app — which strips its prefix before forwarding and tells the app nothing about it.
- **[docs/CODER.md](docs/CODER.md)** — the full guide for [Coder](https://coder.com) workspaces: a copy-pasteable `coder_script` + `coder_app`, why `subdomain = true` is worth insisting on, and why there is no systemd in that container.

### Changed

- **A service-managed daemon is never evicted by a plugin hook.** 0.7.0 taught the newest installed plugin to take the port by SIGTERMing an older daemon. Against a supervised service that starts a fight nobody wins — the hook kills the daemon and spawns a replacement at the same moment the supervisor restarts it, and whichever loses the port bind exits 3. So the service wins: `fleetdeck serve` marks the daemon `managed` on `/health`, the hook leaves it alone, and the version drift is reported in your session brief instead of silently swallowed. Discovering that a fix you installed is not the code that is running should not cost an afternoon.
- The bearer token is now generated whenever the board is reachable from off-box — LAN mode, as before, and now also a reverse proxy in `token` mode. Without this a proxied daemon would have had no token to compare against and would have refused every request it was meant to gate: a locked door with no key.
- CI gains a version-consistency gate. Four manifests carry the same version string and nothing enforced it; standalone makes that drift user-visible, since the npm version and the plugin version are now compared at every `SessionStart`.

**Upgrade note:** no schema change, and nothing to do. Existing plugin installs keep working exactly as before — every new knob is opt-in, and a fleet that sets none of them is byte-for-byte the fleet you had.

## [0.7.1] - 2026-07-14

### Fixed

- **A `/clear` no longer splits your card in two.** The current Claude Code CLI does not keep the session id across a `/clear` — it ends the old id and mints a new one, which starts in the same directory a few milliseconds later. Fleet Deck believed the opposite, so the *old* card kept the tmux pane (and therefore the terminal, watch and kill chips) while going permanently silent, and a *second* card appeared for the new id with no pane at all: you clicked terminal on one card and watched the status updates land on the other. A board-spawned worker that ran `/clear` stranded its own pane the same way. Now the new session id **continues the card**: same callsign, same pane, same ticket, same mailbox, same armed move-to-tmux, and the file ledger comes along too (so a session can't raise a file conflict against its own past self). The retired id is archived and can never be resumed — its transcript is a closed chapter. A fleet that already has a pair split this way is **healed at boot**, so upgrading is enough to put the pane back on the card that is doing the work.

### Added

- **Rename a session.** The animal is the fleet's — the ID part is yours: `wren-a9e1` → `wren-docs-review`. Rename from the board (a `✎` chip on the card or in the drawer) or from Compose with `name <callsign> <suffix>`; `name <callsign> clear` reverts to the automatic name (the ticket name if the card has one, else the birth name). A hand-typed name outranks branch ticket auto-detection — automation never renames over a human — while an explicit `ticket` command still takes the name over. Mail addressed to the old name keeps arriving, as with any rename. New endpoint: `POST /api/sessions/:session_id/name`.

**Upgrade note:** additive schema migration — three nullable columns (`cleared_at`, `succeeded_by`, `custom_suffix`) on the `sessions` table. The daemon upgrades itself on your next session (0.7.0's takeover), and the `/clear` heal runs on that boot.

## [0.7.0] - 2026-07-14

### Added

- **Move to tmux.** A `⇥ move to tmux` chip on cards the board didn't spawn (plain-CLI and other hook-tracked sessions) adopts the session into a board-owned tmux pane via `claude --resume <session-id>` — same session id, same card, full history, and afterwards it's a first-class fleet worker (terminal modal, revive, kill, mail-to-pane). An ended card moves immediately; a live card is **armed** and moves the moment you exit it in your terminal. The arm is durable intent consumed exactly once: it lives in SQLite (not in a timer), a disarm wins even in the settling moment right after you exit, a session that comes back alive cancels the move instead of leaving a standing order, and a daemon death mid-move (crash or upgrade takeover) is recovered by the next boot's sweep. It expires after ~30 minutes (`FLEETDECK_ADOPT_ARM_MS`), and a `/clear` keeps it, since the session is still live. The dialog carries the same asks-twice unsupervised gate as the Spawn form for keeping `--dangerously-skip-permissions` across the move. Refusals are honest: **409** when the session has any spawn lineage, alive or dead (the board owns its pane story — ⟲ revive is that button, and a card never offers both), **409** when nothing ever *proved* the CLI exited (a 3h-silence guess, an agents-CLI absence, or a pre-0.7.0 row with no provenance — a quietly-alive session must never be resumed into a second billed session; arm it instead), and **410** when the cwd or transcript is gone. New endpoint: `POST /api/sessions/:session_id/adopt`.
- **Plugin upgrades take effect automatically.** A SessionStart hook from a newer plugin version now replaces a stale daemon on port 4711: verify (the /health pid must match the HOME pidfile and look fleetd-shaped) → SIGTERM (the existing graceful shutdown; state is SQLite and survives) → boot the newer build, announced in the board ticker (`⬆️ fleetd vX replaced vY`). Strictly newer only — never a downgrade, never on an unparseable/standalone `0.0.0` version — and every uncertain path fails open onto the running daemon. Manual restarts remain fine. (`FLEETDECK_VERSION_OVERRIDE` exists for the test suite only.)
- The daemon version is now in the `/state` snapshot and shown in the board header, so you can see at a glance which build is serving you.

### Changed

- Session ends now carry provenance (`sessions.end_reason`): a real hook end records its reason, retention's 3h-silence guess records `presumed`, and the agents-CLI absence sweep — which is likewise a guess, from a registry that is documented-unreliable — now records `presumed` too. Move-to-tmux treats this as an allowlist: only a proven end may be resumed immediately.

**Upgrade note:** additive schema migration — three nullable columns (`adopt_armed_until`, `adopt_armed_skip`, `end_reason`) on the `sessions` table. Offline cards that predate 0.7.0 carry no end provenance, so they are never offered an immediate move-to-tmux (arm them instead, or revive them if the board spawned them). This is the last upgrade that needs a manual daemon restart to load; from here on the takeover does it for you.

## [0.6.0] - 2026-07-13

### Added

- Jira-ticket callsigns. A session on a branch that carries a Jira key (`feature/PROJ-123-checkout`) is named `<animal>-PROJ-123` instead of `<animal>-<hex>` — the ticket is auto-detected from the git branch, once at birth and once more the first time a ticketless session checks out a ticket branch (a single, announced rename). Every session on one ticket gets a distinct animal; when all twelve are taken the thirteenth falls back to the hex suffix.
- A `ticket <callsign> <PROJ-123>` orchestrator command that pins or changes a session's ticket by hand — a manual pin wins over branch auto-detection and is never overwritten — plus `ticket <callsign> clear` to drop it and restore the birth name.
- Ticket-first worktree and branch names for spawns: a worker spawned from a `PROJ-123` branch lands in `<repo>--fd-PROJ-123-<animal>` on branch `fd/PROJ-123-<animal>`, so sibling worktrees and branch lists group by ticket. Ticketless spawns keep the `<repo>--fd-<callsign>` / `fd/<callsign>` format.
- Mail routing survives a rename: a session's birth (pre-rename) callsign keeps delivering, so a message addressed to the name a peer last saw still reaches it — without double-delivering to a reissued name.

### Fixed

- The board's compose box now surfaces daemon-command rejections (HTTP 200 bodies with `ok:false`, e.g. a malformed `ticket` command) as inline errors and confirms ticket renames, instead of closing as if the command had succeeded.

**Upgrade note:** this ships an additive schema migration — three nullable columns (`ticket`, `ticket_source`, `prev_callsign`) are added to the `sessions` table on first boot, and existing pre-0.6.0 rows read back as `ticket: null`. **Restart the daemon** to load the new code: hooks boot the committed bundle, so source changes ship nothing until it restarts. Live sessions already on a ticket branch then rename themselves once on their next hook event.

## [0.5.1] - 2026-07-13

### Changed

- Board build toolchain: Vite 8 and `@vitejs/plugin-react` 6; the shipped board bundle was rebuilt (no functional change).
- CI: `actions/checkout` v7 and `actions/setup-node` v6 (Dependabot majors).

### Fixed

- A boot-time race in spawn reconciliation: `reconcileSpawns()` read its candidate rows after awaiting the tmux window list, so a spawn (or revive) landing in that window at daemon startup was judged against a stale snapshot, condemned to `gone`, and tombstoned while its pane came up fine. Rows are now snapshotted before the await, mirroring the liveness tick. Surfaced as an intermittent Node 24 CI failure; reproducible on any Node with unlucky timing.
- A startup-banner race in the LAN token-elision test: it asserted on the banner's second line after waiting only for the first, flaking on slow runners.

## [0.5.0] - 2026-07-13

A deep security / reliability / performance / quality audit of the daemon and
board, run as a multi-agent effort with cross-provider adversarial review of
every change. See [`docs/AUDIT-2026-07.md`](docs/AUDIT-2026-07.md) for the full report.

### Security

- Fixed a critical CSRF / cross-site WebSocket hijack: loopback was auto-trusted with no `Origin`/`Host`/`Content-Type` check, so any web page you visited could read the `/ws` snapshot, drive `/ws/term`, and POST `/api/spawn`. State-changing routes and both WS upgrades now enforce an `Origin`/`Host`/`Sec-Fetch` allowlist plus an `application/json` requirement (this also closes DNS rebinding), without breaking the zero-config loopback board.
- Hardened the LAN token: it no longer rides the WebSocket broadcast or gets logged; `fleetd.log` and hook-payload capture are now mode `0600` (capture off by default); `remote.url` is restricted to `https://claude.ai`; the mDNS instance name is anonymized.

### Fixed

- Node 24 compatibility: the HOME-lock owner check identified fleetd via `/proc/<pid>/comm`, which Node 24 renames to `MainThread` — a second daemon could misread a live owner as a recycled PID, steal the lock, and open its SQLite alongside it. The check now resolves the `/proc/<pid>/exe` symlink. Caught by the new CI's Node 22/24 matrix on its first run.
- Four production spawn-lifecycle bugs (reported over the fleet board after they lost every live terminal): `/clear` no longer condemns a live pane; retention verifies a pane via tmux before presuming a silent-but-live agent dead; `pane-dead`/`gone` spawns are re-validated and resurrected with hysteresis, and `revive()` adopts a live pane instead of 409-deadlocking; mail no longer silently truncates at 500 chars (now bounded at 4000, surrogate-safe, and signalled).
- Reliability hardening: worktree removal no longer `rm -rf`s uncommitted work without `--force`; boot reconciliation no longer tombstones the live fleet on a tmux timeout; per-paste unique tmux buffers stop cross-agent mail bleed; plus byte-correct UTF-8 request bodies, WS keepalive and backpressure, bounded DB and caches, and a global `unhandledRejection` handler.

### Changed

- Performance: broadcast coalescing, tiered transcript reads, memoized board and clock context, idle poll backoff, batched PTY output, and a prepared-statement cache.
- Structure: `derive.mjs` (2955 lines) was split into a 314-line composition root plus 13 focused modules, and `App.jsx` (763 → 433 lines) into hooks plus a `Header` component — both verified behavior-preserving. Accessibility: ARIA-legal cards, modal focus traps, and keyboard tile focus.

## 0.4.1 - 2026-07-12

### Fixed

- In the live terminal, Shift+Enter now inserts a newline instead of firing the prompt — the board is the emulator, so it sends the `ESC CR` that Claude Code's own keybinding expects, with nothing to configure.

## 0.4.0 - 2026-07-12

### Added

- Batch spawn: tick **batch** on the spawn form and the prompt box becomes a task list — one agent per line, each in its own git worktree on its own branch, all launched in one click. A `3x` prefix races several independent attempts at the same task.
- The terminal wall: `▦ Terminals` opens every live agent at once in a grid that shares one tmux control client. Exactly one tile types (it wears an amber ring); every other tile is stdin-disabled at the terminal itself.

### Changed

- The model badge now tells the truth: it is derived from the session transcript, so it follows a mid-session `/model` switch instead of freezing on whatever model the session launched with.
- Reworked the README install section so the one line a stranger copies actually runs.

## 0.3.1 - 2026-07-11

### Added

- Worktrees modal: see what is inside an orphaned worktree before you decide to delete it.

### Fixed

- Clear now means clear: conflicts, the needs-you rail, and the activity feed all follow a dead session off the board.
- Corrected the worktree verdict, which was measuring the wrong thing.

## 0.3.0 - 2026-07-11

### Added

- Live terminal: click a board-spawned session's tmux chip and its pane opens in an xterm.js modal bridged over WebSocket to a tmux control-mode client — typing sends real keystrokes to the running agent, no PTY and no native deps.
- Revive: one `⟲` click resumes a dead agent in its own worktree with its full history (`claude --resume`) — same callsign, same card — with **Revive all** for whole columns at once.
- Remote control: hand a session to claude.ai and drive it from your phone; the card grows a 📱 chip named after the callsign.
- LAN mode: `FLEETDECK_BIND=0.0.0.0` opens the board to your network behind a mandatory bearer token, with dependency-free mDNS discovery (`fleetdeck.local`) and a QR-code Share panel.

### Changed

- Made the needs-you rail honest about which sessions are actually waiting on you.

## 0.2.0 - 2026-07-11

### Added

- Spawn watchdog: a spawned pane that never phones home within its window is flagged `stalled` — loudly, and never auto-respawned.
- Pane mail delivery: mail is typed straight into an owned pane when the watcher has not woken the session first.
- Retention and cleanup: silent sessions are presumed ended and moved to OFFLINE; offline cards older than 24h are archived off the board (the row is never deleted).

### Changed

- Environment hygiene for spawned sessions, so a worker cannot inherit a stray `FLEETDECK_*` env and report to the wrong daemon.

## 0.1.0 - 2026-07-10

Initial public release.

### Added

- One local mission-control board at **http://127.0.0.1:4711** showing every Claude Code session on the machine as a card with a callsign, a live column derived from hook telemetry (`queued → working → verifying → needs-you → idle → offline`), and a mailbox.
- Conflict radar: two sessions touching the same file within 30 minutes get whispered at in context and the board flashes hazard-red — and it is worktree-aware.
- Needs-you rail: permission prompts, multiple-choice questions, MCP forms, and trailing questions become cards you answer from the board.
- Mail with honest latency: message any session, a whole repo, or everyone, delivered at the next turn boundary, with idle sessions woken by a small watcher.
- A brainless orchestrator: `assign auto` routes a task to the best existing session with a SQL query, not a model call — the core makes zero model calls.
- One-command plugin install with a self-contained daemon bundle (`node:sqlite` state, nothing to `npm install`); the first session's SessionStart hook elects and launches the daemon. MIT licensed.

[unreleased]: https://github.com/lacion/fleet-deck/compare/v0.9.1...HEAD
[0.9.1]: https://github.com/lacion/fleet-deck/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/lacion/fleet-deck/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/lacion/fleet-deck/compare/v0.7.1...v0.8.0
[0.7.1]: https://github.com/lacion/fleet-deck/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/lacion/fleet-deck/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/lacion/fleet-deck/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/lacion/fleet-deck/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/lacion/fleet-deck/releases/tag/v0.5.0
