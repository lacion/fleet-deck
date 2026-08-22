# Effect migration checkpoint status

- **Checkpoint date:** 2026-08-22
- **Branch:** `fd/v1-effect-feasibility`
- **Published branch:** `origin/fd/v1-effect-feasibility`
- **Current implementation checkpoint:** `972621d` (`feat(effect): supervise daemon background schedules`)
- **P4 root-cutover checkpoint:** `661dfe31b66843f70a1dcebbc4f340ad9c62f76f`
- **P3 rollback anchor:** `bcf3337e48d7dd35437d2e2369d0a91fbcbfa114`
- **Runtime floor:** Bun 1.3.14, revision `0d9b296af33f2b851fcbf4df3e9ec89751734ba4`

This is the durable handoff for the executable
[Effect migration plan](./effect-migration-plan.md). The checkpoint branch is published to
`origin` for continuation. No pull request has been opened, and nothing has been tagged, released,
or deployed.

## Executive status

| Package | State | Resume point |
| --- | --- | --- |
| P0 | Complete | Baselines, workloads, probes, comparisons, and CI policy are recorded. |
| P1 | Complete | Explicit resource owners and ordered shutdown remain the rollback seam. |
| P2 | Complete | The exact Effect RC cohort, kernel, boundaries, and Bun conformance are recorded. |
| P3 | Implementation complete | Bun process routing is live. Quiet-host performance evidence remains an explicit ledger item. |
| P4 | Implemented and checkpointed | The root cutover and shutdown evidence are complete for the pre-P5 artifact. Do not relabel that evidence as measuring the current P5 tree. |
| P5 | Production-integrated checkpoint; exit gate open | Resolve the current gzip budget miss, run the two quiet global suites, then record P5 evidence and ledger completion. |
| P6 | Read-only preflight complete | Freeze route/wire behavior, extract pure policy, then introduce one scoped Bun HTTP server adapter. |
| P7–P14 | Not started | Continue in plan order after the P5 exit gate. |

P5 is deliberately not marked complete in the plan or migration ledger. Its production wiring is
present, but its bundle-size and final global-suite gates are still open.

## What `972621d` implements

P5 background work is now integrated into the production daemon root:

- `prepareBackgroundOwner` separates cold preparation from one idempotent fiber start.
- One aggregate background program owns boot reconciliation, retention, agents polling, and LAN
  refresh.
- `AppConfigLive` replaces the transitional root configuration service.
- `acquireDaemonResources` receives the prepared Background service/controller and returns a cold
  background program.
- The live Layer starts and registers the single Background owner before lifecycle ownership is
  sealed, then publishes both `Background` and `DaemonLifecycle`.
- The root waits on `Background.awaitFailure` instead of an unconditional `Effect.never`.
- The legacy boot Promise chain, legacy agents-poll start, and legacy network-watch start are no
  longer production entrypoints.
- `createCore` no longer owns the boot retention sweep or ten-minute retention interval. It exposes
  narrow `retentionSweep` and `pruneEvents` capabilities for the Effect schedule.
- Legacy agents and network cadence timers are no longer unref'ed; explicit owner shutdown now
  governs natural exit.
- A real BunRuntime fixture proves the three async scheduler finalizers finish before store close,
  callbacks remain stable afterward, and the process exits naturally without `process.exit`.

The same checkpoint also stabilizes two previously failing test fixtures:

- BUG-145: the tmux cleanup fixture sets remain-on-exit atomically, runs `/usr/bin/true`, and owns
  its test server cleanup instead of relying on a login shell/path helper.
- BUG-153: the spawn-reconciliation fixture asserts the durable `spawning` row directly instead of
  depending on a best-effort hook POST and transient `live` observation.

The generated daemon was rebuilt with identifier and syntax minification while preserving names and
line-oriented diagnostics. The package recipe intentionally does not use whitespace minification.

## Verification at this checkpoint

Completed against `972621d` on Bun 1.3.14:

- `bun run typecheck`: pass.
- `bun run ci`: pass; Biome checked 387 files.
- `git diff --check`: pass.
- P5 background focused group: 33 pass, 0 fail.
- Real Bun P5 natural-exit fixture: 5/5 repeated passes; latest single rerun also passed.
- Root/lifecycle/acquisition group: 27 pass, 0 fail.
- Production acquisition matrix: 4 pass, 0 fail.
- Real source signal/lifecycle group: 4 pass, 0 fail.
- Agents/LAN/mDNS group: 25 pass, 1 platform skip.
- BUG-145: 10/10 repeated passes; full cleanup API file 3/3.
- BUG-153: 11/11 repeated source passes; full spawn file 21/21; generated focused case 1/1.
- Regenerated-bundle signal escalation smoke: 1 pass.
- Import boundaries plus P3 production selection: 13 pass, 0 fail.

The full `bun run test` and `bun run test:bundle` suites have **not** been rerun after P5
integration. The earlier full-source failures are now understood and fixed as fixture defects, but a
quiet global rerun is still required before closing P5.

## Open bundle-size gate

The current generated daemon is deterministic and below the raw ceiling, but it exceeds the gzip-9
ceiling:

- Raw: 668,967 B, below the 768,000 B ceiling.
- gzip-9 zlib: 190,303 B, which is 863 B above the 189,440 B ceiling.
- SHA-256: `8d17c569e5b8600b38134b41e9391a656ced7dfed2d25daf4ed471754a75d5b4`.
- `tests/effect/daemon-bundle-policy.test.ts`: one pass and one intentional failure on the gzip
  ceiling only.

Do not silently raise the ceiling or add `--minify-whitespace`. The next session should either
remove at least 863 compressed bytes while preserving diagnostic quality, or obtain explicit
maintainer acceptance and record the exception in evidence.

## P4 evidence remains historical

Accepted P4 evidence in [p4.md](./evidence/effect/p4.md) and
[p4-shutdown.json](./evidence/effect/p4-shutdown.json) measures the pre-P5 tree:

- Source closure prefix: `15c62de5…`.
- Daemon bundle: 688,199 B raw and 186,978 B gzip-9.
- Bundle SHA-256: `da3c674a088d9ff2ab624f422c85c2095cec389f6ac2e57343a95bbc6e8d6e88`.
- 100 measured shutdowns plus ten warmups passed their exit, response, WebSocket, discovery,
  pidfile, listener, socket, child, timer, and root-keepalive residue gates.

That evidence is still valid for the P4 checkpoint, but it must not be rewritten as if it describes
the current P5 artifact.

## Exact resume order

1. Confirm the checkpoint and runtime:

   ```sh
   git switch fd/v1-effect-feasibility
   git status --short
   git log --oneline -5
   bun --version
   ```

2. Resolve the 863 B gzip overage without whitespace minification or obtain and document explicit
   acceptance.
3. Rebuild daemon/bin/hooks twice and verify deterministic build and pack identities.
4. Run the bundle policy, import-boundary, P5 aggregate, and real natural-exit gates.
5. Keep the host otherwise idle; run `bun run test`, then `bun run test:bundle` against the same
   tree.
6. Record P5 evidence, update the migration ledger, and check the applicable P5 plan boxes only
   after every exit gate is green.
7. Begin P6 from the frozen HTTP behavior matrix.

## P6 preflight constraints

- Introduce a temporary exact legacy `Core` service; do not force the SQLite/P8 migration early.
- Adapt and join every legacy async route Promise before enabling request interruption.
- Keep hook holds/watch waiters under their P1 owners until P10 and termbridge behind its owned
  facade until P7.
- Characterize Bun WebSocket `send()` return values before defining the new backpressure policy.
- Preserve the established held-response barriers and `stop(false)`/`stop(true)` ordering on Bun
  1.3.14.
- One integrator should own shared edits to `http.ts`, `live-layer.ts`, and `program.ts`; parallel
  route work should stay in new workflow and test files.

## Repository handoff expectation

After the documentation checkpoint commit, the worktree should be clean, with no staged migration
changes and no known fixture processes or listeners left behind. The next session should fetch and
continue from `origin/fd/v1-effect-feasibility`; `972621d` remains the implementation anchor beneath
the status-only handoff commits. No pull request has been opened.
