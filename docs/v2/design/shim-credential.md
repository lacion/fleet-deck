# Session credentials on the shim host (DL-188 step 1)

- **Status:** Accepted, revision 5 (2026-10-15); revision 6 adds the implementation decisions of §9 (a code map found 16 mismatches with the code), checked by the advisor (Codex) on 2026-10-16; its four corrections are in §9.7, §9.9, §9.13 and §9.15. Round 5: PASS (owner decision after round 4: "Fix + round 5"); after the PASS, the architect made the missing-fence rule exact (§2.3). Revision 1: FAIL (§5). Revision 2: FAIL (§6). Revision 3: FAIL, 3 blockers (§7). Revision 4: FAIL, 2 blockers (§8).
- **Platform:** Linux only (the shim host runs only on Linux until shim parity step 4; the credential path needs `/proc/self/exe`, §2.1). On another OS a shim-hosted session gets no tools, as today.
- **Plan:** `docs/v2/design/shim-parity.md` step 1 (gaps G1, G2). Owner: "lets build them all".
- **Goal:** a session on the shim host gets a session credential like the direct host, so coordinator sessions start and the session tools (`publish_artifact`, `set_focus`, `ask_coordinator`; DL-165) work, also across station restarts; the credential never reaches argv, the launch spec, the shim journal, a file, a log or the station database.

## 1. Facts (corrected)

- Direct host: `prepare_fleet` / the coordinator start mint a `Handoff` (`coordinator/handoff.rs`): a token (`Secret`) and the MCP arguments from `mcp_args(provider, url)`, where `url` is `http://127.0.0.1:<port>/mcp`; the ingress (`coordinator/ingress.rs:98`) binds `127.0.0.1:0` at every station start, so the port changes; `CoordinatorLaunch::child_env` sets `FLEETDECK_COORDINATOR_TOKEN`; `Bindings::insert(digest, Binding { state: Pending })` keeps the binding in memory; the direct pump calls `bound_events` / `ended_events` (`manager/target.rs`) to activate and end bindings; `coordinator/jsonrpc.rs` refuses a call without an active binding.
- Shim host: `prepare_fleet(durable = true)` → `NoTools::Durable`. The shim client (`host/shim/client.rs:61`) writes one frame (header `LaunchSpec` with the full provider `env`, payload = the 32-byte attach key) and closes stdin, then reads one `LaunchReply`; the shim (`host/shim/server.rs`) spawns the provider and replies `Ready`. The shim journal (`host/shim/journal.rs`) records only `Created`, `SpawnAttempted`, `Fenced`; `LaunchSpec` is not journaled. The durable observer (`manager/shim_target.rs:~633`) calls neither `bound_events` nor `ended_events`.
- The station starts the shim as `<its own program copy> shim`: a newly launched shim is always the same build as the station. A shim from an older build only exists as a running process from before an upgrade, and it never receives a launch frame again.

## 2. Design

### 2.1 A two-step launch: the secret only after the shim proves its build

- The launch exchange becomes: (1) the station writes the `LaunchSpec` frame (no secret; `LaunchSpec` gains `secrets: bool`) and keeps stdin open; (2) the shim, before it spawns anything, replies one line `Hello { exe_sha256, protocol: 2 }` when `secrets` is true, where `exe_sha256` is the SHA-256 of its own executable file (read from `/proc/self/exe` on Linux, `std::env::current_exe()` elsewhere, at most 2 GiB: a debug build with all workspace features is over 512 MiB); (3) the station compares `exe_sha256` with the SHA-256 of the exact file it launched (it hashes that path once per station process; the service runs an immutable release copy, `<data>/station/bin/releases/<version>-svc-<ulid>/fleetdeck`): equal → it writes a second frame `Secrets { token }` (header only, max 1 KiB) and closes stdin; different (another artifact, even with the same version string) or no `Hello` within the launch timeout → it closes stdin without a second frame, kills the shim (the existing launch-failure path) and the start fails with `NoTools::Durable` (the session then starts without tools on a fresh launch, as today); (4) the shim reads `Secrets`, then spawns the provider and replies `Ready` as today.
- **One executable object, never a path:** the station launches the shim as `/proc/self/exe shim …` (the exact program object of the running station, not the release pathname); the shim starts the provider's helper as `/proc/self/exe shim-exec …` (its own program object), never `current_exe()` or a stored path. `/proc/self/exe` keeps naming the executed object even if its path is renamed or replaced. The SHA-256 in `Hello` is computed by reading `/proc/self/exe` in the shim and compared with the station's own `/proc/self/exe`; with the same object both are equal, and a mismatch (an unexpected object) stops the launch as above.
- `secrets: false` (a session without tools) keeps today's one-frame exchange.
- The shim keeps the token only in memory (`Secret<String>`), adds `FLEETDECK_COORDINATOR_TOKEN` to the environment of the `shim-exec` call for the provider, and drops it after the spawn. It never writes it to the journal, the key file, a reply line or a log.

