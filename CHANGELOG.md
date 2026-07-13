# Changelog

All notable changes to Fleet Deck are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.1] - 2026-07-13

### Changed

- Board build toolchain: Vite 8 and `@vitejs/plugin-react` 6; the shipped board bundle was rebuilt (no functional change).
- CI: `actions/checkout` v7 and `actions/setup-node` v6 (Dependabot majors).

### Fixed

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

[unreleased]: https://github.com/lacion/fleet-deck/compare/v0.5.1...HEAD
[0.5.1]: https://github.com/lacion/fleet-deck/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/lacion/fleet-deck/releases/tag/v0.5.0
