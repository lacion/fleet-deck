# Effect migration checkpoint status

**Checkpoint date:** 2026-08-20
**Branch:** `fd/v1-effect-feasibility`
**Implementation checkpoint:** `661dfe31b66843f70a1dcebbc4f340ad9c62f76f`
**Previous P3 anchor / P4 rollback target:** `bcf3337e48d7dd35437d2e2369d0a91fbcbfa114`
**Runtime floor:** Bun 1.3.14, revision `0d9b296af33f2b851fcbf4df3e9ec89751734ba4`

This is the durable handoff for the executable
[Effect migration plan](./effect-migration-plan.md). The implementation checkpoint was committed
locally at the user's request. It has not been pushed, published, tagged, or released.

## Executive status

| Package | State | Resume point |
| --- | --- | --- |
| P0 | Complete and committed | Baselines, workloads, probes, comparisons, and CI policy are recorded. |
| P1 | Complete and committed | Explicit resource owners and ordered plain shutdown remain the rollback seam. |
| P2 | Complete and committed | Exact Effect RC cohort, kernel, boundaries, and Bun conformance are recorded. |
| P3 | Implementation complete and committed | Bun process routing is live. Quiet-host performance evidence remains an explicit ledger item. |
| P4 | Implemented and checkpointed at `661dfe3` | Focused, packed, artifact, and measured shutdown gates are green. The final isolated global source and bundle reruns remain open because the latest source run found two failures. |
| P5 | Isolated slices implemented, not production-integrated | Integrate one aggregate Background owner before `DaemonResources` is sealed; then remove the replaced legacy timers. |
| P6 | Read-only preflight complete | Freeze route/wire behavior, extract pure policy, then introduce one scoped Bun HttpServer adapter. |
| P7–P14 | Not started | Continue in plan order after the P5/P6 exit gates. |

## P4 checkpoint that is already proven

P4 replaces the bootstrap `ManagedRuntime` with one `BunRuntime.runMain` root, one captured-context
`IngressSupervisor`, an eight-phase `LifecycleCoordinator`, first/second-signal policy, typed
startup/defect/interruption exits, force-aware process and terminal ownership, and pidfile release
at actual host teardown. The generated daemon and packed CLI exercise the same path.

Accepted evidence is in [p4.md](./evidence/effect/p4.md) and
[p4-shutdown.json](./evidence/effect/p4-shutdown.json):

- 100 measured shutdowns plus ten warmups, across source and generated daemon, all passed residue
  checks.
- Graceful idle p95: 6.676 ms source and 6.014 ms bundle, below the 500 ms gate.
- Worst measured forced total: 223.079 ms source and 220.594 ms bundle, below the 1,750 ms root
  deadline.
- mDNS goodbye p95: 219.746 ms source and 219.634 ms bundle, below the separate 1,000 ms watchdog.
- Generated daemon: 688,199 B raw and 186,978 B gzip-9, SHA-256
  `da3c674a088d9ff2ab624f422c85c2095cec389f6ac2e57343a95bbc6e8d6e88`.
- Two full daemon/bin/hook builds had aggregate SHA-256
  `7e4a56a2b7c1b269e6ff34b979fa456bc12eb30a8c113f710db4985a252716cf`.
- Two packs were identical at 493,842 B with SHA-256
  `5edb764bdb6e99872fee7a7afaaa111e88c2dbb5cbae7a8d6f6a8008a58babe9`.
- P4-focused aggregate: 124 pass, 0 fail. Generated boundary/policy/freshness group: 22 pass,
  0 fail. Packed installed-bin lifecycle: green.

## Latest verification state

The fast gates were rerun immediately before `661dfe3`:

- `bun run typecheck`: pass for daemon and board.
- `bun run ci`: pass, Biome checked 382 files.
- `git diff --check`: pass.

The latest full source run is **not an accepted global gate**:

```text
1459 pass, 1 skip, 2 fail, 1 error
1462 tests across 180 files in 1148.79s
```

Failures to resolve before updating the P4 full-suite placeholders:

1. `tests/cleanup-api.test.ts` — `BUG-145: Clear and dismiss fail loud when tmux is unreachable,
   with a retry path`. It failed waiting five seconds for a manually created remain-on-exit pane to
   become dead. It reproduces alone (`2 pass, 1 fail`). A manual probe showed the new tmux pane's
   login zsh still blocked in `/usr/libexec/path_helper -s`, so the injected `exit` never ran. Treat
   that as a host/fixture lead, not yet a proven product-code cause.
2. `tests/spawn.test.ts` — `restart reconciliation removes a spawn-owned worktree left by a spawn
   interrupted before launch (BUG-153)`. It timed out at 60 seconds after failing to observe the
   spawn become live within eight seconds. This case has not yet been rerun alone.

That source run overlapped the tail of focused agent verification, so it is unsuitable as a quiet
acceptance measurement even apart from the reproducible cleanup fixture failure. The final shipped
bundle suite was deliberately not started after the source gate failed.

Resume verification in this order:

1. Run the BUG-145 case alone after fixing or stabilizing its tmux fixture.
2. Run the BUG-153 case alone and diagnose any repeat.
3. Keep all agents and other test commands idle; run `bun run test` once.
4. If source is green, run `bun run test:bundle` once against the same tree.
5. Replace `FINAL_P4_SOURCE_SUITE` and `FINAL_P4_BUNDLE_SUITE` in
   [p4.md](./evidence/effect/p4.md), then update the P4 ledger row.

