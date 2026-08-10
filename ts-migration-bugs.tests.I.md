<!-- STAGING for the test-conversion phase, GROUP I (tests/spawn.test.ts, spawn-repo.test.ts,
     succession.test.ts, takeover.test.ts): *.test.mjs -> *.test.ts under the full strict +
     strictTypeChecked + stylisticTypeChecked gate. My file alone (no shared append) — merge into
     ts-migration-bugs.md (newest-first) once the phase lands. Entries are migration CONSEQUENCES
     of strict typing; none are product bugs. Newest first. -->

### `assert.equal(typeof X, 'string')` does NOT narrow `X` — use `assert.ok(typeof X === 'string')`   [spawn.test / spawn-repo.test — MIGRATION CONSEQUENCE, latent tsc error caught in review]

- **What breaks.** The `.mjs` idiom `assert.equal(typeof x, 'string'); … x.length …` (spawn's
  `reason`, spawn-repo's `fail_detail`) relied on a runtime typeof check with no compile-time
  narrowing. `node:assert/strict`'s `equal` carries `asserts actual is T`, but `actual` is the
  FIRST argument — the `typeof x` EXPRESSION — so it narrows *that expression* to `'string'`, NOT
  `x` itself. Downstream `x.length` / `assert.match(x, …)` / `Buffer.byteLength(x)` / `x.split(…)`
  therefore still see `x`'s declared type: `string | undefined` (spawn `reason`, TS18048) or
  `string | null` (spawn-repo `fail_detail`, TS2345 ×2 + TS18047).
- **Fix (behaviour-identical).** Replace with `assert.ok(typeof x === 'string', msg)`. `assert.ok`
  carries `asserts value`, and `typeof x === 'string'` IS a typeof type-guard, so it narrows `x` to
  `string` for every read below. Pass/fail is identical to `assert.equal(typeof x, 'string')` —
  both throw iff `typeof x !== 'string'` — and the human message is preserved (promoted to the
  primary `AssertionError` message). This is exactly the F-cluster `assert.equal` vs `assert.ok`
  narrowing distinction, applied to a `typeof` operand.
- **Note.** These two sites (spawn.test.ts `fail-loud: missing cwd` `reason`; spawn-repo.test.ts
  failed-clone `fail_detail`) were the ONLY group-I tsc errors not attributable to the board-DOM
  blocker. They slipped past the initial conversion AND past eslint (which does not fail on raw TS
  type errors, only lint rules), and were caught by whole-program `tsc -p tsconfig.json --noEmit`
  during the orchestrator's independent review. A stale comment asserting that the `typeof`-assert
  narrowed was corrected in the same edit.

### `killDaemonAt`: a nullable `pid` used both as "the pid" AND as a "confirmed-dead" flag collides with `verifyDaemonPid(pid: number)`   [takeover.test — MIGRATION CONSEQUENCE, behaviour-preserving restructure]

- **What breaks.** The `.mjs` cleanup keeps `let pid = null`, calls `verifyDaemonPid(pid, home)`
  with a possibly-null pid, then after SIGTERM re-uses the SAME variable as a liveness flag
  (`process.kill(pid, 0)` in a loop; on ESRCH `pid = null; break;`), and finally gates the SIGKILL
  on `if (pid != null)`. Under strict types this is two separate type errors: `verifyDaemonPid`'s
  signature is `(pid: number, ...)` (rejects null), and `process.kill(pid, ...)` needs a non-null
  number.
- **Fix (equivalent, verified against the source).** (1) Guard the verifier calls as
  `pid == null || !verifyDaemonPid(pid, home)` — identical branch outcome, because
  `verifyDaemonPid` opens with `if (!Number.isInteger(pid) || pid <= 0) return false;`, so
  `verifyDaemonPid(null)` was already returning false without throwing (its own unit test proves it
  for `0`/`-1`); the `||` just never *passes* null now. (2) After the two guards, `const targetPid =
  pid` (TS narrows `pid` to `number` there) and signal `targetPid` everywhere. (3) Replace the
  overloaded nullable-pid "dead" flag with a dedicated `let alive = true` (`alive = false; break;`
  on ESRCH, `if (alive)` gates the SIGKILL). Same three observable behaviours: SIGTERM, ≤20×100ms
  liveness poll, conditional SIGKILL — the boolean just stops multiplexing "is it dead" onto the
  pid slot.

### `Semver` is not exported, but a converted test must hand `compareSemver` two NON-null operands   [takeover.test — MIGRATION CONSEQUENCE]

- **What breaks.** `compareSemver(parseSemver(a), parseSemver(b))` fed `Semver | null` into a
  `(a: Semver, b: Semver)` signature (4 direct call sites + a prerelease-chain loop). `parseSemver`
  returns `Semver | null`; `Semver` is deliberately file-local to takeover.ts (not exported).
- **Fix.** One module-local helper `function mustParse(input: string):
  NonNullable<ReturnType<typeof parseSemver>>` that `assert.ok`s the parse then returns it.
  `NonNullable<ReturnType<typeof parseSemver>>` recovers the exact `Semver` type WITHOUT forcing a
  source export, and is structurally the parameter type `compareSemver` wants. Behaviour-preserving:
  every input at these sites is a valid semver, so the assert never fires — it is a fail-loud guard
  on the null path the `.mjs` would have passed straight through (and `compareSemver` would have
  thrown on anyway).

### `child.std{out,err,in}` are nullable + the `'data'` chunk is untyped   [takeover.test — MIGRATION CONSEQUENCE]

- **What breaks.** `child.stdout.on('data', d => { stdout += d; })` — `stdout`/`stderr`/`stdin` are
  `Readable|Writable | null`; and the `'data'` listener param `d` is untyped (`any` via the
  `EventEmitter` overload), so `stdout += d` trips `no-unsafe-*` and `restrict-plus-operands`.
- **Fix.** Destructure + `assert.ok` the three streams once (fail-loud, matching the `.mjs`'s
  throw-on-null), then `childStdout.on('data', (d: Buffer) => { stdout += d.toString(); })`. Typing
  `d: Buffer` kills the unsafe-any; `d.toString()` is exactly the implicit `Buffer`→string coercion
  the `+=` did before. Side-effect order (attach stdout listener, attach stderr listener, write
  stdin, end stdin) is preserved — the up-front asserts have no side effects.

### `.unref?.()` on a `setTimeout` handle is a provably-unnecessary optional call   [takeover.test — MIGRATION CONSEQUENCE]

- The `.mjs` wrote `setTimeout(...).unref?.()` defensively. Under `@types/node`, `setTimeout`
  returns `NodeJS.Timeout`, which always has `unref()`, so `no-unnecessary-condition` flags the
  `?.`. Dropped to `.unref()`. Runtime-identical in node (the optional guard only ever mattered in a
  non-node runtime this test never runs in).

### `number | undefined` pids need capture-then-assert before arithmetic / `writeFileSync` / `process.kill` / `verifyDaemonPid`   [takeover.test — MIGRATION CONSEQUENCE]

- `ChildProcess.pid` (and the `/health` `pid`) is `number | undefined`. Sites doing `sleeper.pid +
  100000`, `writeFileSync(... { pid: sleeper.pid })`, `process.kill(stub.pid, 0)`, or
  `verifyDaemonPid(healthPid, ...)` all fail. Pattern: right after the `waitUntil(() => x.pid !=
  null)` / `waitForHealth` that already guarantees it, capture `const p = x.pid; assert.ok(p !==
  undefined, ...)` and use `p`. Fail-loud guard that never fires (the wait established liveness).
  Used `!== undefined` (not truthiness) so a pid of 0 would not be silently excluded, though a
  spawned child never has pid 0.

### `waitForHealth()`'s index-signature return forces bracket access for `version`/`managed`, but a locally-cast `/health` does not   [takeover.test — MIGRATION CONSEQUENCE]

- `waitForHealth` returns `{ pid?: number; [k: string]: unknown }`, so `noPropertyAccessFromIndex-
  Signature` requires `health['version']` / `health['managed']` (both `unknown`) while `health.pid`
  stays dot-accessible. By contrast, the `(await getJson('/health')).json` reads are cast to a
  local `interface HealthView { pid?: number; version?: string; managed?: boolean }` whose EXPLICIT
  props keep dot access. So the same field is `health['version']` (bracket) off `waitForHealth` but
  `before.version` (dot) off a `HealthView` cast — the access style tracks whether the property is
  declared or index-signature, not the field name.

### Per-site `/state` cast shape chosen to preserve each `.mjs` access verbatim (defensive `?.` vs direct `.find`)   [takeover.test — MIGRATION CONSEQUENCE]

- The `.mjs` is internally inconsistent: one site is `state.sessions?.some(...)` (defensive optional
  chain), others are `state.sessions.find(...)` (assumes present) and `(state.ticker || []).map(..)`.
  Casting all to the authoritative `contracts/state.ts` `StateResponse` (where `sessions` is
  required) would make the `?.` provably unnecessary → `no-unnecessary-condition` → forced to DROP
  the guard = a behaviour change. Kept faithful by casting PER SITE to a local shape matching that
  site's access: `{ sessions?: SessionView[] }` where the source used `?.`, `{ sessions:
  SessionView[]; ticker?: TickerView[] }` where it used direct `.find` / `.ticker`. Also `(ticker
  || [])` → `(ticker ?? [])` (prefer-nullish-coalescing; identical for `T[] | undefined`, since an
  array is always truthy and non-nullish).

### `Reflect.deleteProperty` for `childEnv.TMUX` (recurs from the board cluster)   [takeover.test — MIGRATION CONSEQUENCE]

- `delete childEnv.TMUX` is a double bind: `NodeJS.ProcessEnv`'s index signature makes dot access a
  `noPropertyAccessFromIndexSignature` error, and rewriting to `delete childEnv['TMUX']` trips
  `no-dynamic-delete`. `Reflect.deleteProperty(childEnv, 'TMUX')` satisfies both and is the same
  static-key delete. (Same resolution the board-cluster notes reached; recorded here because it
  recurs on ProcessEnv specifically.)

### `chain[i]` / `chain[i-1]` under `noUncheckedIndexedAccess` in the prerelease-precedence loop   [takeover.test — MIGRATION CONSEQUENCE]

- Variable indexing a `string[]` yields `string | undefined`, which fails three ways at once here:
  `mustParse` wants `string`, and the `${chain[i]}` message templates ban `undefined`
  (`restrict-template-expressions`). Fix: `const curr = chain[i]; const prev = chain[i - 1];
  assert.ok(curr !== undefined && prev !== undefined);` once at the top of the body, then use the
  narrowed locals. Loop bounds guarantee both are in range, so the guard never fires.

### succession.test — capture-then-`assert.ok` narrowing, typed `prepare<Row>()`, and two behaviour-preserving restructures   [succession.test — MIGRATION CONSEQUENCE]

- **Property-narrowing reset across calls.** Every `cardOf(state, sid)`, `heir.spawn`,
  `mail_meta[newSid]`, `visible[0]`, and DB `.get()` result is `T | undefined`; captured into a
  `const` and `assert.ok`'d before use (property narrowing resets after any intervening call, but
  `const` locals do not). `assert.match(note, ...)` additionally needs `note` narrowed to `string`
  first (it is not an `asserts` and rejects `string | null`).
- **Typed DB rows.** `prepare<RetiredRow>(sql)` etc. type each `.get()`/`.all()` without a source
  import; `ReturnType<typeof openDb>` types the `withDb` callback's `db` param.
- **heirA/heirB mutation restructure.** The `.mjs` did `const heir = { sid: randomUUID() }; heir.
  callsign = (await postHook(...)).json.callsign;` (mutate-after-init). Rebuilt as
  `const heirSid = randomUUID(); const res = await postHook(...); const heir = { sid: heirSid,
  callsign: (res.json as HookResult).callsign };` — same final object, no exactOptionalProperty /
  reassignment friction.
- **Sort table typed as a tuple.** `const expectedClaims: [string, string, string][] = [...]`
  (not inferred `string[][]`), so `.sort((x, y) => x[0].localeCompare(y[0]))` sees `x[0]: string`.
- **Settle sleep.** `await new Promise<void>((resolve) => { setTimeout(resolve, scaleMs(900)); });`
  (same pattern used for the poll sleeps in takeover.test).

### Cross-file FLAGS carried out of group I (cannot fix here — must-not-touch siblings/helpers)

- `tests/wait-scaling.test.ts:29` hardcodes `spawn-repo.test.mjs` and greps its source by line;
  `tests/spec-record-cleanup.test.ts:27` lists `spawn.test.mjs` (+ `spawn-unsupervised.test.mjs`);
  and a `tests/spawn-repo-scratch-cleanup.test.ts` sibling — all run targets via
  `spawnSync/execFileP(process.execPath, ['--test', <path>])` with NO explicit type-strip flag.
  After group I renames spawn/spawn-repo to `.ts`, the orchestrator MUST (a) repoint those hardcoded
  `.mjs` paths, and (b) verify the child `node --test` can execute a `.ts` target (node type-strip
  is on by default on current node, but confirm on the CI matrix's oldest lane). `succession` and
  `takeover` have NO such external runtime references (grep found only self-headers, now deleted),
  so deleting their `.mjs` breaks nothing.
- `tests/helpers/daemon.ts:149` has a STALE COMMENT referencing `takeover.test.mjs`. Cosmetic;
  left untouched (must-not-touch helper) — orchestrator may refresh it to `.ts`.
