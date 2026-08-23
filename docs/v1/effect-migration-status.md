# Effect migration checkpoint status

- **Checkpoint date:** 2026-08-23
- **Branch:** `fd/v1-effect-feasibility`
- **Published branch:** `origin/fd/v1-effect-feasibility` currently at `b2d11d84`
  (`feat(effect): convert the hook route group under the fail-open boundary`)
- **Current implementation HEAD:** `b2d11d84`
  (`feat(effect): convert the hook route group under the fail-open boundary`)
- **P6 completion evidence:** [p6-http-matrix.md](./evidence/effect/p6-http-matrix.md),
  [p6-route-wave.md](./evidence/effect/p6-route-wave.md),
  [p6-graceful-stop-verification.md](./evidence/effect/p6-graceful-stop-verification.md),
  [p6-bench-comparison.md](./evidence/effect/p6-bench-comparison.md)
- **P6.3 HttpServer owner:** `e7900bac`
  (`feat(effect): own the Bun listener as the HttpServer root service`)
- **P5 completion evidence:** [p5.md](./evidence/effect/p5.md)
- **P4 root-cutover checkpoint:** `661dfe31b66843f70a1dcebbc4f340ad9c62f76f`
- **P3 rollback anchor:** `bcf3337e48d7dd35437d2e2369d0a91fbcbfa114`
- **Runtime floor:** Bun 1.3.14, revision `0d9b296af33f2b851fcbf4df3e9ec89751734ba4`

This is the durable handoff for the executable
[Effect migration plan](./effect-migration-plan.md). P5 and P6 are complete.
Implementation HEAD equals origin at `b2d11d84`. This documentation is
uncommitted (including the P6.6 / P6.8 evidence files). No pull request has
been opened, and nothing has been tagged, released, or deployed. The next
session starts P7.

## Executive status

| Package | State | Resume point |
| --- | --- | --- |
| P0 | Complete | Baselines, workloads, probes, comparisons, and CI policy are recorded. |
| P1 | Complete | Explicit resource owners and ordered shutdown remain the rollback seam. |
| P2 | Complete | The exact Effect RC cohort, kernel, boundaries, and Bun conformance are recorded. |
| P3 | Implementation complete | Bun process routing is live. Quiet-host performance evidence remains an explicit ledger item. |
| P4 | Implemented and checkpointed | The root cutover and shutdown evidence are complete for the pre-P5 artifact. Do not relabel that evidence as measuring the current P5 tree. |
| P5 | Complete | Prompt failure publication, the reviewed detached-owner exception, whole-slice rollback, gzip budget, and quiet global suites are recorded at `ca62b94f`. |
| P6 | Complete at `b2d11d84` | Exit gate met with the five dispositions in [effect-migration-plan.md](./effect-migration-plan.md) P6 (cancellation-on-disconnect is deliberately none-yet). |
| P7 | Not started | Terminal bridge under Effect ownership; the termbridge facade is the seam. Plan P7 is the spec. P7.0 is the §7 platform authorization checkpoint. |
| P8–P14 | Not started | Continue in plan order after P7. |

P3's paired quiet-host performance evidence is unchanged and out of P5 scope.
Do not close it from these suites.

## P5 completion at `ca62b94f`

HEAD `ca62b94f` on Bun 1.3.14, quiet WSL2 host, 2026-08-22. Evidence:
[p5.md](./evidence/effect/p5.md). Ledger:
[migration-ledger.md](./evidence/effect/migration-ledger.md).

### Outcome of the three open P5 gates

