# Effect v4 + Bun — implementation plan

*Executable companion to [Effect v4 on Bun — adoption decision](./effect-feasibility.md) and the
v1 [plan of record](./README.md). This document is intended to be handed directly to a Codex goal
and updated as each gate lands.*

**Status:** P0–P5 checkpointed; P5 complete at `ca62b94f`; P6 in progress (P6.1–P6.3 and P6.7 done; P6.8 harness landed, baseline pending)
**Working branch:** `fd/v1-effect-feasibility`
**Starting point:** v0.23.6
**Runtime floor:** exact Bun 1.3.14 in CI; `engines.bun >=1.3.14`
**Effect cohort:** exact `4.0.0-rc.110`
**Migration posture:** complete the application architecture, but replace optional platform
adapters only when their own behavior and performance gates pass

Current continuation state is recorded in [effect-migration-status.md](./effect-migration-status.md).

## 1. Goal contract

### Objective

Move Fleet Deck's daemon to one Effect v4 application runtime, backed deliberately by Bun-native
APIs, without changing public behavior. When this plan is complete:

- one root Effect program owns every daemon-lifetime resource;
- daemon asynchronous workflows use Effect for cancellation, concurrency, scheduling, and typed
  operational errors;
- one-shot subprocesses use a parity-proven `Bun.spawn` adapter; long-lived tmux is Effect-owned
  and uses Bun transport only if its independent stream gate passes;
- native `Bun.serve`, native WebSockets, and `bun:sqlite` remain visible, optimized platform
  boundaries under Effect ownership;
- detached Promises, orphan timers, and unowned children have reached zero, except for an explicit
  reviewed allowlist whose entries are proven process-independent;
- hook fail-open behavior, HTTP/WS contracts, persistence, terminal behavior, and exit codes are
  unchanged;
- the board, shared contracts, and thin hook shims contain no Effect runtime code;
- source, generated bundles, macOS, Linux, real tmux, lifecycle, leak, and performance gates are
  green;
- each optional Bun/Effect adapter trial has either landed or has a checked-in evidence-backed
  **KEEP** decision behind the same service boundary.

### What counts as completion

Completion is not measured by Effect import count or converted line count. It is the conjunction
of these outcomes:

1. **Ownership:** root-scope closure deterministically stops intake, releases held hooks, closes
   viewers and network resources, interrupts and joins background work, closes SQLite, and removes
   only owned process state.
2. **Behavior:** source and bundled behavior remains compatible at every external boundary.
3. **Bun use:** every runtime capability has an explicit migrate/keep/defer decision supported by
   semantics and, where material, representative measurements.
4. **Maintainability:** application services have Live and test layers; pure functions stay plain;
   there is one main runtime and no scattered `Effect.run*` calls.
5. **Release evidence:** the approved RC, unstable imports, costs, platform results, and rollback
   facts are checked in.

An optional candidate such as `Bun.udpSocket`, `@effect/sql-sqlite-bun`, Effect's Bun HTTP server,
or `bun build --compile` can finish as **KEEP/DEFER** when its named gate fails. The goal is still
complete if Effect owns that resource through Fleet Deck's proven adapter and the decision record
contains reproducible evidence.

### Hard non-goals

- No user-visible feature work while this migration is in flight.
- No public wire-format or database-schema redesign.
- No Effect import in `contracts/`, `board/`, or the small `scripts/fleet-*.ts` hook floor.
- No wrapping of pure reducers, parsers, row mapping, or security predicates merely for style.
- No Vitest migration and no `@effect/vitest`; retain `bun:test`.
- No Bun Shell for executable invocation; argv/no-shell execution is a security invariant.
- No telemetry backend, phone-home behavior, RPC, cluster, workflow, AI, or persistence subsystem.
- No test quarantine, weakened assertion, increased timeout, or fixture rewrite to hide drift.

## 2. Architectural invariants

### One runtime, one owner

The steady-state topology is:

```text
src/daemon/fleetd.ts
  BunRuntime.runMain(DaemonApp + LiveLayer)
                       │
                       ▼
             one root Scope / supervisor
       ┌──────────┬──────────┬───────────┬───────────┐
       ▼          ▼          ▼           ▼           ▼
  Bun process  Bun HTTP   bun:sqlite   discovery   schedules /
    service     + WS       service      adapter     terminal fibers
       │          │          │           │           │
       └──────────┴──────────┴───────────┴───────────┘
                       │
                       ▼
         plain domain functions and wire DTOs
```

`fleetd.ts` becomes a thin host entrypoint. Service construction belongs in Layers. Application
workflows yield services. Native adapters translate Bun callbacks, streams, and synchronous calls
into Effects. Domain functions receive and return ordinary values.

The only permitted imperative runtime crossings are:

- `BunRuntime.runMain` in the daemon entrypoint;
- one temporary `execFileP` Promise facade while legacy callers migrate;
- one native-adapter bridge that captures the already-built root `Context` and invokes route
  Effects with `Effect.runForkWith`, `Effect.runCallbackWith`, or `Effect.runPromiseWith` when a
  Bun callback cannot return an Effect directly.

Every temporary bridge is listed in the migration ledger with an owner and deletion work package.
Do not add `Effect.runPromise` or `Effect.runFork` in route, store, poller, terminal, or workflow
modules. `ManagedRuntime` is permitted only for P3's pre-root `BootstrapRuntimeBridge`, when the
legacy daemon has no Effect root. P4 disposes and deletes that bridge before installing the root
runtime. A steady-state callback adapter must never construct a `ManagedRuntime`, rebuild
`LiveLayer`, or own a second Scope.

Native callbacks and the temporary Promise facade converge on one `IngressSupervisor`:

- it is created inside the root program from the already-built Context and never provides or
  rebuilds `LiveLayer`;
- every request, WebSocket callback, or legacy-facade Effect submitted from imperative Bun code is
  tracked as root-owned ingress work;
- quiesce refuses new submissions, then drain/close waits for or interrupts tracked fibers before
  HTTP/Store finalization;
- its runner functions are the only `Effect.run*With` calls after P4 and are confined to the
  platform ingress module;
- before P4 exists, P3 may use one temporary `BootstrapRuntimeBridge` built from the ProcessRunner
  Layer only. P4 deletes/rebinds it to `IngressSupervisor`; it must never survive beside the root
  runtime.

P13 removes the `execFileP` facade. If the custom `Bun.serve` adapter remains, the
`IngressSupervisor` remains as the single host-callback edge—not as a second runtime or resource
graph.

### Target source layout

Create this structure incrementally; do not move pure modules just to match it:

```text
src/daemon/
  fleetd.ts                       # BunRuntime.runMain only after P4
  app/
    program.ts                    # DaemonApp
    live-layer.ts                 # production composition
    errors.ts                     # startup/application tagged errors
    shutdown.ts                   # explicit quiesce/close state machine
    services/
      process-runner.ts
      store.ts
      http-server.ts
      discovery.ts
      terminal.ts
      background.ts
      core.ts
  platform/bun/
    process-runner-live.ts        # direct Bun.spawn
    http-server-live.ts           # current Bun.serve, later platform trial
    sqlite-live.ts                # direct bun:sqlite lifetime
    udp-live.ts                    # only if the multicast gate passes
    content-files-live.ts         # only proven Bun.file/Bun.write uses
  adapters/
    http-policy.ts                # errors/results -> exact Response
    hook-policy.ts                # canonical fail-open recovery
  ...existing pure and compatibility modules...

tests/
  effect/                         # clock/layer/resource policy units
  ...existing black-box suites remain authoritative...
```

### Import boundaries

- `app/` may import Effect, services, domain modules, and adapter interfaces.
- `platform/bun/` may import Bun APIs, selected `node:` compatibility APIs, and Effect.
- domain modules may not import `app/` or `platform/`.
- `contracts/`, `board/`, and thin hooks may not import `effect`, `@effect/*`, `app/`, or
  `platform/`.
- native implementations are selected in `live-layer.ts`, never inside domain workflows.
- tests can inject service Layers; they should not patch globals when a service seam exists.

Extend both Biome restrictions and `tests/import-boundaries.test.ts`. Also scan generated board and
hook output for `effect`/`@effect` package markers so a type-only source mistake cannot become a
bundled regression.

## 3. Exact dependency and API policy

### Initial dependencies

Add with exact versions and commit `bun.lock`:

```sh
bun add --exact effect@4.0.0-rc.110 @effect/platform-bun@4.0.0-rc.110
```

The initial daemon dependency budget is those two packages. Add
`@effect/sql-sqlite-bun@4.0.0-rc.110` only inside P8 if its independent SQL gate chooses it. An
extracted Fleet Deck Bun-platform package may replace `@effect/platform-bun` or become one additional
direct dependency only after P11.10; app-local adapters add no package. The provisional ceiling is
four direct Effect/platform packages only if both the SQL driver and one extracted native-platform
package independently win their gates. Anything beyond that requires an explicit dependency-budget
decision. Do not add `@effect/platform`, `@effect/sql`, or `@effect/experimental`; those names refer
to the older package layout.

Add a repository check that:

- registry dependency versions are exact strings; an approved fork is pinned by immutable commit,
  never a branch;
- every resolved v4 `effect` or `@effect/*` package is on the approved RC, except an explicitly
  registered Fleet Deck fork whose package version and upstream base are that same RC;
