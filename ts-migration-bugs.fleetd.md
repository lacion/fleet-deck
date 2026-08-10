<!-- STAGING for scripts/fleetd/fleetd.ts (the daemon entry) and the two call-site
     tightenings its stricter graph surfaced in spawns.ts / http.ts — merge into
     ts-migration-bugs.md (newest-first, right below the "entries appended below"
     marker) once the concurrent board subagent has finished its own append. Kept
     separate only to avoid a concurrent-append race on the shared log. The
     INITIAL-conversion findings for spawns.ts / http.ts live in
     ts-migration-bugs.spawns.md / ts-migration-bugs.http.md; the entries below are
     the ENTRY-WIRING ripple — what pinning SpawnsCtx's function fields and
     destructuring createHttp's return forced at the call sites. -->

### fleetd.ts — the daemon entry (~920 loc): NO runtime bug; the entry graph's strictness surfaced ONE real EOPT bug in spawns.ts (two call sites) + one destructure-safety idiom in http.ts; the entry itself is all faithful noise with every security contract preserved   [1 BUG (spawns) / rest NOISE]

- **What:** the last `.mjs` on the daemon graph — `fleetd.ts` is the process entry Claude
  Code's `start` script and the production bundle both root at. Converted `.mjs` → `.ts` under
  root maximal-strict; `start` repointed to `fleetd.ts` (node ≥22.18 type-strips it — probed
  `v22.22.2`, boots to `/health` 200 `"startup":"settled"`), `bundle` repointed input
  `fleetd.mjs`→`fleetd.ts` + banner `Source: *.mjs`→`*.ts`. `fleetd.mjs` is intentionally NOT
  deleted this commit — it still imports the same `.ts` siblings and stays the tested source
  entry until the test-conversion phase repoints the launchers/tests; keeping it green means
  every intermediate HEAD boots either way.