### 2.2 A stable tool endpoint: the MCP bridge on a station socket

- The station listens, in addition to its TCP ingress, on a Unix socket at a short, fixed path chosen by the same rules as the shim sockets (`host/shim/journal.rs` `socket_path`, ADR-005 §5.1): `$XDG_RUNTIME_DIR/fleetdeck/<incarnation>/mcp.s`, else `<station dir>/run/mcp.s`, else `${TMPDIR:-/tmp}/fd-<uid>-<incarnation>/mcp.s`, the first that fits `sun_path`; none fits → the shim host gives no tools (`NoTools::Durable`) and logs `socket_path_too_long`. The directory is created 0700 and checked (owned by the station user, mode 0700, not a link) before the bind; the socket is 0600. The path depends only on the incarnation and the directories, so it is the same at every start; the station removes a stale socket file at start after the ownership check.
- A shim-hosted provider gets MCP arguments that start a stdio MCP server: `<station program copy> mcp-bridge --socket <path>` (Claude: an `mcpServers.fleetdeck` entry of type `stdio` with that `command`/`args`; Codex: `mcp_servers.fleetdeck` with `command`/`args` and `env_vars = ["FLEETDECK_COORDINATOR_TOKEN"]`, because Codex passes only whitelisted variables to a stdio server; Claude passes its environment to the server). The bridge reads the token from `FLEETDECK_COORDINATOR_TOKEN` (inherited from the provider), connects to the socket per request (one connection per JSON-RPC message, the token in the first line `AUTH <token>`), and relays the message and the answer. When the station is down, the bridge answers a JSON-RPC error "Fleet Deck is restarting; try again in a moment." and keeps running.
- The station's socket handler runs the same JSON-RPC code as the TCP ingress, with the same token-digest check and limits.
- The direct host keeps its TCP endpoint (no change to the verified direct handoff).

### 2.3 Bindings: durable digests, transitions inside the output transaction, fail closed