Do not reinterpret the accepted `p4-shutdown.json` closure hash as covering later P5 files. The P5
modules are additive and not reachable from the daemon root yet; the measured P4 artifact remains
the byte-identical `da3c674a…` bundle.

## P5 work already present

The following boundary-clean slices and focused tests are implemented in `661dfe3`, but they are
not wired into production:

- `app/background-owner.ts` and `services/background.ts`: status `Ref`, readiness/failure
  `Deferred`s, explicit fiber owner, joined legacy Promise adapter; focused 7/7.
- `app/boot-reconciliation.ts`: clear-fork first, reconciliation plus first retention concurrently,
  broadcast flush last, degraded named failures, defect propagation, joined interruption; focused
  8/8.
- `app/agents-poll.ts`: scoped ProcessRunner polling, active/idle cadence, no overlap, fail-open
  named skips, in-flight cancellation/join; focused 6/6.
- `app/lan-refresh.ts` and `fixed-grid-schedule.ts`: delayed fixed-grid cadence, missed-tick skip,
  no overlap, typed recovery, joined interruption; focused 7/7.
- `app/retention-schedule.ts`: immediate boot sweep, ten-minute fixed grid, first-run readiness,
  typed retry and no overlap; focused 7/7.
- `app/app-config-live.ts`: cold AppConfig Layer, exact port-before-HOME/version ordering,
  source/bundle path normalization, typed startup refusal; focused 8/8.

Production still runs the legacy boot Promise chain, `startAgentsPoll`, `startNetworkWatch`, and
`createCore`'s boot-retention Promise plus interval. Integrating the new modules without removing
those paths would double-run work.

### P5 integration order

1. Split background preparation from fiber start. HTTP needs the same synchronous
   `reconciliationStatus()` before `createHttp`, while the owner must start only after native daemon
   acquisition. A prepared value should expose `service`, `controller`, and an idempotent `start`.
2. Make `lanRefresh` copy and validate its callbacks/options at construction, matching agents and
   retention.
3. In `derive.ts`, keep plain `retentionSweep(nowMs)` and expose a narrow event-prune callback.
   Remove its automatic boot sweep, ten-minute interval, and retention lifecycle. Leave the
   question orphan sweep on its explicit P1 owner until P10.
4. Change `acquireDaemonResources` to accept prepared Background readiness and return cold
   background capabilities instead of starting legacy producers. Keep mDNS as discovery.
5. Build one aggregate background program containing retention, agents, LAN, and boot. Pass
   `retention.awaitFirstRun` directly into boot reconciliation; do not wrap it in another legacy
   Promise.
6. In `makeDaemonLifecycleLayer`, use this exact order: prepare signals; acquire daemon; attach the
   process driver; start the aggregate owner; register it as a producer; then construct the
   coordinator, which seals resources. Publish both `DaemonLifecycle` and `Background`.
7. Replace `TransitionalAppConfig` with `AppConfigLive` and remove duplicate port/HOME/version
   probing from `program.ts`.
8. Replace root `Effect.never` with `Background.awaitFailure`; retain lifecycle close in `onExit`
   inside the provided Layer.
9. Add one aggregate `TestClock` test and one real BunRuntime/natural-exit fixture. Only after the
   latter passes, remove the replaced `unref()` calls in legacy agents polling, network watch, and
   derive retention. Do not remove question or mDNS/watchdog timers.

Production must give the Background owner a shutdown bound compatible with the 1,750 ms root and
250 ms force reserve; the current generic default join timeout is only 1,000 ms.

## P6 preflight handoff

`http.ts` remains a roughly 3,100-line monolith containing pure policy, Node-shaped request/response
shims, body/FIN handling, all routes, snapshot and terminal WebSockets, `Bun.serve`, and lifecycle.
The existing phased shutdown behavior is already hardened and must be preserved.

Key constraints:

- Introduce a temporary exact legacy `Core` service; do not force the SQLite/P8 migration early.
- Adapt and join every legacy async route Promise before request interruption can be enabled.
- Keep hook holds/watch waiters behind their P1 lifecycle owners until P10.
- Keep termbridge internals behind the present owned facade until P7.
- Characterize Bun WebSocket `send()` return values (`-1`, `0`, positive) before defining a new
  backpressure abstraction.
- Make import-time HTTP env caps and FIN timers cold/injectable without changing bytes.
- Freeze the P5 `whenBroadcastIdle` readiness handoff before moving HTTP ownership.

Recommended parallel lanes after P5 integration is stable:

1. P6.1 machine-readable route/wire matrix and black-box characterization.
2. Pure HTTP/hook/auth/security policy extraction with byte-exact differential tests.
3. `HttpServer` service plus scoped Bun transport/lifecycle adapter.
4. WebSocket send-result and terminal-ingress characterization.

One integrator should exclusively own `http.ts`, `live-layer.ts`, and `program.ts`; route agents
should stay in new workflow and test files.

## Repository handoff checks

At the start of the next session:

```sh
git switch fd/v1-effect-feasibility
git status --short
git log --oneline -5
bun --version
```

Expected implementation anchor is `661dfe3`; the follow-up documentation commit contains this
handoff. There should be no staged or unstaged migration work after that documentation commit.
Nothing from this checkpoint has been pushed.
