# Effect migration ledger

This ledger is the per-slice record required by the Effect migration plan. Update the relevant row
before starting the next work package. A compatibility bridge must name its deletion package; an
unstable import or detached task must name its owning adapter or registry entry; and the rollback
column must contain the local commit that cleanly removes the slice.

The baseline starts from branch `fd/v1-effect-feasibility` at
`a9fb3a2abf207c5805d4d9f0d9661fb124a4a49f`. The pre-existing dirty documentation scope and the
machine/runtime facts are preserved in [baseline.md](baseline.md) and its JSON companions.

| Work package | Status | Compatibility bridge | Unstable imports | Detached work allowlist | Focused tests | Benchmark delta | Rollback commit |
|---|---|---|---|---|---|---|---|
| P0 — evidence | Complete | None | None | None | 1,223 source pass + 1 skip; 1,214 shipped pass + 10 skip; exact-cohort probe; four paired comparisons; CI policy 3/3 | Floor recorded: bundle 566,619 B raw / 147,431 B gzip; pack 453,844 B; legacy exec p50 10.254 ms; all stable evidence equal | `aeda84b` |
| P1 — explicit resource handles | Not started | Plain `DaemonResources` owner, deleted/replaced in P4 | None | None permitted | Resource-prefix, double-close, held-hook, poller/timer, HTTP/WS/terminal, DB-close-order suites | Pending | Pending |
| P2 — exact Effect kernel | Not started | In-memory/fake service layers; temporary test helpers | Register every non-stable Effect import in the kernel adapter registry | None permitted | Exact-version, import-boundary, source/generated scan, natural-exit and keep-alive fixtures | Pending | Pending |
| P3 — Bun subprocess pilot | Not started | One `BootstrapRuntimeBridge` for Promise callers, deleted in P4/P13 | Any experimental process API must stay in the Bun process adapter | Process-group detachment only through the process-driver registry | Differential exec/files parity, cancellation/tree/reap, cap/UTF-8, cwd/stdin/env suites | Pending | Pending |
| P4 — root runtime and shutdown | Not started | Atomic replacement of the bootstrap bridge and plain root owner | Root-runtime APIs only inside the Bun runtime adapter | None beyond registered process-tree policy | Acquisition-prefix, signals/deadline, active-resource, exit/log/readiness/takeover suites | Pending | Pending |
| P5 — boot and schedules | Not started | Promise entry façades removed as each workflow moves | Scheduler APIs isolated behind app services | None permitted | Boot order, readiness, reconciliation, single-flight poller and retry suites | Pending | Pending |
| P6 — HTTP and WebSocket workflows | Not started | Existing wire-facing façade retained only for staged caller migration | Transport experiments confined to the HTTP adapter | None permitted | Raw HTTP/WS parity, disconnect cancellation, close/drain/backpressure suites | Pending | Pending |
| P7 — terminal ownership/transport | Not started | Legacy stream decoder allowed only through the measured transport decision | Terminal adapter registry only | None permitted | Byte-order/parser/bridge, viewer lifecycle, backpressure, real-tmux macOS/Linux soak | Pending | Pending |
| P8 — store/SQLite workflows | Not started | Existing store façade retained during caller migration | SQLite adapter registry only | None permitted | Migration, strict bindings, close order, contention/restart/cache and SQL differential suites | Pending | Pending |
| P9 — application workflows | Not started | Caller façades deleted as each workflow completes | App workflow adapters only | Only plan-approved process-independent work | Domain parity plus cancellation/error/finalization suites | Pending | Pending |
| P10 — holds and fail-open | Not started | Hook wire façade retained; manual hold maps deleted on closure | None outside hold service adapter | None permitted | Hook/needs-you/board-hold failure and race matrix; canonical `200 {}` release-all | Pending | Pending |
| P11 — remaining Bun adapters | Not started | Only explicitly accepted KEEP/DEFER adapters | Capability registry with evidence link per use | Process detachment registry only | Per-capability conformance and soak suites | Pending | Pending |
| P12 — build and distribution | Not started | Selected builder is the only production pipeline | Builder adapter only if unstable | None permitted | Reproducible source/generated/packed-install parity and artifact scans | Pending | Pending |
| P13 — cleanup, observability, docs | Not started | No stale bridge allowed | Registry must match remaining imports exactly | Registry must match remaining detached work exactly | Boundary, observability, docs/plan consistency and full global gate | Pending | Pending |
| P14 — RC rehearsal and release gate | Not started | None added by the rehearsal | Updated exact-cohort registry | No new detached work | Exact cohort bump, full local and authorized remote matrix | Pending | Pending |
