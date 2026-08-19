# Effect unstable-import register

This register is the P2 review gate for direct `effect/unstable/*` source imports. The repository
scanner requires the source import set and the rows below to match exactly. A wildcard, an unused
row, or an import without a row fails `tests/import-boundaries.test.ts`.

Every row must name one exact module import and record why stable core APIs were insufficient, the
test files that own its behavior, the exact Effect release candidate last reviewed, and the single
service or adapter module whose removal rolls the experiment back. Update the row in the same
change as its import. Candidate areas in the migration plan are not pre-approvals.

<!-- unstable-import-registry:start -->
| Exact import | Rationale | Owning tests | Last reviewed RC | Rollback module |
| --- | --- | --- | --- | --- |
<!-- unstable-import-registry:end -->

The register is intentionally empty at P2. The approved cohort is `4.0.0-rc.110`; an RC upgrade
must review every populated row together with the dependency cohort.

## Candidate policy

- `effect/unstable/process/*`: comparison-only until it matches Fleet Deck's direct-argv,
  combined-output, cancellation, timeout, and descendant-cleanup contract. The default remains the
  app-local `Bun.spawn` process service.
- `effect/unstable/http/*`: application-route trial only after P6 characterization; it does not
  approve a transport switch.
- `effect/unstable/socket/*`: P7 terminal trial only. A Node-shared server re-export is not treated
  as Bun-native.
- `effect/unstable/sql/*`: P8 semantics and benchmark branch only, with the matching Bun driver.
- `effect/unstable/observability/*`: deferred; no observability backend is introduced by this
  migration.
- Cluster, RPC, workflow, AI, persistence, eventlog, and workers modules remain out of scope.

Core `effect/Schema` is stable vocabulary, not an unstable-register entry, and does not authorize
an incidental replacement of shared contract validators.