- **Faithfulness proven by an ISOLATED bundle diff, not a source diff.** A naive `fleetd.mjs`
  vs `fleetd.ts` source diff is meaningless (486 "changed" lines that are all added type
  annotations). Instead: esbuild-bundle BOTH entries against the identical current `.ts`
  sibling graph (`bundle(fleetd.mjs)` vs `bundle(fleetd.ts)`) and diff the OUTPUTS — that
  cancels the siblings and isolates the entry conversion to ~40 hunks, every one of which is:
  1. **`process.env.X` → `process.env["X"]`** (`noPropertyAccessFromIndexSignature`; `ProcessEnv`
     is an index signature). Pure syntax, ~20 sites.
  2. **`(process.env.X || "default").trim() || "default"` → `(process.env["X"] ?? "").trim() ||
     "default"`** on BIND / PROXY_AUTH / REQUIRE_TOKEN / TRUST_LOOPBACK / MDNS_NAME. Proven
     behavior-identical across unset / "" / "  " / set: the outer `.trim() || fallback` already
     collapses empty-and-whitespace to the fallback, so swapping the inner `|| "default"` for
     `?? ""` never changes the result. `?? ""` is the EOPT/`prefer-nullish-coalescing` form.
  3. **`err?.code || err?.message || "unknown error"` → `errText(err)`** and **`err?.code` →
     `errCode3(err)`**, two extracted helpers over `err instanceof Error`. Faithful: every
     thrown value on these fs/startup paths is a Node `Error` carrying a string `.code`, so
     `errText` returns code→message→"unknown error" exactly as the `||` chain did; for a
     non-Error both old and new yield "unknown error" / `undefined`. The helpers also kill ~10
     copies of the same untyped idiom.
  4. **`.version || version` → `.version ?? version`** on the `package.json` version read. Differs
     only for a falsy-but-non-nullish version (`""` / `0`) — impossible for a real semver field;
     `??` is strictly MORE correct (a literal `0.0.0` wouldn't wrongly fall back). Lint-driven.
  5. **`process.on("SIGINT", shutdown)` → `process.on("SIGINT", () => { void shutdown(); })`**
     (and the same for SIGTERM + `void core.reconcileSpawns().catch(...)`). Wraps the async
     handler so its promise is explicitly voided (`no-floating-promises` / `no-misused-promises`);
     the signal handler always ignored the return, so the `void`/arrow is documentation, not
     a move.
  6. **Trivia, all faithful:** `let settleReconciliation;` → `= null` + `resolve` wrapped as
     `() => { resolve(); }` and called `settleReconciliation?.()` (types it `(() => void) | null`,
     the optional-call guards the never-taken pre-assignment read); `for (const e of entries ||
     [])` → `?? []` (entries is `NetworkInterfaceInfo[] | undefined` — never falsy-non-null);
     `timer.unref?.()` → `timer.unref()` (Node `Timeout` always has `unref`; the `?.` guarded a
     browser-number timer that never occurs here); a `var AUTH_TOKEN = TOKEN` alias captured
     right after the mint/persist block so the share-URL / capture-secret closures hold a stable
     `const` snapshot of the final minted token (TOKEN is a `let` reassigned only DURING the
     mint, before this line — same value, faithful).

- **THE ONE SECURITY-CRITICAL HUNK — verified faithful against the source, not the lossy bundle:
  BUG-156 takeover arbitration (`supersedeIfNewer`).** The `.mjs` guarded a null `/health`
  incumbent with chained optionals; the `.ts` adds an early `if (!incumbent) return false;`:
  - `.mjs`: `if (incumbent?.managed) return false;` / `if (!shouldTakeOver(version,
    incumbent?.version)) return false;` / `if (incumbent?.pid !== record.pid) return false;`
  - `.ts`: `if (!incumbent) return false;` then non-optional `incumbent.managed` /
    `shouldTakeOver(version, incumbent.version)` / `incumbent.pid`.
  - **Why faithful:** when `incumbent` is null the `.mjs` skips the `managed` check
    (`null?.managed` falsy) and reaches `shouldTakeOver(version, undefined)`, which returns
    `false` (its contract: `if (!own || !other) return false` — an unparseable version on either
    end never evicts on a guess), so `!false` → `return false` — the EXACT outcome the `.ts`
    early guard produces two lines sooner. The skipped `managed` check and `shouldTakeOver` call
    have no side effects, so there is no observable difference. Confirmed at runtime:
    `takeover.test.mjs` 17/17 in the bundle lane, INCLUDING "a MANAGED daemon is never evicted"
    and "an UNMANAGED daemon of the same age is still evicted". The sibling
    `record.port === null || !Number.isInteger(record.port)` is likewise runtime-identical
    (`Number.isInteger(null)` is already false); the `=== null` only narrows `number | null`.
  - Every other daemon security contract is preserved verbatim: pidfile HOME lock (`wx`),
    token mint/persist + 0600 chmod-on-open, EADDRINUSE→exit-3 election, `startupFatal` cleanup,
    boot-reconciliation readiness (`settleReconciliation`), LAN/mDNS lifecycle, MANAGED no-evict.

- **THE REAL BUG the entry wiring surfaced — spawns.ts, TWO call sites, EOPT + closure narrowing
  loss [BUG]:** pinning `SpawnsCtx`'s function fields to `ReposSurface['X']`
  (validateBranch / resolveTarget / cloneRepo / materializeBranch / touchRepo / claimTarget /
  targetOwner) so derive.ts's `Object.assign(ctx, createSpawns(ctx))` type-checks
  `CoreCtx <: SpawnsCtx` under strictFunctionTypes exposed two call-site gaps the pre-migration
  `any` hid:
  1. **`resolveTarget(body)` — structural optional-vs-required under exactOptionalPropertyTypes.**
     `SpawnBody.repo?: string` (optional) is NOT assignable to `ResolveTargetBody.repo: string`
     (required) even after the value-narrowing `if (body.repo == null) return` guard —
     optional-vs-required is STRUCTURAL, narrowing the VALUE never flips the PROPERTY. The `.mjs`
     passed the whole `body` (a superset) and relied on `resolveTarget` ignoring the extra
     fields — fine at runtime, formally unsound. **Fix:** build the ResolveTargetBody explicitly
     from the four fields the callee reads — `{ repo: body.repo, repo_host: body.repo_host ??
     null, repo_transport: body.repo_transport ?? null, repo_org: body.repo_org ?? null }` —
     with `?? null` mapping an absent override to the field's declared `string | null` (null /
     undefined / absent all mean "no override"; the untyped call only ever passed `undefined`
     because it was `any`). Zero runtime move, now type-correct.
  2. **`materializeBranch({ branch })` — control-flow narrowing lost across a closure boundary.**
     The `if (!body.branch) return` guard narrows `body.branch` to `string`, but that narrowing
     is LOST inside the nested `Promise.resolve().then(async () => { … })` provisioning closure
     (property narrowing does not survive a nested-function boundary; the callee then wants
     `string`, got `string | undefined`). **Fix:** capture `const branch = body.branch` in the
     enclosing scope right after the guard — a `const` keeps its `string` type into the closure —
     and pass `branch,` (shorthand). Faithful; the same value either way.

- **The destructure-safety idiom the entry forced in http.ts [NOISE]:** the daemon entry does
  `const { refreshLan } = createHttp(...)`. `createHttp` returned `refreshLan` as METHOD
  SHORTHAND (`refreshLan(x) {}`), which types the field as a METHOD and trips
  `@typescript-eslint/unbound-method` on the destructure. **Fix:** arrow-property
  (`refreshLan: (x) => { … }`) — behaviorally identical (the body closes over
  `refreshLanHosts` / `lan`, never `this`) but types the field as a PROPERTY, destructure-safe.
  Matches the codebase's established pattern (SpawnsCtx already uses arrow-property for every
  destructured function field). Only method-shorthand trips the rule; `whenBroadcastIdle`
  (a `function` decl referenced by name) and value fields (`server`) do not.