- `@effect/platform-node-shared`, pulled by a caret from platform-bun, is locked to the same RC;
- CI installs without changing the lockfile;
- an RC upgrade changes the whole cohort in one isolated commit.

### Approved v4 vocabulary

The implementation must use APIs from the selected RC, not v3 muscle memory:

| Need | v4 RC.110 API | Reject in review |
|---|---|---|
| Service tag | `Context.Service` | `Context.Tag`, `Context.GenericTag`, `Effect.Tag`, `Effect.Service` |
| Acquired Layer | `Layer.effect` / `Layer.effectContext` | `Layer.scoped` / `Layer.scopedContext` |
| Callback adapter | `Effect.callback` | `Effect.async`, `Effect.asyncEffect` |
| Structured child | `Effect.forkChild`, `Effect.forkScoped`, `Effect.forkIn` | old `Effect.fork` |
| Detached fiber | `Effect.forkDetach`, only by reviewed exception | old `forkDaemon` or untracked detachment |
| Error recovery | `Effect.catch`, `Effect.catchTag`, `Effect.catchTags`, `Effect.catchCause` | `catchAll`, `catchAllCause` |
| Scope provision | `Scope.provide` | `Scope.extend` |
| Root callback bridge | `Effect.runForkWith`, `Effect.runCallbackWith`, `Effect.runPromiseWith` over the captured root Context | a second `ManagedRuntime` or old generic `Runtime<R>` examples |
| Pre-root P3 bridge | one explicitly disposable `ManagedRuntime.make(ProcessRunnerLive)` | any bridge that survives P4 |
| Time tests | `TestClock` from `effect/testing` | a new test runner |

Prefer direct auditable imports such as `effect/Effect`, `effect/Layer`, `effect/Context`,
`effect/Stream`, and `@effect/platform-bun/BunRuntime`.

### Unstable import register

Create `docs/v1/evidence/effect/unstable-imports.md` and update it whenever an unstable import is
added:

| Area | Allowed candidate | Default decision |
|---|---|---|
| Process | `effect/unstable/process/*` | Comparison API only until it matches the Fleet Deck process contract; custom `Bun.spawn` service is the default |
| HTTP/router | `effect/unstable/http/*` | Allowed for application routes after P6 characterization; transport switch is separately gated |
| Socket | `effect/unstable/socket/*` | Allowed only in the P7 stream/terminal trial; `BunSocketServer` re-exports the Node-shared server and does not count as Bun-native |
| SQL | `effect/unstable/sql/*` plus matching Bun driver | Allowed only in P8's benchmark and semantics branch |
| Schema | core `effect/Schema` for daemon-internal boundaries | Do not replace shared contract validators incidentally |
| Observability | `effect/unstable/observability/*` | Deferred; no backend during this migration |
| Everything else | cluster, RPC, workflow, AI, persistence, eventlog, workers | Out of scope |

Each row records exact imports, why core APIs were insufficient, owning tests, last reviewed RC,
and rollback module.

### Owning a truly Bun-native Effect platform

Fleet Deck is willing to implement, maintain, and upstream missing Bun-native Effect adapters.
That permission is explicit, but extraction follows proof rather than preceding it:

1. build the first adapter app-locally under `src/daemon/platform/bun/` against Fleet Deck's exact
   behavior contract;
2. add conformance tests against the RC.110 service interface and differential Fleet Deck tests;
3. confirm the gap is general to Bun, not a Fleet Deck-only policy such as the combined output cap;
4. prepare an upstream-ready patch for Effect where the generic implementation belongs; the
   parity-proven app-local adapter is a valid mandatory terminal state;
5. submit upstream, extract a separately versioned Fleet Deck package, or consume a pinned full
   fork only after the user approves that specific external/maintenance/dependency choice. Silence
   or unknown upstream timing is never treated as approval.

The first candidate is a real Bun implementation of
`effect/unstable/process/ChildProcessSpawner` backed by `Bun.spawn`. A future native socket-server
adapter is another candidate because RC.110's `BunSocketServer`, like its child-process adapter,
re-exports the Node-shared implementation. Do not claim or publish under the upstream `@effect`
namespace without upstream ownership; use a Fleet Deck scope or a pinned source fork.

If multiple generic Bun gaps prove real, a Fleet Deck-maintained **full fork of
`@effect/platform-bun`** is allowed and preferable to a pile of unrelated shims. Start from the
selected upstream tag, preserve its public exports and conformance suite, replace only the proven
Node-shared delegates with Bun-native implementations, and consume the fork by immutable commit.
Record the upstream base, patch series, checksum, rebasing owner, and rollback to the official
package. Owning a fork does not authorize publishing into Effect's npm namespace; upstreaming or
publishing any package remains a separately approved external action.

An owned package must have:

- exact peer compatibility with one Effect RC cohort and a matching upgrade/conformance matrix;
- no Fleet Deck domain policy in the generic adapter;
- Bun 1.3.14 floor plus latest-stable compatibility coverage;
- API, interruption, stream/backpressure, finalizer, and natural-process-exit tests;
- upstream source/license attribution and a documented security/release owner;
- for a full fork, an immutable source pin, reproducible package contents, and an upstream-rebase
  ledger with every Fleet Deck patch reviewable in isolation;
- an app-local fallback/rollback seam until the extracted package has shipped through a release.

## 4. Bun-native capability register

The Bun Native APIs review is a required gate, not a slogan. Update this table with measured
outcomes as work lands.

| Capability and current callsite | Candidate | Plan decision | Proof required |
|---|---|---|---|
| One-shot execution in `exec.ts` | direct `Bun.spawn` | **MIGRATE FIRST** | Exact result parity, shared output cap, Web-stream draining, live env, abort/timeout race, TERM-to-KILL, descendant cleanup, zero leaks |
| Bounded git execution in `files.ts` | shared Bun process service | **MIGRATE AFTER EXEC** | Preserve partial output, truncation, stdin early-close, exit 1 semantics, and two-search concurrency |
| tmux control client in `termbridge.ts` | `Bun.spawn` long-lived Web streams/FileSink | **EFFECT-OWN; TRIAL BUN IN P7** | Protocol/UTF-8/order/backpressure fixtures plus real-tmux soak; a scoped Node-stream adapter is a valid KEEP outcome |
| Sync CLI capability checks | `Bun.spawnSync` | **SEPARATE CLI-ONLY PARITY TRIAL** | Exit/stdout/stderr/self-path parity and latency; otherwise keep `execFileSync` |
| Cached sync tmux probe inside the daemon | async `Bun.spawn` Effect fiber | **CHARACTERIZE SEPARATELY** | Do not promote `Bun.spawnSync`; preserve cache/readiness and avoid blocking `/health` |
| Detached supervisor/CLI launchers | `Bun.spawn` | **KEEP INITIALLY** | Child survival, `unref`, stdio, signal, and supervisor identity tests before conversion |
| HTTP and server WS in `http.ts` | current `Bun.serve`; optional `BunHttpServer` | **KEEP CUSTOM ADAPTER** (P6.3 Effect-owns `Bun.serve`; P6.7 rejected `BunHttpServer` at rc.110) | Frozen HTTP/WS suite and graceful close remain the contract. Trial evidence: [p6-transport-trial.md](./evidence/effect/p6-transport-trial.md) |
| Static board assets | `Bun.file` Response | **BENCHMARK LATE** | Missing/traversal/MIME/cache/CSP/HEAD/range parity and useful measured gain |
| SQLite seam | static `bun:sqlite`, `strict: true` | **MIGRATE STALE SEAM; KEEP DIRECT BY DEFAULT** | Binding, null normalization, integers, WAL/busy, migrations, permissions, durability, performance |
| Repeated SQL statements | `db.query()` cache | **INVENTORY/BENCHMARK** | Query lifetime and close tests; do not mechanically replace `prepare()` |
| mDNS | `Bun.udpSocket` | **ISOLATE/TRIAL** | Two responders on 5353, interface membership/egress, TTL 255, send backpressure, goodbye completion, roaming, macOS+Linux |
| Content reads/writes | `Bun.file` / `Bun.write` | **SELECTIVE** | Equal lazy-error, limit, atomicity, permission, and durability semantics |
| Directories/metadata/permissions/atomic fd I/O | `node:fs` | **KEEP** | Bun APIs do not replace exact `O_NOFOLLOW`, chmod, fsync, link/rename, random-access, or symlink-safe requirements |
| UUID/random data | Web Crypto | **OPTIONAL CLEANUP** | Fixed shape/entropy tests; no behavior or material performance regression |
| Token compare and exact SHA-256/base64url | `node:crypto` | **KEEP** | Never substitute non-cryptographic `Bun.hash` |
| Daemon/bin/hook bundles | programmatic `Bun.build({ target: "bun", format: "esm" })` | **TRIAL IN P12** | Determinism, banners, builtins, dynamic imports, source/bundle parity, payload integrity |
| Standalone executable | `bun build --compile` | **DISTRIBUTION GATE AFTER BUN.BUILD** | macOS arm64 + Linux x64, assets, writable DB, tmux/git, signals, takeover, self-path, size/start/RSS |
| Tests | `bun:test` | **KEEP** | Add Effect test services, not another runner |
| Environment | explicit `process.env`; consider `--no-env-file` | **KEEP AND CHARACTERIZE** | Service env vs Bun auto-loaded `.env` precedence; no bundled secrets |
| Paths, URLs, and OS metadata | `node:path`, `node:url`, `node:os` | **KEEP** | Mature complete semantics; no Bun-native performance win to prove |
| Incremental UTF-8 decoding | `node:string_decoder` | **KEEP BY DEFAULT** | Terminal fragmentation semantics are proven; replace only by byte-for-byte decoder tests |
| Web primitives | standard `fetch`, `Request`, `Response`, `AbortSignal` | **KEEP** | Portable native Web APIs already supplied efficiently by Bun |
| Hostile raw-protocol test clients | test-only `node:net`, `node:http` | **KEEP** | They intentionally exercise bytes/FIN/keep-alive below friendly fetch abstractions |

