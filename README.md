# Fleet Deck

**One control deck that drives Claude Code and Codex sessions on many machines.**

Fleet Deck v2 is a Rust rewrite of Fleet Deck. Version 1 (v0 in the plan) observed the Claude Code sessions of one machine through hooks. Version 2 starts and drives the agent sessions itself, through the native protocols of the providers, on each machine that you pair with the deck. You watch and control all of them from one web board, also from a phone.

> **Status:** branch `v2`, milestone M3 (fleet features and hardening). The 2.0.0 release and the merge to `main` are owner steps. The v1 README is in [docs/v0/README.md](docs/v0/README.md).

## The parts

| Part | What it does | How many |
| --- | --- | --- |
| **deck** | Serves the board, keeps the fleet state (mail, plans, tasks, routing, conflict radar, usage) and sends commands to the stations. | One for each owner |
| **station** | Runs on each machine that does agent work. It starts and drives the `claude` and `codex` processes and owns their worktrees, files and checkpoints. | One for each machine |
| **board** | The web client (React and TypeScript). It talks only to the deck. | Any number of tabs |

One binary, `fleetdeck`, contains the deck and the station. The deck embeds the board.

## What you can do

- **Drive sessions.** Start, prompt, steer, follow up, interrupt, stop, resume, fork and rename Claude Code and Codex sessions on any paired station.
- **Answer approvals and questions** from the board. A request that the station cannot route to a deck follows the station's offline approval policy. An unknown request fails closed.
- **Workspaces.** A session can get its own Git worktree (`fd/<callsign>` branch) or a fresh clone. Batch and `Nx` spawns always get a worktree.
- **Checkpoints and revert.** The station takes a checkpoint at each turn boundary. You can revert a session to an earlier turn; the conversation and the files go back together. A rejected revert changes nothing. When the station cannot prove the result of a revert, the session needs reconciliation, and input stays blocked until you resolve it on the board.
- **Fleet features.** Callsigns, mail between sessions, tasks with waitable completions, `assign auto` routing without a model call, a plan library, a conflict radar across stations, usage meters (unknown usage is never shown as zero), image paste and notifications.
- **Import a CLI session.** Continue a Claude Code or Codex conversation that you started in a terminal as a driven session, by its native session id (operations guide, §7.1).
- **Coordinator sessions (ADR-006, in progress: item 6-5).** The owner can grant a session a scoped coordinator MCP API to start and steer other sessions, with spawn caps, renewal and revocation.
- **Human shell.** A PTY shell on a station, in the board, when the station policy allows it.
- **Durable sessions (opt-in).** With `host = "shim"` in `station.toml`, the agents keep running through a station restart or upgrade, and the new station reattaches them.
- **Auto-continue (opt-in, default off).** After a station restart, a session that was in a turn can continue with a fixed station message, when every safety rule of ADR-007 holds. This release supports it only with the direct host (see the operations guide, §10).
- **Drain and self-upgrade** with a snapshot, a trial of the new build, a stable launcher and a rollback.

## Quick start (one machine)

1. Build the board and the binary (Rust stable, `bun`, `git`):

   ```sh
   bun install --cwd web --frozen-lockfile
   bun run --cwd web build
   cargo build --release -p fleetdeck-cli
   ```

2. Sign in to the provider CLIs that you want to use (`claude`, `codex`).
3. Start the deck and a local station in one process:

   ```sh
   target/release/fleetdeck up
   ```

4. Set the owner password of the board: `fleetdeck deck password`.
5. Open the board. `fleetdeck open` prints a one-time login URL. The default address is `http://127.0.0.1:7420`.

The station runs sessions only inside `allowed_roots` of `station.toml`. With no file, the station uses the defaults. See the [operations guide](docs/v2/operations.md) for the configuration, more machines, services and upgrades.

## More machines

1. On the deck machine, make an invitation: `fleetdeck deck invite`.
2. On the new machine, start the station and pair it: `fleetdeck station join` (it reads the invitation from a prompt, standard input or `--invite-file`; never from the command line).
3. The station shows on the board.

A station behind NAT dials the deck. A deck can also dial a station that listens (`fleetdeck station invite` and `fleetdeck deck add-station`). The link uses mutual TLS with pinned keys.

> **Caution:** use only loopback, private LAN or tailnet addresses. The deck refuses a public listen address. Do not forward a port from a router.

## Commands

| Command | What it does |
| --- | --- |
| `fleetdeck up` | The deck and a local station in one process. |
| `fleetdeck deck [serve]` | The deck only. Also `invite`, `add-station`, `stations`, `sessions`, `command`, `commands`, `remove-station`, `password`, `token`, `audit`. |
| `fleetdeck station [serve]` | The station only. Also `invite`, `join`, `forget-deck`, `sessions`, `run`, `approve`, `interrupt`, `drain`, `upgrade`. The local commands work without the deck. |
| `fleetdeck open` | A one-time board login URL. |

## Documentation

- [Operations guide](docs/v2/operations.md): configuration, services, pairing, backups, drain and upgrade, troubleshooting.
- [Migration and cutover guide](docs/v2/cutover.md): from Fleet Deck v1 (v0) to v2.
- [Internals](docs/v2/internals.md): glossary, crate map, data flow, link protocol.
- [Packaging](packaging/README.md): installer, systemd and launchd units, container image.
- [Live smoke tests](docs/v2/LIVE-SMOKE.md): the opt-in tests against the real CLIs.
- Architecture decisions: [ADR-001 Claude adapter](docs/v2/adr/ADR-001-native-claude-adapter.md), [ADR-002 Codex adapter](docs/v2/adr/ADR-002-native-codex-adapter.md), [ADR-004 link protocol](docs/v2/adr/ADR-004-link-protocol-v1.md), [ADR-005 ShimHost](docs/v2/adr/ADR-005-shim-host.md), [ADR-006 coordinator MCP](docs/v2/adr/ADR-006-coordinator-mcp.md), [ADR-007 auto-continue](docs/v2/adr/ADR-007-auto-continue.md).
- [Plan](docs/v2/PLAN.md) and [decisions log](docs/v2/DECISIONS-LOG.md).

## Development

| Command | What it runs |
| --- | --- |
| `make check` | The full offline acceptance check: fmt, clippy, tests, `cargo deny`, TypeScript type drift, web typecheck, lint, tests and build, recorded replays. |
| `make test` / `make test-web` | The Rust tests / the web checks. |
| `make ts-types` | Regenerates `web/src/gen` from `fleetdeck-proto`. |
| `make perf` | The performance bars of PLAN §13 (release build, loopback, fake agents). |
| `make package-check` | Static checks of the packaging files and an offline binary smoke test. |
| `make smoke-live` | The live smoke tests (real CLIs, network, a small number of tokens). |

Normal tests use fake agents and recorded fixtures and need no network.

## License

MIT. See [LICENSE](LICENSE).
