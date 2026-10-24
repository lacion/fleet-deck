# Migration and cutover guide: Fleet Deck v1 (v0) to v2

Fleet Deck v2 is a new product with a new binary, new data and new configuration. v2 does not read the v1 data (`FLEETDECK_HOME`, the v1 SQLite file) and does not use the v1 Claude Code plugin or hooks. This guide tells the owner how to move from v1 to v2 on one machine and on more machines.

The repository side of the cutover (PLAN Phase 7, item 7-7) happens on the `v2` branch only: the v0 code (`src/`, `board/`, `hooks/`, `scripts/`, the plugin files and the root `package.json`) is deleted there. The merge of `v2` into `main` and the 2.0.0 release are owner steps.

## 1. What changes

| v1 | v2 |
| --- | --- |
| One daemon (`fleetd`) on port 4711 that observes the Claude Code sessions of one machine through hooks. | A deck (the board and the fleet state) and one station on each machine. The station starts and drives the agents itself. |
| Install through the Claude Code plugin marketplace or npm. | One binary, `fleetdeck`: the cargo-dist installer, Homebrew or `cargo install` (see `packaging/README.md`). |
| Sessions in tmux panes, a live terminal in the browser. | Sessions in the session view (native protocol), and a separate human shell when `allow_shell = true`. |
| Re-armed questions. | Not needed: the request stays open in the protocol; the offline approval policy covers a deck outage. |
| Revive. | Native resume. |
| Adopt or take over a terminal session. | Import the native session id as a driven session (`session.import`, DL-308). |
| The `/fleet` slash command and the fleet-doctrine skill. | A coordinator session with a scoped coordinator MCP grant and a deck-served control skill (ADR-006, DL-217, DL-219). A Claude coordinator starts from version 2.1.296, which the coordinator live smoke verified. A Codex coordinator start still fails with `handoff_unverified`. |
| Share the board on the LAN with a URL token. | Board login (owner password, one-time URL, API tokens) on loopback, the private LAN or the tailnet. |
| Coder support, remote control through claude.ai. | Dropped. |

The full parity table is PLAN §16.

Item 6-13 records the test for each parity row in [PARITY.md](./PARITY.md). Two v1 behaviours have no v2 equivalent yet; they are deferred for an owner decision (DL-131):

- the terminal grid (v2: a session grid)
- station discovery on the LAN (add dial targets by hand)

The minimum CLI version is built as a warning (DL-158, DL-162): the board and `fleetdeck doctor` warn about a CLI that is older than the tested version, and the station refuses only a version that is known to break the protocol.

## 2. Before you start

1. Finish or stop the work of the v1 sessions. v2 does not take over a running v1 session.
2. Keep a copy of the v1 data directory (`FLEETDECK_HOME`, default `~/.fleetdeck`) if you want to read old mail or plans later. v2 does not import them.
3. Sign in to the provider CLIs (`claude`, `codex`) on each machine that will run a station.

## 3. Remove v1

> **Caution:** the v1 hooks call the v1 daemon at every Claude Code event. Remove the plugin before you stop the daemon for good, or each session prints hook errors.

1. Uninstall the plugin: `claude plugin uninstall fleetdeck@fleetdeck` (or use `/plugin`). With the npm channel: `npm rm -g fleetdeck`.
2. Stop the v1 daemon (`fleetd`) and remove its service, if you made one.
3. Start a new Claude Code session and make sure that no hook error shows.

## 4. Install v2

1. Install `fleetdeck` (see `packaging/README.md`).
2. One machine: run `fleetdeck up`, set the password with `fleetdeck deck password`, and open the board with `fleetdeck open`.
3. More machines: run the deck on one machine and pair each station (see the [operations guide](./operations.md) §6).
4. Write `station.toml` on each station with `allowed_roots` set to your code directories.

v2 uses port 7420 for the board and 7421 for the link by default, so v1 (4711) and v2 can run at the same time during a test period. Do not let both drive the same session.

## 5. Move your work

| v1 item | How to move it |
| --- | --- |
| A running session | Let it end in v1. Start a new v2 session in the same worktree, or import its provider session id (see the [operations guide](./operations.md) §7.1). |
| Worktrees made by v1 (`<repo>--fd-<callsign>`) | They stay normal Git worktrees. Start a v2 session with that directory as `cwd`. v2 uses the same naming for the worktrees that it makes. |
| Mail, plans, tasks | Not migrated. Copy what you need into a v2 plan or a prompt. |
| LAN share link | Use the board login on the LAN or the tailnet address. |
| Gateway environment (LLM gateway) | Put it into `env` of a `[[provider_instance]]` in `station.toml`. |

## 6. Roll back to v1

Until the 2.0.0 release, v1 stays on `main`. To go back, stop the v2 services and reinstall the v1 plugin or the npm package. The v2 data stays in its own directories and does not touch v1.

## 7. Repository cutover checklist (item 7-7, branch `v2` only)

Done in lane A round 103 (DL-132); test `crates/fleetdeck-cli/tests/cutover.rs` `the_v0_code_and_plugin_files_are_gone_from_v2`. Steps 1-4 are done; step 5 is item 7-8; step 6 stays an owner step.


1. All lanes of M3 are merged and their checks pass.
2. Delete `src/`, `board/`, `hooks/`, `scripts/`, the plugin files (`.claude-plugin/`) and the root `package.json` on `v2`.
3. Remove the v1 CI jobs and the v1 entries of the root configuration files.
4. Keep `docs/v0/README.md` as the v1 record.
5. Run `make check` on a clean checkout.
6. Owner steps after sign-off: merge `v2` into `main`, tag and release 2.0.0.