Do not use Bun's headline benchmarks as Fleet Deck evidence. Run the representative harnesses in
P0 on the same Bun revision and machine before and after each relevant adapter change.

## 5. Error and policy model

### Error taxonomy

Use `Data.TaggedError` for expected asynchronous failures. Start narrow; do not create one generic
`DaemonError` union that erases policy.

| Family | Representative tags | Interpreter |
|---|---|---|
| Startup | config/path, pid ownership, bind, database open/migration | Root entrypoint; cleanup and current exit code (`EADDRINUSE` remains 3) |
| Process | spawn, non-zero, timeout, output limit, stream, escalation | Owning workflow; legacy facade maps back to exact `ExecResult` |
| Store | query, transaction, closed store, migration | Application workflow or startup; preserve existing HTTP/hook policy |
| Request/domain | validation, auth, CSRF, conflict, not found, capacity | Native HTTP adapter maps to exact current status/body |
| Optional service | discovery, polling, repo probe | Named boundary logs/skips/retries according to current policy |
| Terminal | tmux unavailable, protocol, command timeout, pane dead, stream closed | Viewer/WS adapter closes exactly as today |
| Hook | named intake or infrastructure failures | Hook policy recovers to `200 {}`; pre-dispatch refusal does not mutate, while post-dispatch failures preserve current mutation/rollback behavior |

Interruption is not an error tag. It means the owner cancelled or the daemon is shutting down.
Defects remain defects, are redacted and reported at the owning boundary, and must not be silently
folded into expected failures.

### Fail-open rule

The hook adapter is the final policy interpreter:

- decode/validation failures, authentication failures, named operational errors, defects, and
  interruption before settlement all produce the canonical no-op response;
- authentication/validation refusal before dispatch must not mutate or call `applyEvent`;
- once an authorized hook has dispatched, preserve the current handler's mutation and transaction
  behavior: an outer failure may still answer `{}` after an already-committed diagnostic/event.
  Do not invent an all-hook rollback guarantee as part of this migration;
- no Effect `Cause`, stack, secret, token, path, or warning is sent to Claude;
- duplicate settlement is harmless and only the first completion wins;
- shutdown releases held hook responses while HTTP is still writable.

Keep redundant cleanup where it makes this rule more auditable. Effect abstraction is not a reason
to reduce defensive behavior.

### Logging

Introduce Effect logging behind the current logging contract, not as a format change. Decide once
whether `BunRuntime.runMain` reports root failures; use `disableErrorReporting` if the Fleet Deck
interpreter already logged the Cause. Add redaction tests for tokens, hook bodies, environment
secrets, repo paths where currently protected, and SQL/process arguments. No OTLP dependency or
network exporter is added.

## 6. Shutdown state machine

Do not rely on Layer LIFO alone. Fleet Deck has policy ordering across resources, so model an
idempotent state machine:

RC.110's `BunRuntime.runMain` delegates to the Node-shared runner: `SIGINT`/`SIGTERM` directly
interrupt the root fiber; they do **not** complete an application `Deferred`. Shutdown is therefore
interruption-driven:

- after readiness, `DaemonApp` remains alive with `Effect.never` (or races an internal, non-signal
  shutdown request when one exists);
- root interruption or any other root exit enters `LifecycleCoordinator.close(exit)` through an
  `Effect.onExit` handler placed **inside** the provided `LiveLayer`, so policy close runs while the
  acquired services are still live and before fallback Layer finalizers;
- that coordinator driver is uninterruptible and idempotent, but each blocking phase has its own
  bounded wait and observes a callback-safe force latch;
- a separately acquired host signal observer is installed at the start of `DaemonApp`. The stock
  runner still owns first-signal interruption. If the coordinator is already closing—or the
  observer sees a second signal—it synchronously trips the force latch; it never starts an Effect,
  creates a runtime, or runs finalizers itself;
- internal shutdown requests may complete a `Deferred`; OS signals never depend on that path.

```text
running
  -> quiescing          mark app closed to new work; initiate non-blocking HTTP stop(false)
  -> stopping-producers interrupt/join boot, agents, LAN, retention, and sweep fibers
  -> withdrawing       after LAN joins, send/await mDNS goodbye and close discovery
  -> releasing-holds   settle held hooks as 200 {} while their HTTP sockets are writable
  -> closing-clients   close viewers/WS; interrupt/join terminal and in-flight route fibers
  -> closing-http      race graceful stop with deadline; stop(true) and await if it loses
  -> closing-store     close statements/SQLite only after every store user has joined
  -> releasing-process remove owned pidfile/listeners and finish
```

Requirements:

- first signal begins graceful shutdown; a concurrent second signal transitions the same
  coordinator to forced cleanup (interrupt/kill/force-stop within the remaining deadline) without
  running finalizers twice;
- stop producer fibers immediately after quiescing so LAN cannot reannounce after goodbye and a
  poller cannot broadcast or query after its downstream resource closes;
- preserve the existing one-second **mDNS-stop** watchdog until an explicit root-deadline decision;
  it does not currently guarantee whole-daemon closure;
- P0 must identify the supervisor/takeover deadline. P4 then defines one absolute root deadline
  that respects it and passes the remaining time into HTTP, process, terminal, and discovery
  finalizers;
- ordinary subprocess timeout/abort retains its one-second TERM grace. Daemon shutdown may shorten
  that grace or skip to KILL when the absolute remaining budget requires it; document and test the
  shutdown-specific policy rather than promising two incompatible deadlines;
- normal local shutdown p95 is below 500 ms; forced shutdown completes before the recorded root
  deadline and is measured separately;
- partial startup failure closes only resources already acquired, in policy-safe order;
- the root `onExit` path explicitly runs `LifecycleCoordinator.close` before returning from the
  provided Scope. Individual Layer/acquireRelease finalizers are idempotent safety fallbacks and
  therefore cannot race or reorder the policy sequence when the Scope subsequently closes;
- held-hook release precedes transport close; store close follows all store-using fibers;
- HTTP initiates `server.stop(false)` once, races that Promise against the absolute deadline, then
  calls and awaits `server.stop(true)` if graceful stop loses; never await the two serially while
  quiet keep-alives or WebSockets can hold the first Promise open;
- pidfile removal verifies ownership and is last;
- the root runtime's keep-alive interval and signal listeners are gone after completion;
- expected SIGTERM/takeover and startup errors retain current process exit codes. Use v4's custom
  `Runtime.Teardown` / `Runtime.errorExitCode` where required and test it rather than accepting
  the default interruption code accidentally.

## 7. Work-package graph and operating rule

```text
P0 evidence
  -> P1 explicit plain resource handles
  -> P2 exact Effect kernel
  -> P3 Bun subprocess pilot
  -> P4 root runtime and shutdown
       ├-> P5 boot + schedules
       ├-> P6 HTTP + WebSocket workflows -> P7 terminal ownership/transport
       └-> P8 store/SQLite workflows
             \          |          /
              -> P9 application workflows
                 -> P10 holds/fail-open
                 -> P11 remaining Bun adapters
                 -> P12 build/distribution
                 -> P13 cleanup/observability/docs
                 -> P14 RC upgrade rehearsal + release gate
```

Every numbered slice below is a reviewable commit unless it says otherwise. For each slice:

1. add or identify the characterization test first;
2. make the smallest behavior-preserving change;
3. run focused tests;
4. run the global gate in §9;
5. regenerate and commit affected artifacts;
6. update the ledger and evidence before starting the next slice;
7. revert the slice if its hard invariant fails—never edit the invariant away.

**Branch and CI model:** execute the slices as local commits on this one dedicated implementation
branch. The repository's remote `hook-integrity` job rejects any behavior-bearing daemon payload
diff without a coordinated version bump, so intermediate commits can pass the local/type/test/build
matrix but are not promised to pass that remote job against `main`. If per-slice all-green remote
CI is desired instead, pause before P3 and ask the user to select an early
development/prerelease version; do not invent it.

Two external-authorization checkpoints are mandatory:

1. **Platform checkpoint before P7:** after P6 is locally green, prepare the branch and ask for
   permission to push and open/update a draft PR so the blocking macOS/real-tmux and Linux lifecycle
   jobs can run. At this checkpoint `hook-integrity` may be intentionally red because version
   closure has not happened; record that expected failure, but P7 cannot close until its named
   platform jobs are actually green. If authorization is withheld, pause—the local goal is not
   complete.
2. **Final checkpoint in P14:** ask the user to choose the exact release/development version, make
   one coordinated local version closure, then ask for permission to push/update the PR and run the
   complete remote matrix. P14 and the goal cannot close until all required remote jobs, including
   version and hook-integrity, are green. Tagging, publishing, and releasing remain outside this
   goal and require later explicit authorization.

