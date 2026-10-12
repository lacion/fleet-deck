# Fleet Deck v2 internals

This document is the map of the v2 code for a developer and a reviewer. It gives the glossary, the crate map, the data flow, the link protocol and the durability rules. The binding contracts are in [PLAN.md](./PLAN.md) and the ADRs in [adr/](./adr/); this document points to them and does not change them.

## 1. Glossary

| Term | Meaning |
| --- | --- |
| **deck** | The hub node. It serves the board, keeps the fleet state and routes commands to the stations. One for each owner. |
| **station** | The worker node on a machine. It runs the provider processes and owns the worktrees, files, checkpoints and the session event log. |
| **board** | The web client (React, TypeScript) in `web/`. It talks only to the deck. |
| **link** | The mutual-TLS connection between a deck and a station: one control WebSocket and one data WebSocket (ADR-004). |
| **provider** | An agent CLI: Claude Code (`claude`, stream-json control protocol, ADR-001) or Codex (`codex app-server`, JSON-RPC, ADR-002). |
| **provider instance** | A configured provider on a station: `id`, `driver`, `binary`, `config_home`, `env` (`station.toml`). |
| **adapter** | The code that turns a provider's native protocol into canonical events and commands into native messages (`fleetdeck-providers`). The adapter cores are sans-IO. |
| **session host** | The layer that owns the provider process: `DirectChildHost` (the agent is a child of the station) or `ShimHost` (the agent runs under a `fleetdeck shim` process and survives a station restart, ADR-005). |
| **shim** | The small `fleetdeck shim` process that holds one provider process, buffers its output, writes its stdin by frame id, and reports its exit. |
| **session / run / turn** | A session is one agent conversation. A run is the work that one input starts. A turn is one provider turn inside a run. |
| **canonical event** | A provider-neutral event (`fleetdeck_proto::event::Event`) with a station sequence (`station_seq`) and a session sequence (`session_seq`). Unknown kinds round-trip unchanged. |
| **command / receipt** | A mutating request with a ULID `command_id`. The station decides it once and stores an accept or reject receipt; a retry gets the same receipt. |
| **effect** | An outbox row that a decision stores for the external I/O (start, prompt, answer, stop, revert, resume). States: `queued`, `claimed`, `confirmed`, `retired_not_dispatched`, `outcome_unknown`. |
| **incarnation** | The id of one station database. A restore makes a new one; a command for another incarnation is refused. |
| **outcome_unknown** | The station cannot prove whether the provider received an input. It never resends it. |
| **lost** | A session whose process the station cannot account for after a restart. |
| **checkpoint** | A snapshot of the workspace at a turn boundary, for revert (PLAN §6.6). |
| **callsign** | A short session name, unique on its station; the address for mail and tasks. |
| **workspace** | The directory where a session works: the repository itself, a new git worktree, or a clone. The station provisions it (`workspace/provision.rs`). |
| **mail / task** | Deck-side fleet records: a message from one session to another (delivered as input through the outbox), and a unit of work that the deck assigns to a session. |
| **coordinator grant** | A scoped, expiring permission that lets one session start and steer other sessions through the coordinator MCP API (ADR-006). The owner renews or revokes it. |
| **drain** | A station mode that refuses new work before an upgrade (DL-128). |
| **auto-continue** | An opt-in per-session preference: after a station restart, continue a lost turn with a fixed station message when every ADR-007 rule holds. |

## 2. Crate map

| Crate | What is in it |
| --- | --- |
| `fleetdeck-core` | Ids (ULID), clock, `Secret<T>`, identity keys, `station.toml` and `deck.toml`, directories, SQLite with migrations (`Db`), logging. |
| `fleetdeck-proto` | The wire types: events and the fold into `SessionSummary`, commands and receipts, link frames, versions and features, plans, input, conflicts, shell types. `make ts-types` exports them to `web/src/gen`. |
| `fleetdeck-providers` | The adapter traits, the Claude and Codex adapters, capabilities, checkpoints of the adapter cores, `DirectChildHost` and the shim (`host/shim/`: wire frames, launch, server, stdin, host). |
| `fleetdeck-station` | The station: `store/` (event log, receipts, effects, recovery, shims, reverts, auto-continue facts), `manager/` (decide, session targets, effect worker, durable drivers, resume, revert, auto-continue, drain), workspace service, policy, shell service, local socket, `serve.rs`, `upgrade.rs`. |
| `fleetdeck-link` | TLS from identity keys, the handshake, pairing, the control and data channels, the method table with scopes and features, reconnect. |
| `fleetdeck-deck` | The deck: registry of stations, read model, fleet services (mail, tasks, plans, routing, conflicts, usage), outbox, auth and audit, HTTP and WebSocket API, the embedded board. |
| `fleetdeck-cli` | The `fleetdeck` binary: `up`, `deck`, `station`, `open`, and the hidden `shim` and `shim-exec` commands. |
| `fleetdeck-testkit` | Fake agents (`fake-claude`, `fake-codex`), recorders, the deterministic clock and ids, temporary nodes, fault injection. |