1. **Prompt defect publication — closed** by `02d25b62`. Each of the four
   top-level background children is wrapped in `Effect.onExit` publishing every
   unexpected non-success exit to the ready/failure latches (first-wins
   `Deferred.doneUnsafe`) before the aggregate sibling join. Requested-shutdown
   interrupts are suppressed via a shared `shutdownRequested` box that
   `interrupt()` flips synchronously before `fiber.interruptUnsafe()`.
   Regression `tests/effect/background-prompt-failure.test.ts`: (a) a defecting
   child fails the root promptly while a sibling is stuck in a never-settling
   `ownedLegacyPromise`, within a bounded close; (b) requested shutdown does
   not trip the failure latch. Mutation check: neutralizing the observe wrapper
   makes test (a) fail. Adversarial review (independent model, grok-4.6):
   verdict SHIP, zero findings across eight attack angles, verified against
   `effect@4.0.0-rc.110` internals (`OnExit` is an uninterruptible stack
   continuation completing before `FiberImpl` publishes the exit to observers;
   `forEachConcurrent` reacts only via `addObserver`; `Cause.hasInterruptsOnly`
   cannot suppress mixed defect+interrupt causes). One noted benign delta:
   `owner.close()` after a prompt defect can resolve as requested-interrupt
   instead of rejecting `BackgroundDefectExitError`; production shutdown keys
   off `Background.awaitFailure` (`shutdownTriggerFromExit`), not `close()`'s
   Promise.
2. **Detached-owner exception — accepted.** The single `Effect.forkDetach` in
   `background-owner.ts` is the one reviewed deadline-bounded detached owner: it
   exists so a stuck legacy Promise cannot make root Scope closure exceed the
   P4 hard deadline; the owner is manually registered, interrupted, and
   bounded-joined. Making every legacy bridge cancellation-bounded now was
   rejected. The ledger's former “detached work: None permitted” statement is
   amended to name exactly this exception.
3. **Rollback — whole-slice only.** Reverting the P5 slice commits `972621d5`
   through `ca62b94f` (listed in [p5.md](./evidence/effect/p5.md)) returns to
   the P4 root-cutover anchor `661dfe31`. The previous implication of a
   selectable per-scheduler rollback is dropped because `createCore` no longer
   owns the retention scheduler. No per-scheduler claim remains.

### Outcome of the open bundle-size gate

Closed by `a857bce3` without raising a ceiling and without whitespace
minification. Esbuild flags are unchanged.

- Five secret-redaction regexes (GitLab family, `AIza`, `sk-`, `hf_`,
  `dop_v1_`) had been appended to `SECRET_VALUE_RES` a second time with a
  redundant comment block; the duplicates were deleted (originals remain, each
  pattern now runs once).
- The bundle script gained a post-step deleting esbuild's `/* @__PURE__ */`
  annotations from the generated daemon (bundler hints, meaningless in a
  terminal artifact).

Accepted daemon identity at HEAD `ca62b94f`:

- Raw: 651,685 B, below the 768,000 B ceiling (116,315 B headroom).
- gzip-9 zlib: 188,452 B, 988 B under the 189,440 B ceiling.
- SHA-256: `89b3ef7723961102cb55f986f1710524ee96d55a093db09e7b963c324ee801d9`.
- Double-build byte-identical.
- `tests/effect/daemon-bundle-policy.test.ts`: 2 pass, 0 fail.
- Bundle 19,206 lines; diagnostic name strings present.

The pre-fix P5 checkpoint identity (666,615 B raw / 189,773 B gzip-9, 333 B
over ceiling, SHA-256
`529a1612068705c2e3deccce8574f340bfc6c565307cbe2aece60bc27da6223a`) is
historical and is not the accepted P5 artifact.

### Fixture closures on the same slice

`d53e57a3` and `ca62b94f` (no assertion weakened, 2000 ms budget not raised):

- Packed-install smoke now substitutes the tracked bundle path across the
  recipe tail, so the PURE-strip post-step cannot mutate the committed artifact
  while comparing an unstripped scratch build.
- P1 clone shim arms its TERM/INT trap before writing readiness (trap-window
  race was 6/300 at 44-way oversubscription, 0/300 trap-first).