This plan authorizes local code, test, documentation, generated-artifact, lockfile, and commit work
on the branch. It does **not** authorize pushing, opening/updating a PR, tagging, publishing,
releasing, or sending an upstream Effect patch. Prepare those outputs, then request explicit user
approval for the external action.

### P0 — freeze behavior, cost, and compatibility

**Purpose:** create trustworthy before/after evidence before runtime code moves.

- [ ] P0.1 Record commit, dirty state, Bun version and revision, OS/arch, CPU, and test count in
  `docs/v1/evidence/effect/baseline.md` plus machine-readable JSON.
- [ ] P0.2 Check in a repeatable cost harness for daemon bundle raw/gzip bytes, packed-package
  bytes/content, build duration, daemon cold start to `/health`/reconciliation-ready, hook and CLI
  cold start against their hard deadlines, idle RSS/CPU, graceful/forced shutdown duration, and
  remaining child/FD/socket state. Pin gzip to one checked-in `Bun.gzipSync` level-9 implementation
  on the recorded Bun revision; shell `gzip` defaults and metadata are not comparable evidence.
- [ ] P0.3 Add an exec microbench covering short commands, bounded output, timeout,
  cancellation, and descendant cleanup; report p50/p95/p99 and CPU/RSS after warmup.
- [ ] P0.4 Add HTTP/WS/SQLite representative workloads described in §8; do not use synthetic
  throughput as a correctness replacement.
- [ ] P0.5 In a disposable `mktemp -d` project outside the repository, install exact
  `effect@4.0.0-rc.110` under Bun 1.3.14 and capture the commands/output for source execution, a
  Bun-generated probe bundle, `Scope` finalization, interruption, and natural process exit. P0
  must not change the production package or lockfile.
- [ ] P0.6 In the same disposable probe, prove Effect v4's root fiber keep-alive behavior in a
  subprocess fixture so later removal of `unref()` is deliberate. Recreate the fixture in the repo
  after dependencies land in P2.
- [ ] P0.7 Create the migration ledger with columns for work package, compatibility bridge,
  unstable imports, detached work allowlist, focused tests, benchmark delta, and rollback commit.
- [ ] P0.8 Keep Bun 1.3.14 as the blocking floor/performance baseline and add a latest-stable Bun
  compatibility canary. If the published promise remains `>=1.3.14`, current stable must not be an
  untested runtime.

**Exit gate:** all existing global gates are green; baseline evidence is reproducible twice on the
same machine; the spike exits naturally and under SIGINT/SIGTERM without leaking its keep-alive
mechanism.

**Rollback:** evidence/scripts only; no production dependency or code path.

### P1 — make current resources explicitly releasable before Effect

**Purpose:** give Effect real acquire/release seams instead of hiding existing leaks inside Layers.

- [ ] P1.1 Expand `exec-timeout` coverage for missing executable, sync spawn failure, non-zero with
  stderr, exact combined 1 MiB boundary, pre-aborted signal, live env mutation, inherited/open
  pipes, immediate exit/`onExit`-before-handle publication, exit/abort/timeout exactly-once races,
  and fragmented/invalid multibyte UTF-8 exactly at the shared byte cap.
- [ ] P1.2 Give `createTermBridge` an idempotent async `close()` that rejects/completes waiters,
  stops timers, closes viewers/input, and terminates the control client.
- [ ] P1.3 Give the questions orphan sweep and core retention cadence idempotent stop handles;
  keep scheduling behavior unchanged. Add an idempotent HoldManager lifecycle with
  `quiesce()`, `releaseAll()`, and `close()` so held responses settle before expiry/rearm/sweep
  timers are cancelled.
- [ ] P1.4 Make the LAN watcher return an awaitable stop handle. Retain `startAgentsPoll`'s handle
  and await the cleanup behavior it can provide today; move cancellation of in-flight poll
  subprocesses to P3/P5 after the process seam exists.
- [ ] P1.5 Return an explicit HTTP lifecycle object that owns Bun server, WS clients, keepalive,
  broadcast flush, terminal registry, and held-response release. Make close idempotent and
  await `server.stop(...)`.
- [ ] P1.6 Compose a plain `DaemonResources.close()` integration path without switching signal
  wiring yet. Test every partial-acquisition prefix and double-close.

**Focused proof:** existing HTTP stall/raw timeout/WS/terminal suites; new lifecycle tests asserting
no callback fires after close, no DB use occurs after DB close, and no HTTP listener remains.

**Exit gate:** the current non-Effect daemon can quiesce/release holds and close every currently
stoppable resource through one plain function. In-flight process interruption is explicitly P3/P5,
not a circular P1 prerequisite.

**Rollback:** each new handle is additive until the root begins using it.

### P2 — pin Effect and establish the application kernel

**Purpose:** add the exact runtime cohort, v4-native service conventions, and test infrastructure
without changing daemon startup.

- [ ] P2.1 Add exact `effect` and `@effect/platform-bun` dependencies and lockfile alignment check.
- [ ] P2.2 Add source/generated import tripwires for board, contracts, and hook bundles.
- [ ] P2.3 Create `app/errors.ts`, initial service contracts, Live Layer composition skeleton, and
  fake Layers for tests using `Context.Service`, `Layer.succeed`, and `Layer.effect`.
- [ ] P2.4 Add `bun:test` helpers that run Effects and expose `TestClock`/`TestConsole` without
  hiding typed error channels or defects.
- [ ] P2.5 Add the unstable-import register and a source check rejecting unregistered unstable
  imports and v3 package names.
- [ ] P2.6 Measure a representative kernel/process probe artifact, installed dependency closure,
  and clean-install cost after adding packages. Real daemon bundle/start/RSS deltas begin when P3
  first imports the kernel into production and repeat after P4 switches main.
- [ ] P2.7 Audit the full resolved closure, not only the two direct dependencies: package count,
  lock diff, clean-install time/disk, optional native `msgpackr-extract` binaries, licenses,
  provenance/advisories, and `ws`/`@types/ws` dedupe or version movement caused by
  platform-node-shared. The budget is two direct packages initially; P8 SQL and one P11 extracted
  native-platform package may raise the ceiling to four only after their independent gates, with
  the entire transitive closure recorded. Initial closure must stay at or below 24 resolved
  production packages and 75 MiB installed on the recorded Bun/filesystem; the optional SQL plus
  one extracted adapter ceiling is 30 packages and 100 MiB. Hard-reject a mutable spec, RC cohort
  mismatch, incompatible license, unreviewed production install/native-binary execution,
  exploitable high/critical advisory, excluded-bundle leak, or direct-package ceiling breach.
  Crossing a count/disk ceiling or accepting another provenance risk requires explicit user or
  maintainer acceptance; the executing agent cannot approve it.
- [ ] P2.8 Align `@types/bun` with the blocking Bun 1.3.14 floor and exercise latest-stable types in
  the canary. Add the generic Bun-platform conformance harness needed to extract/upstream an owned
  adapter without mixing Fleet Deck domain policy into it.

**Exit gate:** exact version checks, typecheck, source/bundle smoke, natural-exit fixture, and zero
Effect bytes in excluded bundles. Cost is recorded; no behavior path has changed.

**Rollback:** remove packages, lock changes, kernel skeleton, and checks as one isolated group.

### P3 — Bun-native subprocess service and first Effect workflow

**Purpose:** prove the highest-value cancellation/resource boundary with direct `Bun.spawn`.

- [ ] P3.1 Extract an internal process-driver contract beneath the existing `execFileP` API. Make
  the current Node driver one injectable implementation so parity is differential, not inferred.
- [ ] P3.2 Implement a Bun driver with argv arrays, explicit `cwd`, explicit live `process.env`,
  `stdin` handling, concurrent stdout/stderr Web-stream draining, and exact spawn-error mapping.
  Accumulate/cap bytes before decoding and preserve current
  `Buffer.concat(chunks).toString("utf8")` truncation/replacement behavior; generic stream-to-text
  helpers are not parity evidence.
- [ ] P3.3 Preserve the **combined** 1 MiB cap; do not rely on Bun's per-process `maxBuffer` if it
  changes combined stdout/stderr behavior.
- [ ] P3.4 Preserve wall-clock settlement and TERM-to-one-second-KILL escalation. For `killTree`,
  create/target the POSIX process group exactly as today; prove descendants die.
- [ ] P3.5 Model ownership with `Effect.acquireRelease`/`Effect.callback`; interruption removes
  listeners, closes streams, escalates if needed, and waits only within the shutdown budget.
- [ ] P3.6 Export `execEffect` and keep the exact Promise-shaped `execFileP` as the sole temporary
  runtime facade. Before the daemon has a root Effect program, back it with one lazily constructed
  `BootstrapRuntimeBridge` using `ManagedRuntime.make(ProcessRunnerLive)` only; register and test
  idempotent disposal in P1's aggregate daemon close. Every expected failure still resolves the
  existing `ExecResult` shape. P4 must dispose/delete this bridge before binding the facade to the
  root `IngressSupervisor`; both implementations may never be live together.
- [ ] P3.7 Differential-test old and Bun drivers, benchmark them, then remove the old driver only
  when all result bytes, flags, timing classes, process-tree facts, and leak checks agree.