## 3. Data flow of one command

1. The board sends a command to the deck API. The deck checks the login, the scope and the CSRF token, gives the command a `command_id`, stores it as `pending` and sends it over the link.
2. The station's command router calls `SessionManager::submit`. Guards run first (auto-continue enable, drain).
3. `Store::decide` runs the decision in one SQLite transaction: it checks the policy and the session state, appends the events, stores the effects and the receipt (`manager/decide.rs`). A rejection is stored as a receipt too.
4. The effect worker claims the effect and does the external I/O through the session target (`manager/target.rs`): spawn, write to stdin, answer an approval. It confirms the effect or marks it `outcome_unknown`.
5. The adapter turns the provider output into canonical events. The station appends them to the event log.
6. The link sends the events to the deck with their `station_seq`. The deck folds them into its read model and acknowledges them after the commit.
7. The board receives the change over its WebSocket.

## 4. Link protocol (summary of ADR-004)

- Transport: TLS (rustls) with self-signed certificates from Ed25519 identity keys, pinned after pairing. A control WebSocket (JSON frames up to 1 MiB) and a data WebSocket (byte streams with credit flow control).
- Handshake: both sides send `Hello { proto_min, proto_max, features, role, node_id, link_epoch, nonce }` and sign the transcript with the TLS exporter value. The highest common version wins; the features decide the behaviour.
- Frames: `Request`, `Response`, `Event`, `Ack`, `SnapshotBegin`/`SnapshotSession`/`SnapshotEnd`, `Ping`/`Pong`, `Close`.
- Each method declares its direction, scope and feature (`crates/fleetdeck-link/src/methods.rs`, ADR-004 §9.6). A method whose feature was not negotiated is refused `feature_not_negotiated`. Example: `coordinator.query` and `coordinator.call` need `coordinator.v1`, so an N−1 peer without it cannot reach the coordinator forward (ADR-006 test 26).
- Commands carry `target_incarnation` and `retry_until`. The deck resends the same `command_id` after a reconnect and never after `retry_until`.
- Network scope: loopback, private LAN and tailnet addresses only (D13).

## 5. Durability rules

- **Event log first.** The station commits events, receipts and effects in one transaction before any external I/O.
- **Never resend.** An input whose fate is unknown stays `outcome_unknown`. A continuation is never a copy of an owner prompt.
- **Recovery at start.** `Store::recover` settles every claimed effect and every session with a live process from the last run. Under the direct host the processes died with the station, so their sessions become `lost`. Under the shim host, `recover_station` reattaches each shim with a higher generation, settles the stdin frames, restores the adapter checkpoints, and replays the output from the committed cursor.
- **Shim fencing.** Each attach advances a generation. Only the current generation commits output and moves the cursor; a shim acknowledges output only after the commit.
- **Revert.** A revert runs as a durable step machine (`reverts` table). A crash in a step that cannot be proven gives the state "reconciliation required", and input stays blocked.
- **Upgrade.** The snapshot of `station.db` (main, WAL, SHM) is taken while the station is stopped. After the new station opens the live database, a rollback never restores the snapshot.

## 6. Versions and compatibility

- Link: `proto_min..proto_max` plus feature flags. A deck and a station one build apart (N−1) use the shared features (`crates/fleetdeck-link/tests/version_skew.rs`).
- Stored data: new event fields use `#[serde(default)]`; unknown event kinds round-trip as `EventBody::Unknown`.
- Shim protocol: `wire::PROTOCOLS`; the station and a shim pick the highest common version. The trial of an upgrade refuses a build that cannot speak a running shim's protocol.
- Database: each migration list only grows. A database with a newer schema than the build refuses to open (`DbError::TooNew`).

## 7. Where the tests are

