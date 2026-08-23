# Effect migration checkpoint status

- **Checkpoint date:** 2026-08-23
- **Branch:** `fd/v1-effect-feasibility`
- **Published branch:** `origin/fd/v1-effect-feasibility` currently at `e2518a63`
  (`fix(effect): join started mutations under interruption`)
- **Current implementation HEAD:** `e2518a63`
  (`fix(effect): join started mutations under interruption`)
- **P6.4 wave evidence:** [p6-route-wave.md](./evidence/effect/p6-route-wave.md)
- **P6.3 HttpServer owner:** `e7900bac`
  (`feat(effect): own the Bun listener as the HttpServer root service`)
- **P5 completion evidence:** [p5.md](./evidence/effect/p5.md)
- **P4 root-cutover checkpoint:** `661dfe31b66843f70a1dcebbc4f340ad9c62f76f`
- **P3 rollback anchor:** `bcf3337e48d7dd35437d2e2369d0a91fbcbfa114`
- **Runtime floor:** Bun 1.3.14, revision `0d9b296af33f2b851fcbf4df3e9ec89751734ba4`

This is the durable handoff for the executable
[Effect migration plan](./effect-migration-plan.md). P5 is complete. P6 is
active: P6.1–P6.3 and P6.7 are done; the P6.4 wave converted 18 application
routes at `e2518a63` (box stays open); the P6.8 harness is in tree and the
quiet-host baseline is captured at `ac438c21`. Implementation HEAD equals
origin. This documentation is uncommitted. No pull request has been opened,
and nothing has been tagged, released, or deployed.

## Executive status

| Package | State | Resume point |
| --- | --- | --- |
| P0 | Complete | Baselines, workloads, probes, comparisons, and CI policy are recorded. |
| P1 | Complete | Explicit resource owners and ordered shutdown remain the rollback seam. |
| P2 | Complete | The exact Effect RC cohort, kernel, boundaries, and Bun conformance are recorded. |
| P3 | Implementation complete | Bun process routing is live. Quiet-host performance evidence remains an explicit ledger item. |
| P4 | Implemented and checkpointed | The root cutover and shutdown evidence are complete for the pre-P5 artifact. Do not relabel that evidence as measuring the current P5 tree. |
| P5 | Complete | Prompt failure publication, the reviewed detached-owner exception, whole-slice rollback, gzip budget, and quiet global suites are recorded at `ca62b94f`. |
| P6 | Active; P6.1–P6.3, P6.7 done; P6.4 wave 18/remaining WS+hooks; P6.8 harness landed, baseline captured | WS-snapshot slice next, then hooks-contract-test, then hooks (LAST). P6.6 graceful stop is still open (`res.done` join invariant restored). P6.8 comparison still pending. |
| P7–P14 | Not started | Continue in plan order after P6. |

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

## P6 progress

P6 is the active work. Sub-slices landed on 2026-08-22:

- **P6.1** freeze is committed (`46e13c50` evidence, `9332d576` plan box,
  `307fae0a` freeze tests). Matrix:
  [p6-http-matrix.md](./evidence/effect/p6-http-matrix.md). WS send probe:
  [p6-ws-send-probe.md](./evidence/effect/p6-ws-send-probe.md).
- **P6.2** pure policy extract is committed at `d425cc96`; tripwire retarget
  `3735759e` (published origin).