- [ ] P3.8 Move `files.ts` bounded git execution onto the service while preserving its distinct
  partial-output/truncated/timedOut contract and concurrency cap.
- [ ] P3.9 Once the local Bun driver passes, implement the generic subset as a conforming
  `ChildProcessSpawner`, run upstream Effect's relevant adapter tests plus Fleet Deck differential
  tests, and prepare an upstream-ready patch. The app-local adapter plus that patch is P3's
  deterministic mandatory outcome. Submission, package extraction, or full-fork consumption is
  deferred to P11.10 and requires the specific user approval described in §3; keep
  output-cap/result policy in the app either way.

Do not automatically use `@effect/platform-bun/BunChildProcessSpawner`: in RC.110 it re-exports
the Node-shared implementation. Effect's unstable process API may be a comparison implementation,
but the unbounded `ChildProcessSpawner.string` / `ChildProcessSpawner.lines` helpers and default
process-group semantics may not replace Fleet Deck's contract.

**Exit gate:** complete exec/files parity, no listener/stream/child leaks, focused benchmarks within
budget, and less hand-written ownership in the production path. A `tryPromise` wrapper alone does
not pass.

**Rollback:** switch the injected driver or facade back without changing callers.

### P4 — switch the daemon to one root Effect runtime

**Purpose:** make lifecycle ownership real while retaining the P1 release implementations.

- [ ] P4.1 Extract top-level boot from `fleetd.ts` into `DaemonApp`, preserving preflight order,
  synchronous logs, readiness, exit codes, and takeover behavior.
- [ ] P4.2 Create and unit-test the root `LifecycleCoordinator`, its callback-safe force latch, and
  the ordered §6 state machine. Initially it delegates to P1's aggregate close operations; it does
  not require the root runtime to have switched yet.
- [ ] P4.3 Definition/test-only: build the aggregate
  `Effect.acquireRelease(acquireDaemonResources, LifecycleCoordinator.close)` Layer, root
  composition function, `IngressSupervisor`, and entrypoint fixtures while the production
  entrypoint still uses P1's plain owner and P3's bootstrap bridge. This commit must not start a
  second production runtime or switch main.
- [ ] P4.4 Perform one explicitly **atomic, non-splittable root-cutover commit**: switch the
  entrypoint to `BunRuntime.runMain`; acquire the aggregate Layer; install the root signal observer;
  keep `DaemonApp` alive with `Effect.never` (racing only a real internal shutdown request); place
  the uninterruptible coordinator `onExit` inside `Effect.provide(..., LiveLayer)`; construct and
  bind `IngressSupervisor` before any legacy facade call; and delete/disable
  `BootstrapRuntimeBridge` in the same diff. A fresh cutover process must have no path that lazily
  creates the bootstrap runtime, so the two runtimes are never live together. Do not split this
  list across commits even though other numbered slices are independently reviewable.
- [ ] P4.5 Split ownership incrementally in resource-sized commits: pid claim, SQLite/core, HTTP,
  discovery/LAN, background fibers, and terminal registry. The token file is persistent state and
  is never deleted at shutdown; only the verified owned pidfile is released.
- [ ] P4.6 Complete the §6 quiesce/shutdown integration. Verify from the pinned source that the
  stock runner interrupts the root, ensure its default error reporting cannot duplicate Fleet Deck
  logs, characterize the current external
  takeover/supervisor limit and choose an absolute root deadline; retain the one-second mDNS
  sub-watchdog until the new whole-daemon deadline is proven.
- [ ] P4.7 Model startup errors with exact current exit codes, including bind conflict. Test
  success, typed failure, defect, interruption, acquisition interruption, and failure after every
  acquisition step.
- [ ] P4.8 Test first/second signal, signal during every shutdown phase, takeover SIGTERM, held
  hook at shutdown, active HTTP request,
  active WS viewer, running/TERM-resistant subprocess (including reaping), busy poller, and mDNS
  goodbye.
- [ ] P4.9 Assert zero remaining daemon children, listeners, sockets, scheduled callbacks, and
  root runtime keep-alive after scope closure.

**Exit gate:** `fleetd.ts` is a thin main boundary; root ownership is observable in tests; all
partial boots and shutdown paths finish under the recorded absolute deadline without
use-after-close or post-goodbye/post-close producer activity.

**Rollback:** point the entrypoint back to the proven plain `DaemonResources.close()` path. No DB
schema or wire change makes rollback unsafe.

### P5 — structured boot, polling, and schedules

**Purpose:** replace detached work and manual timer flags with supervised fibers and explicit
policies.

- [x] P5.1 Replace boot reconciliation's manually settled Promise/chain with scoped fork/join and
  a `Deferred`/`Ref` readiness model. Readiness still waits for spawn reconciliation, boot
  retention, and the coalesced broadcast flush, including degraded completion.
- [x] P5.2 Convert agents polling to a scoped fiber. Preserve no overlap, active/idle cadence,
  named fail-open skips, and cancellation of an in-flight process.
- [x] P5.3 Convert LAN refresh to a scoped fiber with current initial-run/cadence/failure behavior.
- [x] P5.4 Move retention scheduling out of `createCore`; keep the retention function itself plain.
- [x] P5.5 Move the question orphan sweep only after P10's fixtures, or keep it on the explicit P1
  handle until then.
- [x] P5.6 Use `Schedule` for policy and `TestClock` for deterministic units, plus real Bun-clock
  integration tests for process exit and finalization.
- [x] P5.7 Remove each corresponding `unref()` only after the root-exit fixture proves that the
  scoped replacement terminates naturally.

Encode retry semantics explicitly: `Effect.repeat` stops on failure, so an optional poll that must
continue after a named failure must catch that named error inside the repeated action. Do not
blanket-catch defects.

**Exit gate:** startup/readiness is deterministic; pollers are single-flight; closing the root
interrupts and joins every scheduled fiber/poller; no post-close callback or keep-alive remains.

**Rollback:** whole-slice only. Reverting the P5 slice commits `972621d5` through `ca62b94f`
(listed in [p5.md](./evidence/effect/p5.md)) returns to the P4 root-cutover anchor `661dfe31`.
`createCore` no longer owns the retention scheduler, so there is no selectable per-scheduler
rollback.

### P6 — Effect application HTTP with Bun-native transport

**Purpose:** make route workflows Effects without losing the hardened `Bun.serve` behavior.

- [x] P6.1 Characterize exact route table, status/body/headers, body limit/drain, stalled FIN,
  request timeout, disconnect, auth, CSRF, loopback/LAN, static asset, WS upgrade, payload,
  heartbeat, terminal, and backpressure behavior.
- [x] P6.2 Split pure parsing/security/response policy from the transport callback without changing
  bytes. Keep Request/Response/fetch Web standards where they are sufficient.
- [x] P6.3 Put the existing `Bun.serve` lifecycle behind the `HttpServer` service and root Scope.
  If callbacks require an imperative bridge, capture the root Context once and use
  `Effect.runForkWith`, `Effect.runCallbackWith`, or `Effect.runPromiseWith` through
  `IngressSupervisor`. It tracks every resulting fiber and Promise-backed request until settlement
  or interruption; no callback constructs another runtime or provides `LiveLayer`.
- [ ] P6.4 Convert route application handlers in repeatable route-group sub-slices, one service and
  focused fixture set per commit. Map typed errors to the exact existing Response only in
  `http-policy.ts`; map hook failures in `hook-policy.ts`. Complete terminal ingress before P7.
