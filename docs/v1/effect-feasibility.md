# Effect v4 on Bun — adoption decision

*Part of [Fleet Deck v1.0](./README.md). This decision refreshes and supersedes the Effect
assumptions in [Foundations-Hardening](./foundations-hardening.md). The executable work plan is
[Effect v4 + Bun migration](./effect-migration-plan.md).*

**Status:** accepted on 2026-08-19
**Baseline:** Fleet Deck v0.23.6, Bun 1.3.14, strict TypeScript 5.9.3
**Selected release line:** exact Effect `4.0.0-rc.110`

## Decision

Fleet Deck will adopt **Effect v4 as the daemon's application architecture** and keep **Bun as
the runtime and platform**. We explicitly accept shipping a pinned v4 release candidate and using
selected `effect/unstable/*` modules when they remove real Fleet Deck lifecycle or control-flow
machinery.

This is broader than a subprocess experiment. The target is one Effect-owned daemon program in
which resource lifetime, asynchronous workflows, cancellation, concurrency, schedules, and typed
operational errors are expressed as Effects. The migration remains staged and reversible because
Fleet Deck's fail-open behavior and Bun-specific transport semantics are more important than
finishing a framework conversion.

"Use Effect fully" has a precise meaning here:

- every daemon-lifetime resource is acquired and released by one root `Scope`;
- every asynchronous application workflow is an Effect before it reaches a native adapter;
- background work is a supervised fiber, not a detached Promise or unowned timer;
- expected failures are typed and interpreted once at the boundary that owns policy;
- Bun callbacks and synchronous APIs live behind small platform services;
- there is one main runtime, plus only temporary, inventoried compatibility bridges during the
  migration.

It does **not** mean wrapping arithmetic, reducers, SQL row mapping, or shared DTOs in
`Effect.sync`. Pure code stays plain because Effect composes with ordinary TypeScript functions.
That is the full architecture, not a partial adoption.

## Why Effect earns its place

Fleet Deck already contains the problems Effect is designed to coordinate:

- subprocess timeout, external abort, output bounds, TERM-to-KILL escalation, and descendant
  cleanup;
- boot work whose readiness depends on several degraded-but-completing tasks;
- agents, LAN, retention, question, HTTP, and terminal timers with different stop policies;
- a shared long-lived tmux control client with command waiters and viewer lifetimes;
- HTTP and hook boundaries where the correct recovery policy varies from fatal startup to the
  canonical fail-open `200 {}`;
- shutdown that must quiesce work, release held hooks, close network resources, join background
  work, close SQLite, and remove only the pidfile it owns.

Today these rules are implemented independently with Promises, flags, timers, abort listeners,
and best-effort cleanup. Effect provides one vocabulary for ownership (`Scope` and
`acquireRelease`), structured concurrency (fibers), coordination (`Deferred`, `Queue`, `PubSub`),
time (`Clock` and `Schedule`), and failure policy (typed errors distinct from defects and
interruption). The benefit is not shorter syntax. It is making "who owns this work, what cancels
it, and what must finish before shutdown" executable and testable.

## Why Effect and Bun are complementary

Effect does not replace the runtime. Bun remains responsible for fast process execution, HTTP,
WebSockets, SQLite, UDP where its semantics fit, bundling, and the test runner. Effect owns how
those capabilities are composed and released.

The migration follows the Bun Native APIs rule **per capability**, not by global substitution:

| Capability | Direction | Reason |
|---|---|---|
| One-shot subprocesses | Migrate to a Fleet Deck Effect service backed by `Bun.spawn` | Native argv execution and Web streams, while preserving Fleet Deck's combined output cap and process-tree escalation |
| HTTP and server WebSockets | Keep `Bun.serve` and native WebSockets; place them under Effect ownership | The current adapter has audited body-drain, timeout, auth, CSRF, upgrade, and backpressure behavior |
| SQLite | Use static `bun:sqlite`; let an Effect layer own open/close and workflow errors | Queries are synchronous and already centralized; `@effect/sql-sqlite-bun` changes transaction and busy-timeout semantics |
| Long-lived tmux process | Migrate only after one-shot process parity and dedicated stream fixtures | Its ordering, UTF-8 fragmentation, input bounds, and teardown behavior are higher risk than ordinary commands |
| mDNS | Isolate first; trial `Bun.udpSocket` behind a real multicast gate | Bun exposes multicast membership and backpressure, but Fleet Deck also needs port-5353 coexistence, interface control, and goodbye completion |
| Content files | Trial `Bun.file`/`Bun.write` only where response and atomicity semantics match | Security-sensitive directory, permission, fd, symlink, and atomic-write operations stay on `node:fs` |
| Bundles | Trial `Bun.build({ target: "bun" })` after runtime parity | The present esbuild output targets Node even though production is Bun-only |
| Single executable | Evaluate after Bun-built JS artifacts are proven | Compilation is a distribution change with separate asset, platform, size, and self-path risks |