- **Fix:** all in place. `tsc -p tsconfig.json` — 0 errors in `scripts/fleetd/**` (only the 14
  known board errors, another session's, remain). `eslint` on fleetd.ts / spawns.ts / http.ts /
  derive.ts — exit 0. Bundle rebuilt from `fleetd.ts`: banner `Source: *.ts`, shebang line 1,
  control-bytes 0, and the isolated entry diff is faithful hunk-for-hunk. Runtime: `node
  fleetd.ts` boots (type-strip) to `/health` 200; bundle-lane `takeover` + `smoke-project-
  isolation` 17/17; `spawn-repo` + `base-branch` 32/32 in BOTH the source lane (fleetd.mjs →
  spawns.ts) and the bundle lane (fleetd.ts).

### THE CUTOVER — `fleetd.mjs` DELETED, every launcher/test/demo repointed `.mjs`→`.ts`; a stale module specifier hiding in a shell heredoc was the one thing static tooling could not catch   [1 BUG (latent stale importer) / rest MECHANICAL]

- **What:** the deferral recorded above (`fleetd.mjs` intentionally kept as the tested source
  entry) is now resolved. `scripts/fleetd/fleetd.mjs` is DELETED; `fleetd.ts` is the sole source
  of truth. Repointed every runtime reference: `bin/fleetdeck.ts`/`.mjs` `SOURCE`, the
  SessionStart hook `scripts/fleet-sessionstart.ts`/`.mjs` fallback, `tests/helpers/daemon.mjs`
  `FLEETD_PATH`, and the demo/acceptance launchers. Rebuilt the three artifacts that embed daemon
  source (`fleetd.bundle.mjs`, `fleet-sessionstart.mjs`) or point at it (`bin/fleetdeck.mjs`).
  Production still runs `fleetd.bundle.mjs`; the `fleetd.ts` source path is the full-checkout
  fallback needing Node ≥22.18 type-strip.

- **THE BUG — a `.mjs`→`.ts` rename that static tooling structurally could not catch [BUG]:**
  `demo/lib/kill-verified-daemon.sh:42` did `await import(path.join(repoRoot,
  'scripts/fleetd/takeover.mjs'))`. `takeover.mjs` was renamed to `takeover.ts` back in
  `0e606973`, so this importer has been dangling ever since — the import throws
  `ERR_MODULE_NOT_FOUND`, the wrapping `verdict=$(node …) || verdict="none"` swallows the non-zero
  exit, and the helper falls through the `*)` case returning 0 WITHOUT signalling. Net effect: the
  identity-bound daemon stop silently degraded to a no-op — it neither killed a verified daemon
  (BUG-008 test 3) nor refused an unverified live pid (tests 1/2). Surfaced as 3 red
  `accept-reset` subtests the moment the cutover gate ran them.
  - **Why every static gate missed it:** the specifier is a string literal inside a bash heredoc
    (`node --input-type=module - <<'EOF' … EOF`) in a `.sh` file. It is not an `import` statement,
    not in any `.ts`/`.js`, and not in the module graph — so tsc, eslint, `verbatimModuleSyntax`,
    and import-resolution are all blind to it. **Lesson for the rest of the migration:** a
    `.mjs`→`.ts` rename is caught automatically ONLY for real import statements in typed source;
    module specifiers embedded in strings (shell heredocs, `node -e`, non-TS config) are invisible
    to the type checker and MUST be swept by hand + a spawning test.
  - **Fix:** repoint line 42 to `takeover.ts` (Node ≥22.18 type-strips the dynamic `.ts` import;
    the imported `pidRecord`/`verifyDaemonPid` already carry the BUG-156 widened `.ts` identity
    matcher) and correct the stale line-12 comment. `node --test tests/accept-reset.test.mjs` →
    4/4 green; a verified `fleetd.ts` daemon is SIGTERMed, a recycled plain-PID and a non-fleetd
    live pid are both refused (code 1, process left alive).

- **The rest is MECHANICAL, all faithful:** every other repoint is a bare path/string swap
  (`fleetd.mjs`→`fleetd.ts` as an import source, a launcher argv, or a bundle input). The one
  security-sensitive matcher — takeover's `/proc`-shape `fleetdScript` regex — was widened to
  accept `fleetd.ts` alongside `fleetd.bundle.mjs`/`fleetd.mjs` in an earlier commit and is
  embedded byte-identically in both the daemon bundle and the SessionStart hook (re-verified: the
  widened alternation is present in both, no `.mjs`-only matcher survives on the daemon graph).