- [ ] P6.5 Preserve the audited WS backpressure contract as implemented: per-socket
  `getBufferedAmount()` thresholds with eviction (snapshot peers `terminate()` past
  `MAX_WS_BUFFER`; terminal viewers `close(1009)` past `MAX_TERM_WS_BUFFER`),
  `send()`/`ping()` return values deliberately ignored (probed: `-1` means queued-not-rejected,
  `0` is ambiguous between empty-payload success, closed socket, and past-cliff drop;
  `ping()` returns `0` on a live socket), no drain-based resume, and no reliance on
  `server.publish` return values for per-subscriber backpressure (probed: publish returns
  payload bytes even while a subscriber sits at Bun's 16 MiB silent-drop cliff). Evidence:
  [p6-http-matrix.md](./evidence/effect/p6-http-matrix.md) §3 and
  [p6-ws-send-probe.md](./evidence/effect/p6-ws-send-probe.md).
- [ ] P6.6 Implement graceful `Bun.serve` stop: initiate `server.stop(false)` once during quiesce,
  release holds, close clients, and race the graceful Promise with the absolute remaining deadline.
  If it loses, call and await `server.stop(true)`; do not await graceful stop serially before the
  force decision.
- [x] P6.7 Independently trial `effect/unstable/http/HttpRouter` and
  `@effect/platform-bun/BunHttpServer`. Switch the transport only if every black-box fixture and
  shutdown budget passes. Override its default shutdown timing to Fleet Deck's budget.
  **Verdict: KEEP CUSTOM ADAPTER** (the continuation rule records this as success). Evidence:
  [p6-transport-trial.md](./evidence/effect/p6-transport-trial.md).
- [ ] P6.8 Benchmark `/health`, `/state`, hook POSTs, large paste, withheld bodies, WS broadcast,
  and static assets at representative concurrency.

Using Effect for all route workflows while retaining a custom scoped `Bun.serve` adapter is a
successful full migration. The platform-bun adapter is optional because Fleet Deck's audited wire
semantics are the requirement. P6.7 decided **KEEP CUSTOM ADAPTER** at rc.110; see
[p6-transport-trial.md](./evidence/effect/p6-transport-trial.md). The P6.8 harness exists at
`d5404aac` (`scripts/effect-migration/p6-http-bench.ts`) but the quiet-host baseline has not been
captured, so P6.8 stays open.

**Exit gate:** no scattered runtime runners, exact HTTP/WS parity, cancellation on disconnect where
safe, all transport resources root-owned, and performance within budget.

**Rollback:** route groups retain translation facades until the group and source/bundle lanes pass.

### P7 — terminal bridge under Effect ownership; Bun transport trial

**Purpose:** scope the highest-risk ownership graph after process, root, and HTTP foundations are
proven, then adopt Bun transport only if its dedicated stream gate passes.

- [ ] P7.0 Reach external-authorization checkpoint 1 in §7. After approval, push/open or update the
  draft PR, make the macOS lifecycle/real-tmux lane blocking, and record `uname -m` in its evidence
  rather than assuming the runner architecture. Linux remains blocking too. An intentionally red
  pre-version `hook-integrity` job does not waive either platform job; without authorization or
  green platform results, pause and leave P7 open.
- [ ] P7.1 Expand parser/protocol fixtures before implementation: fragmented UTF-8 and `%` frames,
  multiple frames in one chunk, FIFO replies, command/attach timeout, write error, child exit,
  pane death, open/close races, and shutdown with active viewers.
- [ ] P7.2 Add byte-boundary fixtures for pre-init output, per-pane output batching, serialized
  input, 1 KiB input chunks, resize ordering/jiggle, and slow WS clients.
- [ ] P7.3 Trial tmux `-C` through direct `Bun.spawn` using Web stdout/stderr and writable stdin.
  Keep the proven parser and `StringDecoder` initially; change decoding only in a separate parity
  commit. If Bun stream/FileSink semantics or performance fail, record **KEEP scoped Node-stream
  transport** and continue the Effect ownership migration.
- [ ] P7.4 Own the shared control process in a Scope; consume output with a core `Stream`; model
  command replies with FIFO `Deferred`s and bounded queues with an explicit overflow policy. Every
  Queue has an explicit `Queue.shutdown` finalizer; root PubSubs also have explicit shutdown
  finalizers because creating a Queue/PubSub does not make it scoped automatically.
- [ ] P7.5 A command timeout tears down the compromised shared client as today. Viewer child scopes
  close only that viewer; ref-count or root closure owns the shared client lifetime.
- [ ] P7.6 Keep `openViewer()` Promise/handle compatibility until the terminal WS route is fully
  Effect-native, then remove the facade and its runtime bridge.
- [ ] P7.7 Soak real tmux with repeated attach/input/resize/detach, fragmented high-volume output,
  daemon SIGTERM, child crash, and WS backpressure on blocking macOS and Linux jobs.

Do not use unbounded `Stream.runCollect`, `ChildProcessSpawner.string`, or an unbounded Queue.
Buffer limits and drop/backpressure policy are external behavior.

**Exit gate:** Effect owns the terminal resource graph; all protocol bytes and ordering agree;
last-viewer/root teardown leaves no tmux client, waiters, timers, streams, or WS state; real soak
stays within RSS/CPU/latency budgets. Bun transport has an evidence-backed MIGRATE or KEEP result.

**Rollback:** keep the P1 closeable Node-stream implementation behind the same Terminal service
until the complete gate passes.

### P8 — Effect-owned store and Bun SQLite seam

**Purpose:** put persistence lifetime and workflow failures in the application model while keeping
synchronous Bun SQLite honest.

- [ ] P8.1 Remove the stale dynamic Node SQLite fallback and import `bun:sqlite` statically in the
  Bun adapter. Preserve null-to-undefined normalization and public row types.
- [ ] P8.2 Trial `strict: true` with fixtures for every binding style, missing/extra parameters,
  integer types, and statement lifetime. Do not enable `safeIntegers` incidentally.
- [ ] P8.3 Build the Store Layer explicitly from
  `Layer.effect(Store, Effect.acquireRelease(openDatabase, closeDatabase))`; returning an object
  with `close()` from `Layer.effect` does not infer cleanup. Keep the centralized query/transaction
  surface synchronous and plain; the Layer owns lifetime, not every SQL call's return type.
- [ ] P8.4 Preserve `user_version` migrations, rollback behavior, `BEGIN IMMEDIATE` boundaries,
  WAL/busy behavior, chmod/sidecars, restart durability, and close ordering. Do not bump the DB
  schema version for an adapter-only refactor. Explicitly finalize owned/cached statements, then
  require `db.close(true)` to complete so `sqlite3_close_v2` cannot leave statement-owned closure
  deferred past the root finalizer.
- [ ] P8.5 Inventory repeated/static statements for `db.query()` caching; benchmark against current
  `prepare()` and change only proven callsites.
- [ ] P8.6 Convert application workflows to yield Store and translate failures once around a
  coarse synchronous DB operation with `Effect.try`; do not wrap each statement. Leave row
  mapping, SQL constants, and pure derivation plain. Treat a synchronous query/transaction as
  non-interruptible; never pretend Effect can cancel work while Bun is blocking the event loop.
  Land one workflow/module per sub-slice, and forbid suspension/yielding inside a direct SQLite
  transaction callback so unrelated fibers cannot interleave on the same connection.
- [ ] P8.7 Independently benchmark `@effect/sql-sqlite-bun@4.0.0-rc.110`. Its serialized semaphore,
  WAL default, five-second blocking busy timeout, writable `BEGIN IMMEDIATE`, and lack of streaming
  queries must match Fleet Deck intentionally. Adopt only if semantics and measured value justify
  the third dependency; otherwise record **KEEP direct bun:sqlite**.

**Exit gate:** migration/restart/durability and query benchmarks pass; DB is acquired once, closed
after all users, and cannot be accessed afterward; SQL candidate decision is recorded.

**Rollback:** unchanged DB schema/files allow code rollback to the prior seam.

### P9 — migrate application workflows one bounded seam at a time

**Purpose:** finish the daemon application architecture after platform services are stable.

Recommended order:

- [ ] P9.1 spawn/revive/dismiss orchestration and supervised launch decisions;
- [ ] P9.2 repo/worktree/git asynchronous workflows;
- [ ] P9.3 bounded files/search/cache workflows;
- [ ] P9.4 mail and provider-specific asynchronous orchestration;
- [ ] P9.5 remaining HTTP-triggered application services;
- [ ] P9.6 takeover/election operations that benefit from typed lifecycle policy.

For each module:

1. identify the manual Promise/cancellation/retry/resource/error mechanism being removed;
2. add a service Effect without changing its public policy;
3. keep a temporary facade only while unmigrated callers exist;
4. convert callers and tests;
5. delete the facade and ledger entry immediately when the final caller moves.

Keep intentional sync probes (`repo-identity`, small capability checks) plain at first. Trial
`Bun.spawnSync` later as its own parity and latency commit. Keep detached supervisor/CLI launches
out of the shared scoped process runner until survival and signal semantics have dedicated tests.

**Exit gate:** all daemon asynchronous application workflows are Effects; remaining Promises are
native callback/Response values inside named adapters; every compatibility bridge is inventoried.

**Rollback:** module-sized service/facade commits; no cross-module flag day.

### P10 — questions, holds, and fail-open cleanup last

**Purpose:** migrate the most policy-sensitive timers only after root/HTTP/store behavior is stable.

- [ ] P10.1 Add fixtures for disconnect, timeout, no-board, persistence failure, duplicate
  completion, rearm race, daemon shutdown, HTTP close, and defects before/after mutation.
- [ ] P10.2 Model each hold/rearm lifetime as a child Scope with a `Deferred` result and explicit
  first-settlement semantics.
- [ ] P10.3 Move orphan sweeping to a scoped fiber driven by `Effect.repeat`/`Schedule` and the
  `Clock` service only if doing so preserves deliberate redundant cleanup and fail-open
  auditability. Effect's `Scheduler` service controls fiber dispatch/yielding; it is not the
  periodic-timer abstraction.
- [ ] P10.4 Ensure root quiesce settles all outstanding holds to canonical `200 {}` before HTTP
  stops, even if Store or terminal teardown fails.
- [ ] P10.5 Preserve pre-dispatch no-mutation behavior and the exact current post-dispatch
  mutation/rollback behavior; ensure Causes/secrets never reach the hook client.

**Exit gate:** all existing and new hook/needs-you/board-hold suites pass from source and bundle;
every shutdown/failure race returns control to the native terminal; no hold timer or Deferred is
left live.

**Rollback:** keep the explicit P1 hold manager behind the same policy adapter until the entire
race matrix passes.

### P11 — finish the Bun-native capability trials

**Purpose:** use Bun to its fullest where it is actually complete and faster for Fleet Deck.

- [ ] P11.1 Isolate mDNS behind Discovery/Datagram interfaces. Trial `Bun.udpSocket` multicast
  membership, interface selection, TTL 255, loopback, boolean send/backpressure and `drain`.
- [ ] P11.2 Prove two responders can coexist on port 5353 on macOS and Linux. Bun 1.3.14's
  documented UDP options expose neither the current `reuseAddr` contract nor an equivalent
  documented reuse option; a coexistence failure means **KEEP node:dgram**.
- [ ] P11.3 Prove goodbye datagrams reach a receiving peer or packet capture before close; Bun's
  `send() === true` and later `drain` prove buffer acceptance/writability, not completion of one
  particular datagram. Bind failures remain fail-open, interface roaming reconfigures safely, and
  stop finishes within the shutdown budget.
- [ ] P11.4 Trial `Bun.file` for immutable board asset Responses only after exact traversal,
  missing-file, MIME, CSP, cache, HEAD, Range, and content-length fixtures. Keep current path/header
  gates outside the file API.
- [ ] P11.5 Inventory content-only writes that can safely use `Bun.write`. Keep `node:fs` for
  directories, metadata, permissions, symlink-safe fd operations, fsync, atomic link/rename,
  bounded random access, and transcript tails.
- [ ] P11.6 Characterize Bun's `.env` auto-loading against system service and runtime overrides;
  decide explicitly whether production launchers need `--no-env-file`. Always pass live
  `process.env` to children and scan bundles for secrets.
- [ ] P11.7 Keep constant-time compare and exact security hashes on proven crypto APIs. Trial Web
  Crypto UUID/random generation only with shape/entropy fixtures; never use `Bun.hash` for secrets.
- [ ] P11.8 Trial `Bun.spawnSync` only in bounded CLI/capability paths with exit/output/self-path
  parity and latency evidence. For the daemon's cached tmux probe, characterize an asynchronous
  `Bun.spawn`/Effect replacement and its readiness/cache semantics; do not add a new event-loop
  blocking sync call.
- [ ] P11.9 Characterize detached supervisor/CLI launchers separately: child survival, `unref`,
  stdio flush, process group, signal forwarding, and identity. Record MIGRATE or KEEP; never route
  deliberately detached children through an ordinary root-scoped process API accidentally.
- [ ] P11.10 Close the owned-platform register for each generic gap. A parity-proven app-local
  implementation plus an upstream-ready patch is a valid local completion state. Present the
  maintenance/dependency evidence and ask before upstream submission, package extraction, or full
  fork consumption; if approved, run exact-RC conformance, package, lock, and rollback gates before
  consuming it. If not approved, retain the app-local implementation and record that decision.

**Exit gate:** each row in §4 has measured **MIGRATE**, **KEEP**, or **DEFER** evidence and a named
owner. No broad "replace all node imports" task remains.

**Rollback:** one adapter/callsite per commit.

### P12 — Bun-native builds and optional executable

**Purpose:** align distribution with the Bun-only runtime after behavior has stabilized.

- [ ] P12.1 Add a programmatic Bun build script for daemon/bin/hook artifacts using
  `Bun.build({ target: "bun", format: "esm" })`. Preserve generated banners, shebangs, top-level
  await, dynamic/JSON imports, builtins, version/self paths, and deterministic outputs.
- [ ] P12.2 Compare esbuild and Bun-built artifacts through the complete source/bundle, CLI,
  plugin, hook-integrity, static asset, and release-version suites before selecting Bun.build.
- [ ] P12.3 Verify Effect is correctly bundled into the daemon artifact and absent from hook/board
  artifacts. If any runtime dependency is externalized, prove the published install contains it.
- [ ] P12.4 Record build time, artifact raw/gzip, packed package contents/size, daemon/bin/hook
  cold start, hook deadline margin, and idle RSS. Remove esbuild only after all generated targets
  pass.
- [ ] P12.5 After Bun-built JS artifacts ship green, separately prototype `bun build --compile` for
  macOS arm64 and Linux x64. Exercise external writable DB, board assets, git/tmux discovery,
  signals, takeover identity, upgrades, and self-path assumptions.
- [ ] P12.6 Adopt the executable only if platform artifacts and cost/operations are acceptable;
  otherwise record it as a deferred distribution project. It is not required for the Effect
  architecture.

**Exit gate:** selected JS build is reproducible and all source/generated behavior agrees;
compiled-executable decision has cross-platform evidence.

**Rollback:** build-script selection only; never mix bundler and control-flow changes in one
commit.

### P13 — remove migration scaffolding and document the architecture

**Purpose:** ensure the resulting system is simpler and operable, not a permanent hybrid.

- [ ] P13.1 Drive **all temporary runtime bridges** and old process drivers to zero with no
  exception. Separately drive stopped-timer flags, manual abort listeners, and detached-work
  allowlist entries to zero or a reviewed, proven process-independent permanent exception.
- [ ] P13.2 Enforce one runtime boundary and Layer-only Live dependency selection with source tests.
- [ ] P13.3 Add structured Effect annotations/spans only behind current logging, with no exporter;
  expose useful local facts such as fiber/poller state, queue depth, WS backpressure, subprocess
  escalation, and shutdown phase where they aid diagnosis.
- [ ] P13.4 Add Cause/log redaction and duplicate-root-log tests.
- [ ] P13.5 Update `architecture.md`, route map, lifecycle state machine, validation gates,
  foundations outcome notes, and contributor guidance to current Bun/Effect reality.
- [ ] P13.6 Update the migration ledger with final module map, permanent plain zones, unstable
  imports, adapter decisions, benchmark deltas, and rollback/data-compatibility statement.

**Exit gate:** no stale compatibility island or contradictory plan-of-record assertion; a new
contributor can locate root composition, a service, a native adapter, its error policy, and tests
without reverse engineering `fleetd.ts`.

### P14 — rehearse an RC upgrade and prove release readiness

**Purpose:** prove that accepting a release candidate is operationally sustainable.

- [ ] P14.1 On a dedicated branch/commit, inspect the next selected v4 tag's migration notes and
  exact source for every registered unstable import.
- [ ] P14.2 Upgrade the full Effect cohort atomically, regenerate the lockfile, and run all gates.
- [ ] P14.3 Record compile/API changes, semantic changes, runtime/bundle deltas, and any adapter
  revalidation. Revert the rehearsal if it is not the release candidate being shipped; retain the
  evidence.
- [ ] P14.4 Re-verify that the blocking macOS lifecycle/real-tmux lane introduced before P7 and the
  blocking Linux lane both pass on recorded architectures.
- [ ] P14.5 Run the full security/fail-open review, soak, release delta audit, and generated
  artifact diff.
- [ ] P14.6 Reach external-authorization checkpoint 2 in §7: ask the user to select the exact
  version, then make one coordinated local version closure only after the migration is closed,
  rather than bumping payload versions through intermediate internal commits.
- [ ] P14.7 With explicit approval, push/update the PR and require the complete remote matrix,
  including version and plugin hook-integrity, to pass. Do not tag, publish, release, or submit an
  upstream patch without separate explicit approval.

**Exit gate:** one RC bump has been successfully rehearsed, the proposed cohort is exact and
documented, all local and required remote gates are green, any soft performance exceptions were
accepted by the user, the selected version is closed consistently, and rollback to the
pre-migration code remains data-compatible. Awaiting push/PR/CI authorization is a pause point, not
a passing exit gate; only tag/publish/release may remain outside the completed implementation goal.

## 8. Performance and soak plan

### Workloads

Run on the same pinned Bun revision with warmups and machine metadata:

| Area | Representative workload and recorded facts |
|---|---|
| Process | 1,000 short argv commands; output at/beyond cap; pre-abort; timeout; TERM-resistant child; open inherited pipe; child tree. Record p50/p95/p99, CPU, peak RSS, settlement accuracy, leftover descendants. |
| Daemon | 30+ cold launches to `/health` and reconciliation-ready; five-minute idle; 15 sessions; 10 terminal viewers; shutdown in idle/busy/held-hook states. Record latency, RSS, CPU, event-loop delay, FDs/processes. |
| HTTP | `/health`, `/state`, hook POST, large paste, withheld body, raw stalled FIN, static assets at 1/10/100 concurrency. Record throughput only alongside p95/p99 and correctness. |
| WebSocket | snapshot broadcast, slow reader, send backpressure, reconnect/heartbeat, terminal output fanout at representative client counts. Record queue/bytes/drop and latency. |
| SQLite | open+migrate, snapshot reads, representative 10k read/write mix, transaction, busy contention, restart, statement-cache growth. Record latency, blocked event-loop time, RSS. |
| Terminal | fragmented/high-volume tmux control output, command round trips, input/resize, attach/detach churn, last-viewer close, daemon shutdown. Record latency, byte ordering, backpressure, RSS/CPU. |
| Build | daemon/bin/hooks twice, package pack, optional executables. Record time, deterministic hash, raw/gzip/package/executable size, daemon/CLI/hook cold start, hard hook-deadline margin, and idle RSS. |

Local macOS and CI Linux numbers are separate datasets. Absolute performance enforcement belongs
on stable hardware; ordinary CI enforces correctness, relative artifact budgets, and leak facts.

### Provisional budgets

Replace baseline-relative placeholders with P0's checked-in numbers, but do not silently loosen
them:

- generated daemon bundle: no more than **750 KiB raw / 185 KiB gzip** without an accepted record;
  the current raw baseline is 566,619 bytes and P0 establishes the only enforceable deterministic
  gzip baseline;
- cold `/health` p95: no more than baseline +20% or +25 ms, whichever allowance is larger;
- idle RSS: no more than baseline +20% or +10 MiB, whichever allowance is larger;
- idle CPU: less than +0.2 percentage points;
- `/health`, `/state`, and process p95: no more than 10% regression;
- local graceful shutdown p95 below 500 ms; every forced case below P4's recorded absolute root
  deadline, with the current one-second mDNS sub-watchdog tracked separately;
- zero leaked children, timers, listeners, sockets, viewers, waiters, or root fibers after close;
- zero Effect bytes in board, contracts, and hook artifacts;
- no unrecorded DB, wire, exit-code, or fail-open drift, regardless of performance.

An exceeded soft budget requires a checked-in explanation, user-visible benefit or reliability
gain, and explicit acceptance from the user/maintainer. The executing agent must pause for that
decision rather than authoring its own acceptance record. A correctness, security, fail-open,
persistence, or leak failure is hard and cannot be traded for speed.

## 9. Verification matrix

### Per-slice global gate

Run focused tests first, then at minimum:

```sh
bun run typecheck
bun run typecheck:board-tests
bun run ci
bun run bundle
bun run bundle:bin
bun run bundle:hooks
bun run build:board
bun run test
bun run test:bundle
```

Build every generated target before the source/bundle run, then verify tracked **and untracked**
artifact diffs intentionally: daemon, bin, hooks, and `board-dist` must be either clean or staged as
the exact result of their source change. Rebuild a second time when checking determinism. From P2
onward, add a clean packed-install launch smoke so an accidentally externalized Effect dependency
cannot pass only in the repository checkout. The final version-bearing PR runs the remote version
and hook-integrity lanes described in §7; preserve them.

### Required focused suites by risk

| Risk | Existing anchors plus additions |
|---|---|
| Subprocess | `exec-timeout`, repo/worktree/spawn/files callers; add complete P1 race/result/process-tree matrix |
| HTTP/raw transport | `http-stall`, `raw-request-timeout`, `csrf-guard`, `loopback-gates`, static serving; add in-flight/graceful close and bind-order tests |
| WebSocket/terminal | `ws-hardening`, `terminal-ws`, terminal parser/bridge suites; add send-backpressure, active-viewer shutdown, real-tmux soak |
| Fail-open | `hook-auth`, `hook-stubs`, `needs-you`, `board-hold-presence`, `fleetd-audit-regressions`; add P10 failure/race matrix |
| Lifecycle/takeover | `takeover` and mDNS suites; add partial acquisition, double signal, shutdown order, active-resource leak subprocess fixtures |
| SQLite | migration/transaction/store suites; add strict-binding, close order, contention, restart durability, SQL candidate differential tests |
| Boundaries/build | import-boundary tests, source/bundle parity, board build, hook bundle scan, package-content and exact-version checks |

### Platform matrix

- Linux x64: required on every migration PR.
- macOS arm64: make required before P7/P14 completion; it must run real tmux, multicast, takeover,
  and lifecycle fixtures rather than only pure tests.
- Bun 1.3.14 remains the blocking compatibility/performance floor; latest stable runs as a
  compatibility canary and becomes blocking before retaining the published `>=1.3.14` promise at
  release.
- Other architectures: no new promise from this migration. Compiled artifacts require their own
  declared target matrix.

## 10. Review checklist

Use this on every migration PR:

- [ ] What manual lifetime/cancellation/error mechanism did this remove?
- [ ] Who owns the new resource and what closes it on success, typed failure, defect, timeout, and
  interruption?
- [ ] Is any acquisition unintentionally uninterruptible, or any finalizer allowed to fail?
- [ ] Did a fiber become detached? If yes, why is process independence correct and where is it
  registered?
- [ ] Is buffer/backpressure/drop policy explicit and bounded?
- [ ] Does a timeout interrupt the underlying Bun operation, or only stop awaiting its Promise?
- [ ] Does fail-open still mean `200 {}` and no leaked Cause, with no mutation before dispatch and
  unchanged current mutation/rollback semantics after authorized dispatch?
- [ ] Did a native adapter preserve exact Bun/Node semantics rather than only types?
- [ ] Could SQLite close before a user, HTTP close before a held hook, or pidfile disappear before
  resource shutdown?
- [ ] Are source and generated bundle both tested?
- [ ] Did Effect enter board/contracts/hooks transitively?
- [ ] Are exact RC versions and unstable imports still registered?
- [ ] Is the benchmark representative, compared on the same runtime/machine, and within budget?
- [ ] Can this commit be reverted without a data or wire migration?

## 11. Rollback policy

- Keep public facades during each caller migration, then delete them promptly; do not maintain two
  permanent implementations.
- Keep injectable adapter selection only through the corresponding parity period.
- Do not change database schema or wire schema as part of Effect/Bun mechanics, so code rollback
  stays safe.
- Regenerate committed artifacts at each behavior commit. A rollback includes the matching
  generated files and lockfile.
- If the root runtime fails after P4, revert entrypoint selection to the P1 plain resource owner;
  do not partially bypass finalizers.
- If an unstable API changes on an RC bump, adapt inside its registered service module or revert
  that optional adapter; never scatter compatibility conditionals across workflows.
- Preserve benchmark and rejection evidence even when a candidate adapter is reverted.

## 12. Codex goal prompt

Copy the block below as the goal objective. Do not mark the goal complete after a pilot; it covers
the whole plan.

```text
Execute docs/v1/effect-migration-plan.md end to end on the dedicated v1 Effect branch.

Adopt Effect v4 as Fleet Deck's daemon application architecture and Bun as its deliberate native
runtime. Pin the complete Effect cohort to exactly 4.0.0-rc.110 initially; unstable modules are
allowed only through the plan's registered, tested adapter boundaries. Use the Bun Native APIs
skill for every runtime capability decision and CodeGraph before locating or changing indexed
code. Fleet Deck is willing to own/upstream missing Bun-native Effect platform adapters: prove
them app-locally first, then extract a Fleet Deck-scoped lockstep package/fork only when the plan's
conformance gate justifies it.

Work package by work package and commit-sized slice by slice. First freeze behavior/performance
and make current resources explicitly closeable. Then add the Effect kernel, prove a direct
Bun.spawn subprocess service, establish one root Scope, IngressSupervisor and shutdown state
machine, migrate boot and schedules, HTTP/WS application workflows, Effect ownership of terminal
streaming, store lifetime/coarse workflow errors, remaining async workflows, and holds/fail-open
behavior. Finish the selective Bun API trials, Bun.build evaluation, cleanup, docs, RC-upgrade
rehearsal, and release-readiness evidence.

Preserve every hard invariant in the plan: canonical hook 200 {} fail-open behavior, exact
HTTP/WS contracts and exit codes, argv/no-shell subprocess behavior and bounded output, SQLite
durability, terminal protocol ordering, one runtime boundary, no Effect in board/contracts/hook
bundles, no detached or leaked resources, and full source/generated parity. Pure deterministic
TypeScript remains plain. Do not weaken or quarantine tests.

Run focused tests and the complete local global gate after every behavior slice; regenerate and
inspect all committed artifacts. Use the plan's one-branch/final-version-closure model for the
remote hook-integrity gate. Update the migration ledger, unstable-import register, benchmarks, and
work-package checkboxes as evidence lands. Optional adapter candidates such as tmux Bun transport,
Bun.udpSocket, BunHttpServer, @effect/sql-sqlite-bun, Bun.file, Bun.build, and --compile land only
if their named semantics/performance gates pass; otherwise record an evidence-backed KEEP/DEFER
decision behind the Effect service and continue. If a soft performance budget needs acceptance,
pause for the user; do not accept the exception yourself.

Do not stop at an intermediate green state while a safe in-scope work package remains. Mark the
goal complete only when every mandatory work package and final definition of done in the plan is
satisfied, all optional trials have a recorded decision, blocking macOS/Linux gates pass, and no
required migration or cleanup work remains.

Local branch edits and commits are in scope. Do not push, open or update a PR, tag, publish,
release, or submit an upstream patch without explicit user approval. Pause for the two §7 approval
checkpoints: the P7 platform draft-PR run and the P14 version/final remote-CI run. Withheld
authorization means the goal remains open; it is not a reason to mark it complete. Tagging,
publishing, releasing, and upstream submission are not required to complete the implementation.
```

## 13. Progress ledger

Update this table only when a work package's exit gate has actually passed:

| Package | State | Evidence | Rollback point |
|---|---|---|---|
| P0 baseline and compatibility | Not started | — | — |
| P1 explicit resource handles | Not started | — | — |
| P2 exact Effect kernel | Not started | — | — |
| P3 Bun subprocess service | Not started | — | — |
| P4 root runtime/shutdown | Not started | — | — |
| P5 boot and schedules | Complete | [p5.md](./evidence/effect/p5.md) | `ca62b94f`; whole-slice revert of `972621d5`–`ca62b94f` restores `661dfe31` |
| P6 HTTP/WS workflows | Not started | — | — |
| P7 terminal stream | Not started | — | — |
| P8 store/SQLite | Not started | — | — |
| P9 application workflows | Not started | — | — |
| P10 holds/fail-open | Not started | — | — |
| P11 Bun capability trials | Not started | — | — |
| P12 build/distribution | Not started | — | — |
| P13 cleanup/docs | Not started | — | — |
| P14 RC rehearsal/release readiness | Not started | — | — |
