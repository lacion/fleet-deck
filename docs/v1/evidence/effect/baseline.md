# P0 Effect migration baseline

Recorded on 2026-08-19 before production runtime code or dependencies were changed. Timing files in
this directory are machine-local evidence; correctness and leak assertions are portable, while
absolute macOS and Linux performance numbers remain separate datasets.

## Source state

- Branch: `fd/v1-effect-feasibility`
- Commit: `a9fb3a2abf207c5805d4d9f0d9661fb124a4a49f`
- Test files: 137 (`tests/**/*.test.ts`)
- Production dependencies: none; no `effect` or `@effect/*` package was present
- Bun floor: 1.3.14

The worktree was intentionally not clean when P0 began. These were pre-existing documentation
changes and are not attributable to the migration implementation:

```text
 M docs/v1/README.md
 M docs/v1/architecture.md
 M docs/v1/foundations-hardening.md
 M docs/v1/validation-and-gates.md
?? docs/v1/effect-feasibility.md
?? docs/v1/effect-migration-plan.md
```

P0 scripts and evidence therefore record both the starting dirty scope above and the complete live
dirty scope captured by each run. No baseline measurement assumes a clean checkout.

## Machine and runtime

| Field | Value |
|---|---|
| OS | Darwin 25.5.0 (`RELEASE_ARM64_T6041`) |
| Architecture | arm64 |
| CPU | Apple M4 Pro, 14 logical CPUs |
| Memory | 25,769,803,776 bytes |
| Bun | 1.3.14 |
| Bun revision | `0d9b296af33f2b851fcbf4df3e9ec89751734ba4` |

The machine-readable metadata, artifact sizes, timings, process samples, package contents, and
residue checks are in `baseline-run-1.json` and `baseline-run-2.json`. The same pinned runtime and
machine produced both runs. `Bun.gzipSync` with level 9 is the only gzip measurement used for the
daemon budget.

## Unchanged global gate

Before adding P0 implementation files, every build target was regenerated twice without an
artifact diff. TypeScript, board-test TypeScript, and Biome passed. The unchanged behavioral suites
reported:

| Target | Pass | Skip | Fail | Test files | Duration |
|---|---:|---:|---:|---:|---:|
| Source (`bun run test`) | 1,220 | 1 | 0 | 137 | 755.53 s |
| Shipped daemon (`bun run test:bundle`) | 1,211 | 10 | 0 | 137 | 702.03 s |

The different skip counts are the suite's existing source-versus-shipped and platform predicates;
neither run had a failure.

## Repeatable evidence

Run from the repository root after regenerating all shipped artifacts:

```sh
bun run effect:p0:baseline --out=docs/v1/evidence/effect/baseline-run-1.json
bun run effect:p0:exec --out=docs/v1/evidence/effect/exec-run-1.json
bun run effect:p0:workloads --out=docs/v1/evidence/effect/workloads-run-1.json
bun run effect:p0:probe --out=docs/v1/evidence/effect/effect-probe-run-1.json
```

Repeat with `run-2` output names on the same machine. The scripts fail when a correctness,
deadline, deterministic-build, finalization, signal, or residue invariant fails. Distribution
metrics are reported as p50/p95/p99 after their configured warmups; raw run data remains in the
JSON rather than being copied into this narrative.

The Effect probe creates and removes a disposable project outside the repository. It installs the
exact initial cohort there, never touching the production manifest or lockfile, and proves source
execution, a Bun-generated bundle, scoped finalization, natural exit, root-fiber keep-alive, and
SIGINT/SIGTERM interruption cleanup.

## Recorded floor

Both runs passed every correctness and residue assertion, and all four two-run comparisons report
`ok: true`, identical stable evidence, and identical metric shape. The principal floor values are:

| Surface | Recorded floor |
|---|---:|
| Daemon bundle | 566,619 bytes raw; 147,431 bytes with `Bun.gzipSync` level 9 |
| Packed archive | 453,844 bytes; 20 files; byte-identical across two packs |
| Production build | All four targets byte-identical across two complete pipelines |
| Cold daemon health | p50 47.726 ms; p95 168.796 ms; p99 169.485 ms (30 launches) |
| Reconciliation readiness | p50 114.027 ms; p95 235.057 ms; p99 236.717 ms |
| Graceful idle shutdown | p50 2.166 ms; p95 6.671 ms; p99 10.186 ms |
| Legacy short subprocess | p50 10.254 ms; p95 11.964 ms; p99 67.332 ms (1,000 measured after 100 warmups) |
| Effect cohort closure | 20 resolved packages; 53,823,351 installed bytes |
| Effect probe bundle | 83,456 bytes |

The workload pair also passes the HTTP concurrency and raw-body matrices, 15-session state data,
WebSocket snapshot/backpressure/reconnect/heartbeat cases, ten-viewer terminal fanout, Fleet Deck
SQLite migration/contention/restart cases, event-loop sampling, and idle/busy/held-request shutdown
cases. These are comparison floors, not promises that every timing is machine-independent.

The exact-cohort probe pins `effect@4.0.0-rc.110` and
`@effect/platform-bun@4.0.0-rc.110` in its isolated fixture. It observed acquire/complete/release in
that order for source and bundle execution, held the root fiber alive for an explicit 250 ms
observation before each signal, finalized on both SIGINT and SIGTERM, and exited with code 130.

P0 closed with TypeScript, board-test TypeScript, Biome, and the CI policy tests green. The four
production build targets were regenerated twice with identical hashes. The post-P0 source suite
reported 1,223 pass, 1 platform skip, and 0 failures; the shipped-daemon suite reported 1,214 pass,
10 platform/source-only skips, and 0 failures. Both ran 1,224 tests across 138 files.

## Evidence map

- `baseline-run-{1,2}.json` and `baseline-comparison.json`: build, package, cold-start, command,
  five-minute idle, mDNS, forced-stop, and process-residue evidence.
- `exec-run-{1,2}.json` and `exec-comparison.json`: the legacy subprocess contract and resource
  floor, including exact byte caps, cancellation, resistant children, process groups, and inherited
  pipes.
- `workloads-run-{1,2}.json` and `workloads-comparison.json`: HTTP, WebSocket, terminal, SQLite,
  event-loop, and active-shutdown evidence.
- `effect-probe-run-{1,2}.json` and `effect-probe-comparison.json`: exact dependency closure,
  source/bundle execution, keep-alive, interruption, and finalization evidence.