- **P6.3** done at `e7900bac` (`feat(effect): own the Bun listener as the
  HttpServer root service`). `HttpServer` Context.Service is published beside
  Background; `makeHttpServerOwner` wraps the byte-unchanged `createHttp`
  transport; the direct `http.bind` production entrypoint is retired for
  `httpServer.bind`. Root-Scope retirement fallback is registered during
  acquire (LIFO after the coordinator's release; the coordinator retires the
  listener via the phased `beginGracefulStop`/`forceStop` and never calls
  `http.lifecycle.close` — the fallback genuinely starts `closeHttpOnce` as a
  safe second pass behind the transport's own latches). Sole bridge =
  `IngressSupervisor.runPromiseExit` (readiness on first bind;
  `HttpServer.runRequest` with `ApplicationQuiescingError` refusal).
  Adversarial review: SHIP-WITH-NITS, zero blockers; both SHOULD-FIXes applied
  (truthful comments; a focused test pinning coordinator-then-fallback LIFO).
  Gates: 18 regression suites green (incl. full root/lifecycle/acquisition
  battery, natural-exit fixtures, freeze tests), 7 focused owner tests. Bundle
  identity at that commit: 654,290 B raw / 189,229 B gzip-9 / SHA-256
  `0b89888b4336531fa91500833b0c16f1be4dfb01a34c47e7deea7caffa5c1d5f`,
  deterministic. Record these as the P6.3-commit identity, not as a frozen
  ceiling fact — a concurrent worker is recovering gzip headroom, so the
  numbers will change again before the next global run.
- **P6.7** decided **KEEP CUSTOM ADAPTER** (the plan's continuation rule
  records this as success). Evidence:
  [p6-transport-trial.md](./evidence/effect/p6-transport-trial.md). Spike
  branch `fd/p6-7-spike` @ `3cf7aa29`; scratch worktree `/tmp/fd-wt-p6-7`.
  Four blocking rc.110 gaps: (1) Socket surface lacks
  `getBufferedAmount`/`terminate`/`ping`/`drain`; (2) `upgrade()` overwrites
  custom `ws.data`; (3) shutdown cannot express
  `stop(false)`-once-race-deadline-`stop(true)` and ends in `process.exit(130)`;
  (4) `server.timeout(request, N)` is only a TS-private field. Also:
  +434,628 B raw / +118,002 B gzip scratch delta (gzip delta alone exceeds the
  gzip ceiling) and visible fiber-per-request overhead (1,174 vs 1,876 rps on
  a `/health` smoke). Byte-drift: default 404 and failed-upgrade responses are
  empty vs the daemon's JSON bodies.
- **P6.8** harness merged as `d5404aac` (`scripts/effect-migration/p6-http-bench.ts`
  + `.md`): pure-Bun load harness, workloads
  `health`/`state`/`hook`/`hook-fail-open`/`paste`/`withheld`/`ws`/`static-shell`/`static-asset`,
  targets `source|bundle`, p50/p95/p99 + rps JSON output, `--label=baseline`
  implying the Bun-floor check. The pre-conversion quiet-host baseline was
  captured at `ac438c21` (`docs/v1/evidence/effect/p6-baseline.json`, taken at
  `51d39ddd` on Bun 1.3.14 immediately after fully green global suites —
  source 1,502/0; bundle 1,493 pass, 9 skip). Representative concurrency is
  1/8/32. P6.8 exit criterion: `/health`+`/state` p95 ≤ baseline+10%;
  throughput judged only with p95/p99+correctness. Do not check the P6.8 plan
  box until the post-conversion comparison is in.

P6.4's plan box stays open (18 routes converted; WS-snapshot and hooks remain).
P6.5's contract is already frozen (preserve as implemented). P6.6 remains open.

## P6.4 wave at `e2518a63`

Five commits on `fd/v1-effect-feasibility`, all pushed, parent `ac438c21`.
Evidence: [p6-route-wave.md](./evidence/effect/p6-route-wave.md). Matrix overlay:
[p6-http-matrix.md](./evidence/effect/p6-http-matrix.md).

| Commit | Slice | Review |
| --- | --- | --- |
| `56a15e8a` | health/state (G1, G2) — P6.4 pilot; three template SHOULD-FIXes applied | SHIP-WITH-NITS |
| `cffb9dea` | paste-image (P8); static recorded legacy-until-P13 | SHIP-WITH-NITS |
| `c7eeb641` | settings/command/mail/cleanup (P6, P7, P3, P4) | initially **DO-NOT-SHIP** |
| `62ef3c0f` | control, 11 routes (P12–P22) | initially **DO-NOT-SHIP** |
| `e2518a63` | join-on-interrupt + bundle line-comment strip | **CLOSED-SHIP-WITH-NITS** |

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

**18 routes** through the Effect bridge. Excluded/remaining: GET `/mail` + GET
`/api/watch` (held/lease semantics, under P1 owners until P10), leftover
application POSTs (P2 ack, P5 worktrees/remove, P9 arm, P10 preflight, P11
spawn, G3 GET settings, G4–G6), static assets (legacy until P13), WS-snapshot
ingress slice (next), hooks/fail-open group (LAST, behind an exhaustive
fail-open contract test per the full-spine guardrail).

**Quiet global suites at HEAD `e2518a63`** (Bun 1.3.14, quiet WSL2, 2026-08-23):

- `bun run test` = 1,554 pass, 0 fail, 192 files, 530.42 s, exit 0.
- `bun run test:bundle` = 1,545 pass, 9 skip, 0 fail, 192 files, 519.53 s, exit 0.

Logs: `/tmp/fd-effect/quiet-test-9.log` and
`/tmp/fd-effect/quiet-test-bundle-9.log`.

**Accepted daemon identity at HEAD `e2518a63`** (line-comment strip in the
post-step; minify-equivalence proof byte-identical; 18 lexer self-checks):

- Raw: 601,875 B.
- gzip-9 zlib: 164,469 B, **24,971 B** under the 189,440 B ceiling. The
  recurring ceiling squeeze is retired.
- SHA-256: `b9c02c5f00abe54edb516e2859b5de61badca18472220c5ac774b8bcf145f771`.
- 19,841 lines; deterministic. Generated bundle carries no comments now
  (line numbers, kept names, shebang, banner preserved).

Standing landmine: version manifests still `0.23.6` while daemon + bundle
changed. `hook-integrity` needs a version bump ×4 before a PR to main /
publish. Not a reason to revert conversions.

## Exact resume order

P5 exit gates are closed. The P6.4 wave converted 18 routes; the next session
continues P6.4 at the WS-snapshot slice.

1. Confirm the checkpoint and runtime:

   ```sh
   git switch fd/v1-effect-feasibility
   git status --short
   git log --oneline -8
   bun --version
   ```

   Expected HEAD **and** origin are `e2518a63`. This documentation may still be
   uncommitted; do not switch branches. Leave untracked `.claude/agents/` and
   `/tmp/fd-wt-*` alone.

2. Convert the **WS-snapshot** ingress slice (next P6.4 group). Then write the
   exhaustive fail-open **contract test**, then convert the **hooks** slice
   (LAST). Then close P6.6 / P6.8 (comparison against `ac438c21`). Do not mark
   P3's quiet-host performance item closed. Do not start P7–P14. Do not treat a
   busy-host P6.8 smoke JSON as the captured baseline — that baseline already
   exists at `docs/v1/evidence/effect/p6-baseline.json`.

## P6 preflight constraints

P6 is the active work. Constraints from the read-only preflight, still in
force, plus P6.3 owner constraints that route workers inherit:

- Introduce a temporary exact legacy `Core` service; do not force the SQLite/P8
  migration early.
- Adapt and join every legacy async route Promise before enabling request
  interruption.
- Keep hook holds/watch waiters under their P1 owners until P10 and termbridge
  behind its owned facade until P7.
- Characterize Bun WebSocket `send()` return values before defining the new
  backpressure policy. **SATISFIED 2026-08-22.** Evidence:
  [p6-http-matrix.md](./evidence/effect/p6-http-matrix.md) §3 and
  [p6-ws-send-probe.md](./evidence/effect/p6-ws-send-probe.md). Two headlines:
  `send()` returns are consulted nowhere in `http.ts`, and `GET /state` vs
  `/ws` snapshot differ intentionally (`lan` block present only in HTTP).
- Preserve the established held-response barriers and `stop(false)`/`stop(true)`
  ordering on Bun 1.3.14. P6.7 confirmed the platform-bun adapter cannot
  express that machine; keep the custom adapter.
- One integrator should own shared edits to `http.ts`, `live-layer.ts`, and
  `program.ts`; parallel route work should stay in new workflow and test files.
- **P6.4 (a)** request-bridged Effects must NOT `yield* HttpServer` /
  `DaemonLifecycle` / `Background` — the ingress runtime captured the
  pre-daemon Context (`R` must stay within
  `AppConfig | ProcessRunner | ProcessRuntimeControl`) — use the callback-held
  service reference instead.
- **P6.4 (b)** consumers key off `HttpServer.state()`, never a retained
  address value.
- **P6.4 (c)** rollback nuance: reverting `program.ts`'s wiring alone leaves a
  truthfully-unbound published service while the real listener still runs — a
  full P6.3 revert must include the live-layer wiring.

## Repository handoff expectation

This documentation is currently uncommitted. Implementation HEAD equals
`origin/fd/v1-effect-feasibility` at `e2518a63`. No pull request has been
opened. After the documentation is committed, the next session continues P6.4
at the WS-snapshot slice from `fd/v1-effect-feasibility`. Leave untracked
`.claude/agents/` and `/tmp/fd-wt-*` alone.