- New station table `shim_bindings (digest TEXT PRIMARY KEY, shim_id TEXT NOT NULL, session_id, binding_id, grant_id, provider_instance, native_id NULL, generation INTEGER, classes TEXT, scope TEXT, state TEXT, control_skill TEXT, created_at INTEGER)`: the SHA-256 digest of the token, never the token, tied to the shim execution (`shim_id`).
- **Compute, then apply:** the binding helpers are split into a pure `transitions(rows, events) -> Vec<Transition>` (no memory change) and an `apply(&Bindings, &[Transition])`.
- **Transitions inside the output transaction:** the durable driver's output commit (`manager/durable.rs` `commit`, which already writes the events, the checkpoint and the cursor in one transaction) computes the transitions of the batch (`bound` on `session.started` with the native identity; `ended` on the process end, a loss, or a native replacement) and writes the row changes and their lifecycle events in the same transaction. A crash before the commit loses both the events and the transitions; the replay after restart sees the events again and computes the same transitions. There is no separate observer step for bindings.
- **Fail closed:** an invalidation (`ended`, rotation of the old token) is applied to memory *before* its transaction (the old token stops working at once); if the transaction fails, memory stays invalidated and the session has no tools until the next start reloads the rows. For a revoke and a rotation, the durable fence intent comes before that transaction (below). An activation (`bound`, a new token's row) is applied to memory only *after* its transaction committed. A failed launch insert → the start goes on without tools.
- **Root identity changes are signals in the transaction:** the Claude and Codex cores emit the ADR-006 §5.6 adapter output `NativeIdentity { native_id }` for every root identity statement (Claude: every root `system/init`; Codex: `thread/started` and each root `turn/started`); the shim drivers (`manager/shim_claude.rs`, `shim_codex.rs`) pass these signals, in order with the events, to the durable step; `transitions` turns a different root identity into `ended` (`native_replaced`) for the bindings of that shim, written in the same output transaction as the checkpoint and cursor. Identical identities and child identities produce no transition.
- **Invalidation intent first (write-ahead fence):** an invalidation that is not provider output is an owner revoke or a rotation by resume or relaunch. Grant expiry needs no fence: the expiry time is durable in the grant row, and startup loads every binding of an expired grant as `ended`. Provider-output invalidations need no fence: the replay recomputes them from the committed bytes. A revoke or a rotation runs these steps in this order:
  1. Gate memory: the old token is refused at once.
  2. Record the intent durably in `<station dir>/run/bindings.fence` (mode 0600). The station writes the complete new content (the header `fleetdeck-fence 1`, then one line `<shim_id> <digest>` for each entry, the existing entries kept) to `bindings.fence.tmp` in the same directory, fsyncs the file, renames it over `bindings.fence`, and fsyncs the directory. One writer at a time (a station mutex holds steps 2 to 4 of one operation). At most 4096 entries.
  3. Only after step 2 succeeded: the row change and its event in one transaction.
  4. After that transaction committed: remove the entry with the same atomic write. A failed removal leaves the entry, which is harmless (the row is already `ended`).
- **A failed intent fails the operation:** when step 2 fails (a write, fsync, rename or directory fsync error, or the entry limit), the station does not start step 3. The revoke answers the owner with the error "The revoke was not saved. The old token is blocked until Fleet Deck restarts. Try again." A resume or relaunch is refused with "The station cannot save the session credentials. Try again." and starts no new provider. The memory gate stays. The station never reports an invalidation as done unless step 2 or step 3 is durable. It does not use a provider stop as a substitute for durable evidence.
- **Startup reads the fence before the rows:** every reattached binding named in the fence loads as `suspended` (refused). If the fence cannot be read or parsed, every reattached binding loads as `suspended`. The station creates the fence (the header only, with the same atomic write) before it stores the first `shim_bindings` row, and never deletes the file. So a missing fence with no `shim_bindings` rows is a first start (the station creates it); a missing fence with rows counts as unreadable. A suspended binding never becomes active again; the session gets tools back only through a resume or relaunch with a new token. A leftover `bindings.fence.tmp` is deleted (the rename did not happen, so its content was never in effect).
- **Startup:** the shim host loads `shim_bindings` rows of the reattached shims into `Bindings` before any observer or socket accepts a call; until that load (and the replay of the committed-but-unobserved events) finishes, the ingress refuses every shim-hosted call with "Fleet Deck is starting; try again in a moment." Rows of shims that are not reattached are deleted in one transaction.

### 2.4 Coordinator sessions

- A coordinator grant (`GrantScope` other than the session's own tools) uses the same launch, bridge and rows. The refusal of a shim-hosted coordinator start is removed.

## 3. Security notes

- The token exists only in: the station's memory during the launch, the second launch frame on the shim's stdin (a pipe between two processes of the same user), the shim's memory until the spawn, the provider's environment and the bridge's environment (the same exposure as the direct host's environment today).
- Never in: argv (the bridge reads it from its environment), `LaunchSpec`, the shim journal, the key file, a reply line, logs, the station database (digest only), the board or the deck.
- The bridge socket is in a checked 0700 directory: no other user can connect or bind it; the token is still required on every request.
- The token goes only to the exact artifact the station launched (SHA-256 of the executable file).
- A shim of another build never receives the token (the `Hello` build check, before the provider exists).

## 4. Tests

- Leak tests: decode `LaunchSpec.args` and `env` byte vectors and assert that no value equals or contains the token and that `FLEETDECK_COORDINATOR_TOKEN` is absent; prove the test by planting the token in a test copy and seeing it fail; raw-byte scans of the journal, the key file, the reply lines, the captured logs and the database file.
- The two-step launch: a `Hello` with another build → no second frame is written (the fake shim records every byte it received: zero token bytes) and the start fails; a missing `Hello` (timeout) → the same.
- The provider sees the token (a fake provider prints its digest); the bridge relays a tool call; after a station restart (the shim and the provider keep running) the next tool call through the bridge succeeds, with the binding restored from its row; while the station is down the bridge answers the restart error.
- Lifecycle: first activation on `session.started`; end on exit and on loss; a native replacement ends the old binding; events that arrive while the station was down are applied in order at replay.
- Crash points: kill the station after each transaction step (launch insert, `bound`, rotation, end) and check after restart that only the expected token works (old and new token both tried).
- A coordinator session on the shim host starts and its grant tools answer through the bridge.
- Identity: two shim executables with the same version string but different bytes → the second gets no token.
- Crash between the source-event commit and anything after it: the transitions are in the same transaction (a test injects a failure after the events are written but before the commit, and one after the commit; the binding state matches the event log in both).
- Database-error injection on each invalidation: the old token is refused at once and stays refused.
- Socket path: a station directory where `station.sock` fits but `<station dir>/run/mcp.s` does not → the `/tmp` fallback; a fallback directory owned by another user or with a loose mode → refused.
- Executable identity: replace the release file after the station started and before a launch, and between `Hello` and `Secrets`: the shim and `shim-exec` still run the original object (assert by the hash a fake provider reports) and the token never reaches the replacement.
- Root identity: an identical repeated root `system/init` → no transition; a different root identity → `native_replaced` in the same transaction; a child identity → none; a crash right after the replacement batch → after restart the old binding is ended.
- Fence: (a) a crash after step 2 and before step 3, then reattach → the old token is refused (suspended); (b) a database failure in step 3, then a crash, then reattach → refused; (c) a failure in each part of step 2 (write, file fsync, rename, directory fsync, the entry limit) → no transaction is started, the revoke answers the fixed error, a resume or relaunch starts no provider, the memory gate refuses the old token; (d) a crash between the temp write and the rename → the old fence is in effect, the temp file is deleted at startup, and the other entries are kept; (e) a fence that does not parse → every reattached binding is suspended; (f) a binding of a grant that expired while the station was down loads as `ended`; (g) the entry is removed after step 3, and a failed removal keeps a harmless entry.
- Live smoke (opt-in, owner-run, like the existing live smokes): real Claude and real Codex sessions on the shim host call `set_focus` through the bridge, before and after a station restart.

## 5. Review map (revision 1 → revision 2)

| Finding | Answer |
| --- | --- |
| BLOCKER capability negotiation after the secret | §2.1 two-step launch: `Hello` with the build before any secret and before the spawn; a fresh launch always runs the station's own build |
| BLOCKER the endpoint port changes at restart; port takeover | §2.2 a stable Unix socket in the station's 0700 directory, reached through `fleetdeck mcp-bridge`; no TCP port for shim-hosted sessions |
| MAJOR no lifecycle integration on the durable path | §2.3 the observer calls `bound_events` / `ended_events`; restore before replay |
| UNVERIFIED no commit boundary for rotation and revoke | §2.3 one transaction per change, memory after commit, fail closed, `shim_id` in the row; §4 crash-point tests |
| MAJOR the byte-string leak test can miss the token | §4 decode the byte vectors, check the env key, prove the test with a planted token |
| MINOR wrong journal fact | §1 corrected |

## 6. Review map (revision 2 → revision 3)

| Finding | Answer |
| --- | --- |
| B1 observer transitions missed across a crash | §2.3 transitions computed and written inside the durable output transaction; authentication waits for startup replay |
| B2 not fail-closed on invalidation | §2.3 compute/apply split; invalidations applied to memory before the transaction; activations after the commit |
| B3 the version string is not an artifact identity | §2.1 `Hello` carries the SHA-256 of the shim's executable; the station compares it with the file it launched |
| B4 Codex does not pass the token to a stdio server | §2.2 `env_vars = ["FLEETDECK_COORDINATOR_TOKEN"]` for Codex; §4 live smokes for Claude and Codex |
| B5 the socket path can exceed `sun_path` | §2.2 the ADR-005 §5.1 short-path rules with a checked 0700 directory; §4 length and ownership tests |

## 7. Review map (revision 3 → revision 4)

| Finding | Answer |
| --- | --- |
| BLOCKER a failed invalidation can return after a crash | §2.3 invalidation fence file: reattached bindings of fenced shims load suspended and never reactivate; a failed fence write stops the provider |
| BLOCKER `shim-exec` reopens a pathname after the check | §2.1 the station runs `/proc/self/exe shim`, the shim runs `/proc/self/exe shim-exec`; no path is reopened; Linux only |
| BLOCKER root identity replacement emits no event | §2.3 the ADR-006 §5.6 `NativeIdentity` adapter output flows through the durable step and ends the binding in the same transaction |

## 8. Review map (revision 4 → revision 5)

| Finding | Answer |
| --- | --- |
| BLOCKER the fence is recorded too late | §2.3 write-ahead order: memory gate, then the durable fence intent (temp file, fsync, rename, directory fsync, existing entries kept), and only then the transaction; startup reads the fence before the rows |
| BLOCKER the fallback trusts an unconfirmed stop | §2.3 no fallback: a failed intent fails the revoke or the rotation with a fixed error, and the station never reports an invalidation that is not durable; a stop is never used as evidence |

## 9. Implementation decisions (revision 6)

A read-only code map (2026-10-15) compared §1–§4 with the code. These decisions win over the earlier text where they differ.

1. **Grant expiry.** The station stores no grant row. `shim_bindings` gets a column `expires_at INTEGER NULL` (Unix ms, from `CoordinatorSpec.expires_at`; `NULL` for a fleet-only binding). Startup loads a row whose `expires_at` is in the past as `ended` (`expired`).
2. **Socket path.** The candidates, in order: `$XDG_RUNTIME_DIR/fleetdeck/<incarnation>/mcp.s`; `<station dir>/run/mcp/mcp.s` (a dedicated directory, created and checked as 0700); `${TMPDIR:-/tmp}/fd-<uid>-<incarnation>/mcp.s`. The first that fits `sun_path` and passes the directory check. A new function beside `journal::socket_path`; that function does not change.
3. **The bridge program.** The provider runs the bridge, so it cannot be `/proc/self/exe`. The program is the stable service link `<data>/station/bin/current` (not resolved), so an upgrade keeps it valid for a long-lived session; without a service install (a development run) it is `current_exe()`. The bridge wire format is fixed as version 1 (`AUTH <token>` line, then one JSON-RPC line; the answer is one JSON line or EOF), so a newer bridge talks to an older station and the reverse.
4. **The hash compared at launch.** The station hashes `ShimHostConfig.exe`, which is `/proc/self/exe` in production and the test binary in tests.
5. **A failed check.** On a wrong or missing `Hello`, the station closes stdin, kills the shim and waits for it (explicitly: today the launch error paths do not kill). The shim exits when stdin closes before `Secrets`. The start fails with the error "The shim could not be verified." There is no automatic relaunch without tools.
6. **Tool arguments at start time.** `ShimHost::prepare` does not put the bridge arguments into the spec. `start_with(prepared, Some(LaunchSecrets { token, extra_args }))` adds them; a failed launch insert of the binding row → `start_with(prepared, None)` (no tools, the one-frame exchange).
7. **Replay finished.** After attach, the host snapshots the last complete-line boundary of the shim output that is buffered at that moment (from `Status { end }`, cut back to the last newline; the driver commits complete lines only). The bindings of a reattached shim stay held (the ingress answers "Fleet Deck is starting; try again in a moment.") until the driver has committed output up to that boundary and the sink has applied its transitions. A shim end releases the hold only after its `ended` transition is durable; it never releases a binding to `active` by itself.
8. **Start order.** `Bindings` exists before `recover_station`; the fence and the rows load first; the binding sink is registered on the store before any driver restores.
9. **Codex root identity.** The first root identity comes from the matched thread-open answer (`thread/start` or `thread/resume`). A later `thread/started` is classified by `thread.id` and its `parentThreadId` / `source` metadata (the vendored 0.160.0 schema marks a subagent thread); a missing entry in `child_threads` is not proof of a root.
10. **Codex `env_vars` is not verified.** The bridge mode starts for Claude only. Codex shim sessions keep `NoTools::Durable` until a live smoke (an ignored test with a run command for the owner) proves that Codex passes `FLEETDECK_COORDINATOR_TOKEN` to a stdio server; then a one-line gate change enables it. **Update 2026-10-16:** the bridge mode is gated by `BRIDGE_VERIFIED` (the versions whose stdio bridge passed the live smoke `live_bridge`); Codex 0.162.1 passed and is listed.
11. **Suspended in memory.** The row state `suspended` loads into memory as `BindingState::Revoked` (terminal, refused). No new `BindingState` variant.
12. **Rotation.** The only rotation path is a resume (`target.start` → `resume_durable`); a resume is fleet-only (`resume.rs` sets `coordinator: None`). There is no separate relaunch path.
13. **Revoke.** All three steps run inside the admitted decision (after pairing, receipt deduplication and freshness checks, so a rejected command changes nothing): the memory gate and the durable fence write (step 2) come first inside the decide write, then the row end (step 3) in the same transaction; a failed fence write returns the error and the transaction rolls back. Step 4 (fence entry removal) runs after the decision committed. The decision reads the durable rows, not memory, because memory is already gated.
14. **Launch protocol constant.** `LAUNCH_PROTOCOL = 2`, separate from the attach `wire::PROTOCOL`.
15. **Store sink.** The output transaction calls a `BindingSink` registered on the store: `invalidate` inside the write closure before the commit; `activate` on the database writer thread directly after the commit and before the next write job, so a later transaction cannot be overtaken by an earlier activation. `activate` never revives a binding that is terminal in memory or still held by the replay gate. The driver constructors do not change.