One non-obvious finding matters: in `4.0.0-rc.110`,
`@effect/platform-bun/BunChildProcessSpawner` re-exports the Node-shared child-process adapter.
Using it would not exercise `Bun.spawn`. Fleet Deck therefore owns a narrow Bun process service
instead of assuming every package with `bun` in its name is Bun-native.

Fleet Deck is explicitly willing to own the missing generic Bun integration too. The application
adapter is the proving ground; once its behavior and the Effect service conformance suite pass, we
will upstream a genuinely Bun-native `ChildProcessSpawner` or maintain a Fleet Deck-scoped package
or pinned fork in lockstep with the selected Effect RC. The same route is open for other genuine
platform gaps such as the Node-shared `BunSocketServer`. Fleet Deck-specific policy stays in the
application layer, and we do not publish under the upstream `@effect` namespace without upstream
ownership.

If several generic gaps are confirmed, a Fleet Deck-maintained full fork of
`@effect/platform-bun` is explicitly acceptable: preserve the upstream public surface and tests,
replace only proven Node-shared delegates with Bun-native implementations, pin it by immutable
commit, and carry an upstream-base/rebase/rollback ledger. The detailed ownership gate is in the
implementation plan.

## Deliberate plain-TypeScript zones

These boundaries remain framework-free even after the migration is complete:

| Zone | Invariant |
|---|---|
| `contracts/` | Shared wire types and the deliberately loose fail-open validators do not import Effect. |
| `board/` | The browser consumes HTTP/WS contracts and never acquires the daemon runtime transitively. |
| thin `scripts/fleet-*.ts` hook shims | Hook startup and failure stay tiny, fast, and independent of the daemon dependency graph. |
| pure event/derive/snapshot/parser functions | Deterministic functions remain directly callable and cheap to test. |
| native platform adapters | `Bun.spawn`, `Bun.serve`, `bun:sqlite`, and any UDP adapter stay visible behind services rather than hidden under generic abstractions. |
| security predicates and atomic filesystem primitives | Exact constant-time, permission, fd, symlink, and durability semantics take precedence over stylistic uniformity. |

The import boundary is a performance and reliability feature. It prevents Effect from inflating
the board or per-event hook bundles and keeps the domain usable without a runtime.

## RC and unstable-module policy

The accepted initial cohort is:

```json
{
  "dependencies": {
    "effect": "4.0.0-rc.110",
    "@effect/platform-bun": "4.0.0-rc.110"
  }
}
```

`@effect/sql-sqlite-bun` may be added later only by its SQL gate, also at exactly
`4.0.0-rc.110`. Fleet Deck does not add the v3-era `@effect/platform`, `@effect/sql`, or
`@effect/experimental` packages; their v4 functionality moved into `effect` and
`effect/unstable/*`.

Rules:

1. Never use `@rc`, `@beta`, `^`, `~`, or a mutable Git branch for a direct Effect dependency.
2. Commit `bun.lock` and verify that every resolved v4 `effect`/`@effect/*` package uses the
   approved RC, except a registered immutable fork based on that same RC. Official
   `@effect/platform-bun` has a caret dependency on
   `@effect/platform-node-shared`, so the lockfile is part of the architecture.
3. Import the module that owns the API, for example `effect/Effect`, `effect/Layer`,
   `effect/unstable/process/ChildProcess`, and `@effect/platform-bun/BunRuntime`.
4. Keep an unstable-import register naming its owner, benefit, parity tests, and rollback seam.
5. Upgrade the entire cohort in one dedicated commit. Do not mix an RC bump with feature work.
6. Every bump reruns typecheck, all source and bundle tests, process-exit/lifecycle tests, import
   tripwires, and the cost harness. Read the exact tagged source and migration notes; do not
   trust v3 examples or snippets written for another RC.