- Acquisition fixture scrubs `FLEETDECK_TEST_DAEMON_SCRIPT` from the child env
  so bundle-mode runs do not emit the boot seam banner.
- Clone shim block is `sleep 1 & wait` so dash services a pending group-SIGTERM
  immediately. Differential bisection: `tests/process-driver-reference.test.ts`
  immediately before `p1-spawns-lifecycle` reproduced 3/3 pre-fix, 5/5 green
  post-fix; four other driver-heavy prefixes never failed.

### Quiet global suites at HEAD `ca62b94f`

Run on Bun 1.3.14, quiet WSL2 host, otherwise idle, 2026-08-22:

- `bun run test` = 1,493 pass, 0 fail, 186 files, 538.60 s, exit 0.
- `bun run test:bundle` = 1,484 pass, 9 skip (platform skips), 0 fail, exit 0.

Logs: `/tmp/fd-effect/quiet-test-3.log` and
`/tmp/fd-effect/quiet-test-bundle-3.log`.

## P5 production wiring (from `972621d`)

The production integration at `972621d`, still true at HEAD:

- `prepareBackgroundOwner` separates cold preparation from one idempotent fiber
  start.
- One aggregate background program owns boot reconciliation, retention, agents
  polling, and LAN refresh.
- `AppConfigLive` replaces the transitional root configuration service.
- `acquireDaemonResources` receives the prepared Background service/controller
  and returns a cold background program.
- The live Layer starts and registers the single Background owner before
  lifecycle ownership is sealed, then publishes both `Background` and
  `DaemonLifecycle`.
- The root waits on `Background.awaitFailure` instead of an unconditional
  `Effect.never`.
- The legacy boot Promise chain, legacy agents-poll start, and legacy
  network-watch start are no longer production entrypoints.
- `createCore` no longer owns the boot retention sweep or ten-minute retention
  interval. It exposes narrow `retentionSweep` and `pruneEvents` capabilities
  for the Effect schedule.
- Legacy agents and network cadence timers are no longer unref'ed; explicit
  owner shutdown now governs natural exit.
- A real BunRuntime fixture proves the three async scheduler finalizers finish
  before store close, callbacks remain stable afterward, and the process exits
  naturally without `process.exit`.
- The sole `Effect.cached` startup wrapper was replaced with a cold,
  single-assignment `Deferred` gate.

The question orphan sweep remains the explicit P1 handle until P10 (the allowed
P5.5 choice). P5.1–P5.7 plan boxes are checked.

The same integration checkpoint also stabilized two previously failing test
fixtures (BUG-145 tmux cleanup; BUG-153 spawn-reconciliation durable-row
assertion). Those remain in the tree; they are not re-litigated here.

## P4 evidence remains historical

Accepted P4 evidence in [p4.md](./evidence/effect/p4.md) and
[p4-shutdown.json](./evidence/effect/p4-shutdown.json) measures the pre-P5 tree:

- Source closure prefix: `15c62de5…`.
- Daemon bundle: 688,199 B raw and 186,978 B gzip-9.
- Bundle SHA-256: `da3c674a088d9ff2ab624f422c85c2095cec389f6ac2e57343a95bbc6e8d6e88`.
- 100 measured shutdowns plus ten warmups passed their exit, response,
  WebSocket, discovery, pidfile, listener, socket, child, timer, and
  root-keepalive residue gates.

That evidence is still valid for the P4 checkpoint, but it must not be rewritten
as if it describes the current P5 artifact.

## P6 complete at `b2d11d84`

P6 is complete. Sub-slices (historical, all landed):

- **P6.1** freeze (`46e13c50` evidence, `9332d576` plan box, `307fae0a` freeze
  tests). Matrix: [p6-http-matrix.md](./evidence/effect/p6-http-matrix.md). WS
  send probe: [p6-ws-send-probe.md](./evidence/effect/p6-ws-send-probe.md).