| Area | Tests |
| --- | --- |
| Decisions, effects, recovery | `crates/fleetdeck-station/src/manager/*tests.rs`, `crates/fleetdeck-station/src/store/*tests.rs` |
| Real station processes (SIGKILL, faults, local CLI, upgrade) | `crates/fleetdeck-cli/tests/station_*.rs` |
| ShimHost (ADR-005 §9) | `crates/fleetdeck-cli/tests/shim_*.rs`, `host::shim::tests`; the map is in `docs/v2/progress/m3-durable.md` |
| Adapters and recorded fixtures | `crates/fleetdeck-providers/tests/` |
| Link | `crates/fleetdeck-link/tests/` |
| Deck API and fleet services | `crates/fleetdeck-deck/tests/` |
| Board | `web/src/**/*.test.tsx` (unit), `web/e2e/*.spec.ts` (Playwright at a phone viewport against a fake deck) |
| Performance | `make perf` (`crates/fleetdeck-cli/tests/perf.rs`) |
| Live CLIs (opt-in) | `make smoke-live` ([LIVE-SMOKE.md](./LIVE-SMOKE.md)) |

## 8. Where each M3 feature lives

Use this table to find the code of a feature. The tests of a feature are next to its code, and its board flow is in `web/e2e/`.

| Feature | Station | Deck | Board |
| --- | --- | --- | --- |
| Workspaces (worktree, clone, batch) | `workspace/provision.rs`, `manager/workspaces.rs` | `http/workspace_routes.rs` | `web/src/spawn/`, `e2e/workspace.spec.ts`, `e2e/spawn.spec.ts` |
| Assign, tasks, mail | — | `store/fleet.rs`, `http/assign_routes.rs`, `http/fleet_routes.rs` | `web/src/assign/`, `web/src/fleet/`, `e2e/assign.spec.ts`, `e2e/fleet.spec.ts` |
| Coordinator MCP (ADR-006) | `coordinator/` (MCP ingress, bindings, tools, `forward.rs` link forward; `coordinator.query` and `coordinator.call` are gated by `coordinator.v1` in `crates/fleetdeck-link/src/methods.rs`), `manager/coordinator_command.rs` (`session.coordinator_renew`, `session.coordinator_revoke`; `check_spec` refuses a `SessionSpec.coordinator` that is not an owner start or whose provider handoff is unverified, DL-215), `coordinator/handoff.rs` (credential handoff into the child environment) | `store/coordinator.rs`, `store/coordinator_ops.rs`, `store/grants.rs`, `http/grant_routes.rs` | `web/src/session/commands.ts`, `web/src/fleet/FleetPanel.tsx` (grant renewal; in progress, lane B 6-5) |
| Plans | — | `store/plans.rs`, `http/plan_routes.rs` | `board/PlanLibrary.tsx`, `e2e/plans.spec.ts` |
| Usage meters | provider limits (`crates/fleetdeck-providers/src/codex/limits.rs`) | read model | `board/UsageMeters.tsx`, `e2e/usage.spec.ts` |
| Images and notifications | image input (`crates/fleetdeck-proto/src/input.rs`; Claude `control.rs`, Codex `rpc.rs`) | — | `session/` (images), `board/notify.ts`, `e2e/images.spec.ts` |
| Conflict radar | — (the deck reads the file touches from the events) | `store/conflicts.rs` | `board/ConflictRadar.tsx`, `e2e/conflicts.spec.ts` |
| Files, checkpoints, revert | `workspace/checkpoint.rs`, `workspace/restore.rs`, `manager/revert*.rs`, `manager/files.rs` | `http/file_routes.rs` | `web/src/session/`, `e2e/revert.spec.ts` |
| Import a CLI session | `manager/decide.rs` (`session.import`) | method table | `web/src/spawn/`, `e2e/spawn.spec.ts` |
| Human shell | `shell/`, `store/shell_effects.rs` (schema V16) | `http/shell_routes.rs` | `web/src/shell/`, `e2e/shell.spec.ts` |
| ShimHost | `manager/shim*.rs`, `store/shims.rs` | — | — |
| Auto-continue | `manager/auto_continue.rs`, `store/auto_continue.rs` | method table | `web/src/session/SessionView.tsx` |
| Drain and upgrade | `manager/drain.rs`, `upgrade.rs`, `local.rs` | — | — (CLI: `fleetdeck station drain`, `fleetdeck station upgrade`) |

The paths in the Station column are below `crates/fleetdeck-station/src/`, and the paths in the Deck column are below `crates/fleetdeck-deck/src/`, when no crate is named. The paths in the Board column are below `web/src/` or `web/`.