Accepted unstable areas are process, HTTP/router, socket, and SQL **only when the
corresponding work package passes its parity gate**. Observability, RPC, workflow, cluster, AI,
and persistence modules are outside this migration unless a separate decision adds them.

## Compatibility evidence already established

On 2026-08-19:

- npm exposed matching `4.0.0-rc.110` releases for `effect`, `@effect/platform-bun`, and
  `@effect/sql-sqlite-bun`;
- a clean Bun 1.3.14 probe installed exact `effect@4.0.0-rc.110` and successfully ran
  `Effect.runPromise(Effect.succeed(...))`;
- package inspection confirmed `BunRuntime.runMain`, core v4 resource/concurrency APIs, Bun's
  native HTTP server implementation, the Node-backed child-process re-export, and the
  `bun:sqlite` SQL driver;
- the repo baseline was 137 test files and a generated daemon bundle of 566,619 raw bytes. P0 pins
  one deterministic gzip implementation before recording an enforceable compressed baseline.

That proves import/runtime compatibility, not production readiness. Shutdown behavior,
subprocess cancellation, HTTP parity, terminal streaming, bundle cost, and process keep-alive
remain implementation gates.

## Non-negotiable behavioral invariants

- hook parse, auth, validation, and infrastructure failures remain canonical `200 {}` no-ops;
- interruption represents cancellation/shutdown, not an operational error returned to users;
- startup invariant failures release acquired resources and preserve current exit codes;
- subprocesses preserve argv/no-shell execution, live environment, the shared 1 MiB output cap,
  exact result shapes, and TERM-to-KILL/process-group cleanup;
- native HTTP preserves body draining, stalled-request handling, auth, CSRF, WebSocket upgrade,
  payload, heartbeat, and backpressure semantics;
- SQLite migrations, transaction boundaries, row normalization, permissions, and durability do
  not change incidentally;
- pure domain output and every public HTTP/WS contract remain byte-for-byte compatible unless a
  separate product change deliberately versions them;
- no Effect code enters the board, contracts, or fail-open hook bundles;
- no test is weakened, quarantined, or rewritten merely to make the migration pass.

## Continuation rule

The architectural decision is **GO**. Individual adapter replacements are still green-or-replan:
Effect ownership continues even if `Bun.udpSocket`, `BunHttpServer`,
`@effect/sql-sqlite-bun`, or a compiled executable fails its own semantics/performance gate. A
failed optional adapter is recorded as **keep the proven native/Node-compatible implementation
behind the Effect service**, not as a failure of the whole migration.

The ordered commits, gates, budgets, rollback points, and copy-paste Codex goal are in
[effect-migration-plan.md](./effect-migration-plan.md).

## Primary sources

- [Effect v4 migration guide at RC.110](https://github.com/Effect-TS/effect/blob/66114151c2b4640bf773f2b3456ce70d679422f6/MIGRATION.md)
- [Effect services migration at RC.110](https://github.com/Effect-TS/effect/blob/66114151c2b4640bf773f2b3456ce70d679422f6/migration/services.md)
- [Effect runtime migration at RC.110](https://github.com/Effect-TS/effect/blob/66114151c2b4640bf773f2b3456ce70d679422f6/migration/runtime.md)
- [Effect BunRuntime source at RC.110](https://github.com/Effect-TS/effect/blob/66114151c2b4640bf773f2b3456ce70d679422f6/packages/platform/bun/src/BunRuntime.ts)
- [Effect Node-shared runMain signal behavior at RC.110](https://github.com/Effect-TS/effect/blob/66114151c2b4640bf773f2b3456ce70d679422f6/packages/platform/node-shared/src/NodeRuntime.ts)
- [Effect Bun child-process adapter source at RC.110](https://github.com/Effect-TS/effect/blob/66114151c2b4640bf773f2b3456ce70d679422f6/packages/platform/bun/src/BunChildProcessSpawner.ts)
- [Effect Bun HTTP server source at RC.110](https://github.com/Effect-TS/effect/blob/66114151c2b4640bf773f2b3456ce70d679422f6/packages/platform/bun/src/BunHttpServer.ts)
- [Bun child-process documentation](https://bun.sh/docs/runtime/child-process)
- [Bun HTTP server documentation](https://bun.sh/docs/runtime/http/server)
- [Bun SQLite documentation](https://bun.sh/docs/runtime/sqlite)
- [Bun UDP documentation](https://bun.sh/docs/runtime/networking/udp)