- **P6.2** pure policy extract `d425cc96`; tripwire retarget `3735759e`.
- **P6.3** `e7900bac` (`feat(effect): own the Bun listener as the HttpServer
  root service`). `HttpServer` Context.Service is published beside Background;
  `makeHttpServerOwner` wraps the byte-unchanged `createHttp` transport; the
  direct `http.bind` production entrypoint is retired for `httpServer.bind`.
  Root-Scope retirement fallback is registered during acquire (LIFO after the
  coordinator's release). Sole bridge = `IngressSupervisor.runPromiseExit`
  (readiness on first bind; `HttpServer.runRequest` with
  `ApplicationQuiescingError` refusal). Adversarial review: SHIP-WITH-NITS;
  both SHOULD-FIXes applied. P6.3-commit identity (historical, not HEAD):
  654,290 B raw / 189,229 B gzip-9 / SHA-256
  `0b89888b4336531fa91500833b0c16f1be4dfb01a34c47e7deea7caffa5c1d5f`.
- **P6.7** **KEEP CUSTOM ADAPTER**. Evidence:
  [p6-transport-trial.md](./evidence/effect/p6-transport-trial.md). Four
  blocking rc.110 gaps (no `getBufferedAmount`/`terminate`/`ping`/`drain`;
  `upgrade()` overwrites custom `ws.data`; shutdown cannot express
  `stop(false)`-once-race-deadline-`stop(true)` and ends in `process.exit(130)`;
  `server.timeout(request, N)` is TS-private) plus a gzip delta that alone
  exceeds the ceiling.
- **P6.4** complete — see below.
- **P6.5** preserved as implemented (probe + matrix §3; WS slice extracted the
  decisions as pure leaves byte-identically; `ws-hardening` / `terminal-ws`
  stayed green).
- **P6.6** complete. All seven clauses implemented and pinned. Evidence:
  [p6-graceful-stop-verification.md](./evidence/effect/p6-graceful-stop-verification.md).
  Nuance: `stop(true)` is awaited on every path except hard absolute-deadline
  exhaustion (deliberate, pinned).
- **P6.8** within budget. 12/12 `/health`+`/state` p95 cells PASS the +10%
  line. Evidence: [p6-bench-comparison.md](./evidence/effect/p6-bench-comparison.md);
  [p6-postconv.json](./evidence/effect/p6-postconv.json) vs
  [p6-baseline.json](./evidence/effect/p6-baseline.json). The `/command`
  harness gap is closed: `command` is a tenth workload (`POST {text: note}`
  → 200 `core.command` relay) in `p6-http-bench.ts` and `--workload=all`.
  Quiet-host numbers wait on the next idle-machine slot; smoke-only until then.

### P6.4 wave and closing slices

Wave evidence: [p6-route-wave.md](./evidence/effect/p6-route-wave.md). Matrix
overlay: [p6-http-matrix.md](./evidence/effect/p6-http-matrix.md).

| Commit | Slice | Review |
| --- | --- | --- |
| `56a15e8a` | health/state (G1, G2) — P6.4 pilot; three template SHOULD-FIXes applied | SHIP-WITH-NITS |
| `cffb9dea` | paste-image (P8); static recorded legacy-until-P13 | SHIP-WITH-NITS |
| `c7eeb641` | settings/command/mail/cleanup (P6, P7, P3, P4) | initially **DO-NOT-SHIP** |
| `62ef3c0f` | control, 11 routes (P12–P22) | initially **DO-NOT-SHIP** |
| `e2518a63` | join-on-interrupt + bundle line-comment strip | **CLOSED-SHIP-WITH-NITS** |
| `a1ea6020` | `/ws` snapshot converted-by-ownership + pure leaves; `/ws/term` until P7 | — |
| `3eeb2641` | hook fail-open contract | — |
| `b2d11d84` | hook route group (LAST; fail-open settler) | SHIP-WITH-NITS; both nits applied |

**Defect family (headline — record honestly).** The bridge's `runPromiseExit`
was `runPromise(Effect.exit(...))` and **rejected** under external interruption
(settler defect 500 instead of any Exit); and async mutating settlers answered
503 while a started native Promise still owned SQLite/tmux, unsticking
`res.done` so `closeClients` no longer joined in-flight mutations before
closing-store (risks: `SQLITE_MISUSE`, partial cleanup Clear, silent
success-after-refusal). All 190+ green tests missed it; two independent
adversarial reviews caught it on both slices. Fix: `Effect.runPromiseExitWith`
at the bridge (true Exits; P4 contracts re-verified) + `startOnce` witness with
join-on-interrupt (503 only when the operation provably never ran); live-bridge
tests now pin join-before-resolve. Residual accepted nit: a shutdown-only
duplicate error-log line on controlAsync interrupt × rejection races (bytes
single and correct).

**18 HTTP routes + hooks** through the Effect bridge. Excluded-by-design: GET
`/mail` + GET `/api/watch` (held/lease semantics, under P1 owners until P10),
static assets (legacy until P13). `/ws/term` stays behind the termbridge
facade until P7. Leftover application handlers (P2 ack, P5 worktrees/remove,
P9 arm, P10 preflight, P11 spawn, G3 GET settings, G4–G6 fs/worktrees) remain
legacy for later packages, not P6 incompleteness.

**Exit-gate dispositions at `b2d11d84`:**

- **no scattered runtime runners** — the sole bridge is
  `IngressSupervisor.runPromiseExit` via `HttpServer.runRequest`
  (import-boundary + owner tests).
- **exact HTTP/WS parity** — freeze tests + fail-open contract + byte-identity
  fixtures across every slice.
- **cancellation on disconnect where safe** — DELIBERATELY NONE YET: admitted
  requests JOIN their operations (the frozen `res.done` invariant restored by
  `e2518a63`); disconnect-triggered cancellation was judged not-yet-safe for
  converted routes and is deferred to the packages that make the underlying
  operations abort-aware. This is the reviewed disposition, not a silent pass.
- **all transport resources root-owned** — P6.3 owner + fallback ordering test.
- **performance within budget** — P6.8 evidence.

**Quiet global suites at HEAD `b2d11d84`** (Bun 1.3.14, quiet WSL2, 2026-08-23):

- `bun run test` (source) = **1,578 pass / 6 skip / 0 fail**.
- `bun run test:bundle` one run had a single fail in
  `tests/process-driver-reference.test.ts` ("Node reference close is
  idempotent...", 1030 ms). **5/5 green** in bundle-mode isolation; the full
  rerun was **1,569 pass / 15 skip / 0 fail**. Recorded as a suite-context
  flake occurrence in a historically timing-sensitive file (second documented
  incident involving this file; a recurrence earns bisection).
- The 6/15 skips are the fail-open contract's declared fault-injection skips
  plus platform skips.

**Accepted daemon identity at HEAD `b2d11d84`:**

- Raw: 603,458 B.
- gzip-9 zlib: 164,890 B, **24,550 B** under the 189,440 B ceiling.
- SHA-256: `cab4fbc2f66565d72e52f62c52e00edd73c359798fc5f590f18fe43cebb7c874`.
- Deterministic.

Standing landmine: version manifests still `0.23.6` while daemon + bundle
changed. `hook-integrity` needs a version bump ×4 before a PR to main /
publish. Not a reason to revert conversions.

### Open notes (do not close from P6)

- P3's paired quiet-host performance evidence remains an explicit ledger item.
- Quiet-host recapture of the tenth (`command`) harness workload — smoke-only
  until the next idle-machine slot; do not mix with the nine-workload
  `comparison.key`.
- Suite-context flake in `tests/process-driver-reference.test.ts` as above.
- Version-manifest landmine (`0.23.6` ×4) still stands for any merge-to-main.

## Exact resume order

P6 exit gates are closed. The next session starts **P7 — terminal bridge under
Effect ownership**. The termbridge facade is the seam; the plan's P7 section
is the spec.

1. Confirm the checkpoint and runtime:

   ```sh
   git switch fd/v1-effect-feasibility
   git status --short
   git log --oneline -8
   bun --version
   ```

   Expected HEAD **and** origin are `b2d11d84`. This documentation may still be
   uncommitted (including `docs/v1/evidence/effect/p6-bench-comparison.md`,
   `p6-postconv.json`, `p6-graceful-stop-verification.md`); do not switch
   branches. Leave untracked `.claude/agents/` and `/tmp/fd-wt-*` alone.

2. **P7.0** is the §7 platform authorization checkpoint: after P6 is locally
   green, prepare the branch and ask for permission to push and open/update a
   draft PR so the blocking macOS/real-tmux and Linux lifecycle jobs can run.
   Implementation through `b2d11d84` is already on origin; this documentation
   is not. At this checkpoint `hook-integrity` may be intentionally red
   because version closure has not happened; record that expected failure, but
   P7 cannot close until its named platform jobs are actually green. If
   authorization is withheld, pause — the local goal is not complete. Do not
   mark P3's quiet-host performance item closed. Do not start P8–P14.

3. Then P7.1 (expand parser/protocol fixtures) onward per the plan. Keep the
   proven parser and `StringDecoder` initially; change decoding only in a
   separate parity commit. If Bun stream/FileSink semantics or performance
   fail, record **KEEP scoped Node-stream transport** and continue the Effect
   ownership migration.

## P7 preflight constraints

From the plan's P7 section and the constraints still in force from P6:

- The termbridge facade is the seam (`openViewer()` Promise/handle
  compatibility) until the terminal WS route is fully Effect-native (P7.6);
  then remove the facade and its runtime bridge.
- Do not use unbounded `Stream.runCollect`, `ChildProcessSpawner.string`, or
  an unbounded Queue. Buffer limits and drop/backpressure policy are external
  behavior. Every Queue has an explicit `Queue.shutdown` finalizer; root
  PubSubs also have explicit shutdown finalizers.
- A command timeout tears down the compromised shared client as today. Viewer
  child scopes close only that viewer; ref-count or root closure owns the
  shared client lifetime.
- Keep hook holds/watch waiters under their P1 owners until P10.
- Introduce a temporary exact legacy `Core` service; do not force the
  SQLite/P8 migration early.
- Request-bridged Effects must NOT `yield* HttpServer` / `DaemonLifecycle` /
  `Background` — the ingress runtime captured the pre-daemon Context (`R`
  must stay within `AppConfig | ProcessRunner | ProcessRuntimeControl`).
- Consumers key off `HttpServer.state()`, never a retained address value.
- Preserve the established held-response barriers and `stop(false)` /
  `stop(true)` ordering on Bun 1.3.14. Keep the custom HTTP adapter.
- Disconnect-triggered cancellation of converted HTTP routes is **not** in
  P7's gift: admitted requests JOIN (P6 exit-gate disposition). Do not enable
  request interruption until the underlying operations are abort-aware.
- One integrator should own shared edits to `http.ts`, `live-layer.ts`, and
  `program.ts`.
- Full P6 revert must include live-layer wiring, not only `program.ts`.
  Per-group HTTP rollback remains `effectRoutes=null`.

## Repository handoff expectation

This documentation is currently uncommitted. Implementation HEAD equals
`origin/fd/v1-effect-feasibility` at `b2d11d84`. No pull request has been
opened. After the documentation is committed, the next session starts P7
from `fd/v1-effect-feasibility`, beginning at the P7.0 authorization
checkpoint. Leave untracked `.claude/agents/` and `/tmp/fd-wt-*` alone.
