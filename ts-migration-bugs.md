# TypeScript migration — bugs & issues surfaced by strict typing

Living log for the full `.mjs → .ts` migration of Fleet Deck (`fd/typescript-migration`).
Every latent bug, unsound pattern, or wire-contract mismatch that **strict TypeScript
exposed** in the existing JavaScript goes here — one entry each, newest first.

Config: root `tsconfig.json` is *maximally* strict (`strict` + `noUncheckedIndexedAccess`
+ `exactOptionalPropertyTypes` + `noPropertyAccessFromIndexSignature` + `noUnused*`).
So "the type checker complained" spans three very different things; each entry is tagged:

- **BUG** — a real latent defect (wrong runtime behavior possible). Fixed in place; test added where feasible.
- **UNSOUND** — code that worked by luck / convention the compiler couldn't see; tightened or narrowed.
- **NOISE** — pure ergonomics of maximal-strict on dynamic JS (index access, optional exactness); annotated, no behavior change.

Format:
```
### <module>.ts — <one-line title>   [BUG|UNSOUND|NOISE]
- **What:** the symptom the checker flagged (file:line)
- **Why it's real / why it's noise:** …
- **Fix:** what changed, and whether runtime behavior moved.
```

---

## Migration ordering constraints (sequencing gotchas the plan didn't foresee — not strict-typing bugs)

### env-scrub / run-nonce / config / takeover — hook-coupled leaves can't convert before the hooks are bundled
- **What:** `docs/v1/ts-migration.md` (Phase 2) names `env-scrub` as the **first** conversion —
  "47 lines, a security boundary, the perfect proof-of-recipe." But these four daemon leaves are
  imported *directly* by the **unbundled** plugin hooks (`scripts/fleet-sessionstart.mjs`,
  `scripts/fleet-hook.mjs`, `scripts/fleet-watch.mjs`), which `hooks/hooks.json` runs as raw `.mjs`
  via `node "${CLAUDE_PLUGIN_ROOT}/scripts/fleet-*.mjs"` on the *user's* Node — package `engines`
  floor `^22.13.0`.
- **Why it's real:** Node 22.13–22.17 cannot strip TS types. A raw `.mjs` hook importing
  `./fleetd/env-scrub.ts` would **throw on load** on those versions instead of failing open — a hard
  regression for anyone on the engine floor. The plan's "coexistence is free" proof only covers the
  daemon's two run-paths (the esbuild bundle + Node ≥22.18 source stripping); it did not account for
  the plugin hooks, which are a *third*, engine-floor-pinned, raw-source run-path.
- **Coupled set (complete, measured):** `env-scrub`, `run-nonce`, `config`, `takeover` — all leaves
  (0 local imports). `bin/fleetdeck.mjs` / `bin/tmux-version.mjs` import nothing from `fleetd/` (they
  spawn the committed bundle as a child), so `bin/` does **not** extend the set.
- **Resolution:** convert these four **together with** bundling the plugin hooks (task #10). Once the
  hooks ship as committed plain-JS artifacts that inline their imports, no raw-`.mjs` consumer of
  daemon source remains and the four convert cleanly. Phase 2's recipe-proof therefore starts on the
  smallest **daemon-only** leaf (`tickets`, 49 loc, fan-in 5) instead of `env-scrub`.

---

<!-- entries appended below as modules convert -->

### helpers.ts — type-guards to carry narrowing, one dead-defense drop, one load-bearing coercion kept   [NOISE]
- **What:** the pure, closure-free leaf shared across the core (`spawnRowRevivable`,
  `sessionAdoptableNow`, `claudeEnvArgvPrefix`, `createKeyedMutex`, `mapLimit`,
  `chmodWritableWhereOwned`/`blockedPaths`, `parseCommand`, …). `tsc` + type-aware `eslint` clean,
  **zero runtime move**. Eight strict-typing notes; the coercion audit (5) is the one that touched
  behavior-adjacent source, so it's the interesting one.
- **Why it's real / why it's noise:** NOISE on all eight — nothing at runtime moved:
  (1) **Optional-chain truthiness doesn't narrow the object.** `spawnRowRevivable` reads
  `const runCwd = row?.worktree_path ?? row?.cwd` then `!!runCwd && ['pane-dead',…].includes(row.status)`.
  `runCwd` being truthy proves `row` is non-null *at runtime*, but the compiler can't infer that back
  through the `?.`, so `row.status`/`row.session_id` error as possibly-undefined. Added an explicit
  `&& row != null` into the boolean chain (short-circuits identically; runtime-identical since a truthy
  `runCwd` already guaranteed it).
  (2) **`cwdIsDirectory(p): p is string` made a type-guard so its one caller narrows.** `sessionAdoptableNow`
  does `const cwd = session.cwd` (`string | null | undefined`) then `cwdIsDirectory(cwd) && …existsSync(claudeTranscriptPath(cwd, …))`.
  Typing the probe `boolean` would leave `cwd` un-narrowed at the `claudeTranscriptPath(cwd, …)` call; the
  `p is string` predicate (body unchanged — `if (!p) return false;` then `statSync`) carries the narrowing
  through the `&&`. Same for the truthiness already inside it.
  (3) **Two dead `String()` defenses dropped.** `shellQuote(s)` had `String(s).replace(…)` and
  `colFromAgentState(raw,…)` had `String(raw ?? '')`. Every caller passes a string (or `raw` is already
  typed `string | null | undefined`), so the coercion was defending against a shape the types now forbid —
  dropped to `s.replace(…)` / `(raw ?? '').toLowerCase()`. No runtime path changed: a real string is its
  own `String()`.
  (4) **One `String()` coercion KEPT, with a justified `no-base-to-string` disable.** `parseCommand(text)`
  takes `text: unknown` (it's `core.command(ev.text)` — a raw HTTP-body field), and the very first line is
  `String(text ?? '').trim()`. Unlike (3) this is load-bearing: the degenerate `object → "[object Object]"`
  path is intentional garbage-in handling for an untrusted wire value. `strictTypeChecked`'s
  `no-base-to-string` flags it, so it carries an inline `// eslint-disable-next-line …no-base-to-string`
  with a **follow-up to delete it once `http.ts` validates the body to `string`** — at which point `text`
  narrows to `string` and the coercion becomes dead like (3). Logged so the follow-up isn't lost.
  (5) **`ParsedCommand` is now a discriminated union; regex-capture reads get `?? ''`.** Writing the return
  type as a union on `cmd` (with separate success/`error` arms for `ticket`/`name`) means every branch's
  object shape is checked. Under `noUncheckedIndexedAccess`, `m[1]`/`m[2]` off a `RegExpExecArray` are
  `string | undefined` even though the groups are non-optional in these patterns and always match when the
  regex does — defaulted with `?? ''` (unreachable at runtime; the group is always present when `exec`
  returns non-null). `colFromAgentState`'s return is likewise pinned to the four-column literal union.
  (6) **`mapLimit` needs an index guard for `items[i]`.** `out[i] = await fn(items[i])` — `items[i]` is
  `T | undefined` under `noUncheckedIndexedAccess`. Captured `const item = items[i]; if (item === undefined) continue;`
  before the call (unreachable for the dense arrays this runs over — a hole would be skipped, not mapped —
  same honest-but-unreachable guard as `transcript.ts`).
  (7) **`process.getuid` property-narrowed by capture; `blockedPaths` cache read get-first.** `getuid` is
  optional on `process` (absent on Windows), so `typeof process.getuid === 'function'` doesn't narrow the
  later call — captured `const getuid = process.getuid` first, then `typeof getuid === 'function' ? getuid() : null`
  (the paste.ts precedent). And `Map.get` returns `V | undefined`, so the owner-name cache rewrote
  `if (owners.has(uid)) return owners.get(uid)` to a get-first `const cached = owners.get(st.uid); if (cached !== undefined) return cached;`
  (behavior-identical — a cached name is never `undefined`).
  (8) **`no-useless-assignment` on `let entries = []` before a `try`/`catch`-that-returns.** Both walkers
  had `let entries: fs.Dirent[] = []; try { entries = readdirSync(…) } catch { return }`. The ESLint core
  rule flags the `[]` initializer as never-read (the only way past the `try`/`catch` is the successful
  assignment, since the catch `return`s). Dropped to `let entries: fs.Dirent[];` — TS definite-assignment
  analysis accepts it precisely *because* the catch always returns. Runtime-identical.
- **Fix:** type-guards + a `row != null` chain link + `?? ''` capture-guards carry narrowing the compiler
  can't infer through `?.`/index access; two dead coercions dropped; one untrusted-wire coercion kept
  behind a disable with a delete-when-`http.ts`-validates follow-up. No test changed; 43/43 direct-importer
  + 92/92 daemon-consumer green vs source, and the same green vs the regenerated bundle.

### exec.ts — a `let`→`const` reorder, an overload-pinning `encoding`, and the `String(err)` catch-22 again   [NOISE]
- **What:** the git/subprocess helper — `execFileP` (timeout + SIGTERM→SIGKILL escalation) plus the
  four stderr scrubbers (`distillGitStderr`, `redactGitText`, `gitStderrDetail`, `baseBranch`). `tsc`
  + type-aware `eslint` clean, **zero runtime move**. Five strict-typing notes; (3) is the one that
  changed a source line for a type reason, so it's the interesting one.
- **Why it's real / why it's noise:** NOISE on all five — no behavior at risk:
  (1) **`ExecResult.code` had to admit `null`.** The result union's failure arm is
  `{ ok: false; code?: string | number | null | undefined; err: string }`. `code` comes straight off
  Node's `ExecFileException.code`, which is `string | number | null | undefined` — so the annotation
  has to carry `null` even though no consumer branches on it being null. Consumers read `code` as
  **both** a string (`'ETIMEDOUT'`) and a number (`worktrees.mjs:216` does `merged.code !== 1`), which
  is why the union keeps both — the wire contract this helper has always emitted, now written down.
  (2) **`execFile`'s callback params were `string | Buffer` until I pinned the overload.** Without an
  explicit `encoding`, `execFile(cmd, args, opts, cb)` resolves to the `Buffer` overload, so `stdout`
  /`stderr` in the callback are `string | Buffer` and every `.trim()`/string use fails. Adding
  `encoding: 'utf8'` to the options object selects the string-callback overload → `stdout`/`stderr`
  are `string`. **Runtime-identical**: `'utf8'` is already `execFile`'s default encoding, so this
  pins the type of a value that was always a UTF-8 string at runtime; it adds nothing to the call.
  (3) **A `let child` prefer-const false-positive, fixed by reordering rather than suppressing.** The
  timeout handler (`deadline`) must reference `child` (to `child.kill(...)`), and the child's exit
  callback must reference `deadline` (to `clearTimeout(deadline)`) — a mutual reference. The JS
  declared `let child;` first, assigned it after building `deadline`, so the linter saw a
  single-assignment `let` and demanded `const`. Rather than a `// eslint-disable prefer-const`, I
  **reordered** to `const child = execFile(…, cb)` *before* `const deadline = setTimeout(…)`: `cb`
  forward-references `deadline`, which is legal because it's a closure invoked off-tick (never during
  `execFile`'s synchronous return) **and `no-use-before-define` is deliberately off** in the config.
  This let me drop the forward-declared `let`, the `child &&` null-guards (a `const` from `execFile`
  is non-nullable), and the `target` capture (a `const` doesn't widen inside nested closures). Also
  `.unref?.()` → `.unref()` (no-unnecessary-condition: `unref` always exists on `NodeJS.Timeout`).
  (4) **`String(err)` in the error-callback fallback hit the same no-base-to-string ⇄
  no-unnecessary-type-assertion catch-22 payload-capture logged.** The fallback message is
  `(stderr || err.message || <x>).trim()`. `err` is `ExecFileException`; `no-base-to-string` rejects
  `String(err)` (it doesn't see `ExecFileException` as `Error`-derived → "may stringify to
  `[object Object]`"), but writing `String(err as Error)` gets auto-stripped by the formatter's
  `eslint --fix` as `no-unnecessary-type-assertion` (`ExecFileException` **is** assignable to `Error`),
  reverting to `String(err)` and re-firing the first rule. Broke the loop with `err.name` (a plain
  `string`) — **behavior-identical**, because this branch is reached only when both `stderr` and
  `err.message` are empty, and `Error.prototype.toString()` returns exactly `name` when `message === ''`.
  (5) **`noUncheckedIndexedAccess` on the scrubbers' split/regex reads.** `str.split('\n\n', 1)[0]`
  and a successful `RegExpExecArray[1]` are both `T | undefined`; fixed with `?? ''` on the split and a
  captured-and-guarded `const name = m?.[1]; if (name?.trim()) …` in `baseBranch`. Same string
  extracted, `null` returned on the (unreachable-but-honest) miss.
- **Tooling hazard (not a typing bug — logged so the next converter doesn't lose an hour):** the
  Write/Edit tools turn the escape sequence `\u2028\u2029` inside the C0/C1-stripping regex in
  `gitStderrDetail` into **literal** U+2028/U+2029 bytes on write (confirmed via `cat -A` →
  `M-bM-^@M-(M-bM-^@M-)`). After any Write/Edit that touches that line, restore it with
  `perl -CSD -i -pe 's/\x{2028}\x{2029}/\\u2028\\u2029/g' scripts/fleetd/exec.ts`. (The lone literal
  U+FFFD `�` in `.replace(/^�+/, '')` is intentionally literal in the original and stays as-is.) The
  `no-control-regex` disable must sit **immediately** above the regex line, or `eslint --fix` strips
  it as unused.
- **Fix:** annotations + the reorder only; runtime behavior identical (37/37 `exec-timeout` +
  `base-branch` + `git-stderr-detail` green vs source **and** vs the regenerated bundle; whole-project
  `tsc --noEmit` clean, `eslint` clean).

### payload-capture.ts — a recursive `Json` projection type, and a dead defensive `String()` the honest contract exposed   [NOISE]
- **What:** the 503-line redaction leaf — secret-*key* / secret-*value* / credentialed-*URL* scrubbing
  (`isSecretKey`, `SECRET_VALUE_RES`, `maskCompactTokens`, the five `scrubUrlCredentials` layers) plus
  the byte-budgeted, opt-in hook-payload capture (`boundedPayload`/`createPayloadCapture`). `tsc` +
  type-aware `eslint` clean, **zero runtime move**. Three strict-typing notes worth recording.
- **Why it's real / why it's noise:** NOISE — but note (2) is a genuine contract narrowing that the
  checker + type-aware lint *forced into the open*, so it's the interesting one:
  (1) **The bounded projection walker needs a recursive `Json` return type + two runtime-guard casts.**
  `boundedPayload`/`visit` walk an `unknown` payload and emit a JSON-serializable value, so the return
  type is `type Json = string | number | boolean | null | Json[] | { [k: string]: Json }`. Inside
  `visit`, `noUncheckedIndexedAccess` + the `unknown` boundary can't carry element/value types through
  the `Array.isArray` / `typeof === 'object'` guards, so the two branches take `current as unknown[]`
  and `current as Record<string, unknown>` respectively. Pure ergonomics of maximal-strict on a
  deliberately-dynamic walker.
  (2) **`String(text ?? '')` in the two exported scrubbers was dead defense — a two-step lint cascade
  proved it.** `redactDiagnosticText` / `scrubUrlCredentials` took an untyped param and coerced with
  `String(text ?? '')`. With an `unknown` param, `text ?? ''` has type `{} | string`, and
  `@typescript-eslint/no-base-to-string` flags it ("an object stringifies to `[object Object]`");
  narrowing to the honest contract `string | null | undefined` makes `text ?? ''` a plain `string`, at
  which point `@typescript-eslint/no-unnecessary-type-conversion` flags the `String()` as a no-op.
  Both diagnostics are correct: for the real contract the `String()` was JS-era belt-and-suspenders.
  Dropped it → `redactValue(text ?? '')` and `(text ?? '').replace(...)`.
  - **VERIFIED behavior-identical across the live call domain, not just the happy path.** The *only*
    inputs whose behavior `String()` changed are non-string, non-nullish values — it coerced them;
    without it the downstream `.replace` would throw. Every real caller passes a `string`: the git exec
    helper builds `.err` via `String(...)` in all three branches (`exec.mjs:56/90/94`), `snapshot.mjs`
    guards `origin_url == null` before calling, and the `spawns`/`worktrees`/`exec` sites pass
    `.join('\n')` or already-scrubbed stderr strings. Nullish is still absorbed by `?? ''`. So the
    coercion was dead for the entire reachable domain; there is no throw regression. Going forward a TS
    caller that passes a non-string is a **compile error** — the migration surfacing an unclean contract
    instead of silently coercing it.
  (3) **`NOOP = () => {}` tripped `no-empty-function`.** Added the rule's sanctioned escape — a comment
    body (`/* capture disabled: swallow every call */`). This is the no-op returned by
    `createPayloadCapture` when `FLEETDECK_CAPTURE_PAYLOADS` isn't `on`.
- **Minor, folded in (no separate note):** `process.env['FLEETDECK_CAPTURE_PAYLOADS']` uses bracket
  access (`noPropertyAccessFromIndexSignature`); `PayloadCaptureOptions.secrets: readonly unknown[]`
  keeps the pre-existing defensive `secrets.filter((s): s is string => typeof s === 'string' && s.length > 0)`
  live rather than dead, since the element type is genuinely `unknown` at the boundary.

### statements.ts — the row-shape vocabulary finally minted; a circular `ReturnType` the WeakMap forced   [NOISE]
- **What:** the ~90-statement prepared-query map + the cached-UPDATE `updateSession` writer.
  This is the module `ledger.ts` and `plans.ts` each left a comment pointing at ("replaced by
  the real statements-layer export when that converts") — so the conversion's real work was
  minting the store's **row types** (`SessionRow`, `MailRow`, `SpawnRow`, `RepoRow`, `PlanRow`,
  `ConflictRow`, `TouchRow` + local projections) and wiring each `SELECT` to one via
  `db.prepare<R>(sql)`. Two things genuinely resisted a naive typing; neither is a runtime defect.
- **Why it's real / why it's noise:** NOISE — `tsc` + type-aware lint clean, zero runtime move.
  But two structural notes worth recording:
  (1) **The memoizing WeakMap makes `createStatements`'s return type circular.** The cache is
  `WeakMap<SqliteHandle, Statements>` and `Statements` is "whatever `createStatements` returns" —
  but `createStatements`'s body *reads* that WeakMap, so inferring its return type from its own
  body is circular and `tsc` refuses (`… implicitly has return type 'any' because it does not
  have a return type annotation and is referenced directly or indirectly in one of its return
  expressions`). Because `noPropertyAccessFromIndexSignature` etc. are off for `.mjs`, the still-JS
  `derive.mjs` consumer is unaffected either way — but the fix keeps the TS honest for the future
  `derive.ts`.
  (2) **`SqliteStatement<SqlRow>` is not assignable to `SqliteStatement<SpecificRow>`** (a bare
  `SqlRow` index signature has none of the named props), so a single `const q: QMap = { … }`
  annotation with plain `db.prepare('sql')` entries would *fail* to type — the row shape has to be
  driven **per statement** by `db.prepare<Row>('sql')`, letting inference build the map's type from
  the individual generics rather than a hand-written map interface.
- **Fix:** (1) extracted a `build(db)` helper that compiles `q`/`FIELDS`/`updateSession` with **no**
  reference to the cache, derived `export type Statements = ReturnType<typeof build>` from it, and
  left `createStatements` as a thin memoizing wrapper — the return type now flows from `build`,
  which is self-contained, so no circularity. (2) annotated each `SELECT` with `db.prepare<R>(…)`
  (`INSERT`/`UPDATE`/`DELETE` stay the default `SqlRow` — they're only ever `.run()`); every domain
  comment (BUG-034 lease, BUG-107 aliases, BUG-149/150/128, H-R5/R6, /clear succession, plan
  library) preserved verbatim. Row nullability is **write-path-faithful**, matching the ledger.ts /
  plans.ts stand-ins it supersedes: PKs, AUTOINCREMENT ids, always-stamped timestamps, and
  `DEFAULT`-carrying columns are non-null; a column legitimately absent on some row (`repo_id` on a
  hook-born session, `executed_via` on an unexecuted plan, `delivered_at` on undelivered mail) is
  `| null`. `updateSession(sid: string, upd: Record<string, SqlValue>): void`,
  `updateStmts = new Map<string, SqliteStatement>()`, `FIELD_SET = new Set<string>(FIELDS)`. The
  exported row types are **purely additive** — nothing internal reads a row (the module only
  compiles statements + does index access `upd[k]`), so the `<R>` only shapes what future TS
  consumers infer, never what this file checks. **No runtime behavior moved** — tsc + eslint clean;
  51/51 (settings-transaction + derive-audit-reliability + daemon-maintenance) and 56/56 (repos +
  worktrees) green vs source, and 51/51 green vs the regenerated bundle.
- **Follow-up (not this commit):** `ledger.ts` / `plans.ts` still carry their *provisional*
  stand-in row interfaces. Now that the real exports exist, a later change can import
  `TouchRow`/`SessionRow`/`PlanRow`/`PlanStatus` from here and delete the stand-ins — kept separate
  to keep this commit a clean mechanical conversion.

### db.ts — the store's schema layer; three strict knobs, no defect   [NOISE]
- **What:** `openDb()` (DDL + `migrate()` + the 0600 confidentiality chmod) built on
  `sqlite.ts`'s `openDatabase()`. A clean conversion — nothing latent surfaced — but three
  maximal-strict knobs each demanded a deliberate shape rather than a cast:
  (1) `noPropertyAccessFromIndexSignature` on the three `PRAGMA table_info(<t>)` reads:
  `.all().map(r => r.name)` fails because a default `SqlRow` is a pure index signature, so
  `r.name` is illegal dot access. (2) `useUnknownInCatchVariables` on both confidentiality
  `catch (err)` blocks: the old `err?.code`/`err?.message` don't compile on `unknown`.
  (3) the `openDb(file, fsImpl = { chmodSync, statSync })` seam — what *type* is `fsImpl`?
- **Why it's noise (not a bug):** every one is an ergonomics choice with zero runtime move.
  (1) Asserted the real row shape at each query — `db.prepare<{ name: string }>('PRAGMA
  table_info(sessions)')` (via a local `PragmaColumnInfo`) — so `.map(r => r.name)` stays on
  dot access and yields `string[]`. The SQL guarantees the `name` column; the assertion
  belongs with the query (exactly the contract `SqliteStatement<R>` was minted for). The
  full pragma row is `cid/name/type/notnull/dflt_value/pk`; nothing here reads the rest.
  (2) Added two tiny narrowing helpers — `errCode(err): string | undefined` (drives the
  ENOENT skip on a lazily-absent WAL/SHM sidecar) and `describeErr(err): string` (reproduces
  `err?.code || err?.message || 'unknown error'` for the refusal message). Faithful to the JS,
  now type-safe: a caught value is `unknown` and Node's errno `code` isn't on `Error`.
  (3) Typed `fsImpl` as a **minimal structural** `DbFsImpl` (`chmodSync(path, mode)`,
  `statSync(path): { mode }`) — *not* `typeof import('node:fs')`. The real fs functions
  satisfy it structurally, AND the tests' chmod-refusal doubles (whose `statSync` returns only
  `{ mode }`) will still satisfy it once tests convert (#11); `Pick<typeof fs, …>` would reject
  those doubles because a mock's `{ mode }` isn't a full `Stats`.
- **Fix:** annotations + two helper fns only; runtime behavior identical (bundle grew 618.2→618.6kb
  from the two helpers surviving as real functions). Verified: tsc + eslint clean; db-perms 4/4 and
  daemon-maintenance 17/17 green vs source **and** vs the regenerated bundle.
- **Aside (not a migration bug — pre-existing, logged so it isn't re-misdiagnosed):**
  `agents-ingest.test.mjs` flakes hard on this WSL box (2–6 of 10 subtests fail with
  `daemon at 127.0.0.1:<port> never became healthy: timeout` from `tests/helpers/daemon.mjs`,
  10s `healthTimeoutMs`, ~10 daemons spawned in quick succession). Proven **not** the migration's
  doing: with my db.ts change stashed and the committed `db.mjs` restored, the same file failed
  **6/10** on the same quiet machine — worse than my 2/10. Root cause is the daemon-boot health
  budget under rapid multi-spawn on WSL, present on `main`. The deterministic db.ts tests
  (db-perms, daemon-maintenance) pass every run; use those as the db.ts signal, not agents-ingest.

### sqlite.ts — the store's foundational types + a monkey-patch the linter can't see is safe   [NOISE]
- **What:** the runtime-agnostic driver seam. This is where the whole store's type
  vocabulary is minted — `SqlValue` (a cell), `SqlRow = Record<string, SqlValue>`,
  `SqlRunResult` (`{ changes, lastInsertRowid }` both `number | bigint`), the
  caller-generic `SqliteStatement<R = SqlRow>`, and the wrapped `SqliteHandle` whose
  `prepare<R>()` lets downstream layers assert a row shape at the query, not the driver.
  Three lint findings surfaced, all ergonomic:
  (1) `@typescript-eslint/unbound-method` on `const emitWarning = process.emitWarning`.
  (2) `no-unnecessary-type-conversion` on `String(warning)` in the `warning instanceof Error ? … : String(warning)`
  fallback. (3) `prefer-nullish-coalescing` on the `.get()` normalizer
  `row == null ? undefined : row`.
- **Why it's noise (not a bug):**
  (1) The capture is the *point* — node:sqlite emits its lone `ExperimentalWarning` at
  import, so the seam swaps `process.emitWarning` for a filter and must restore the
  **exact original method object** in `finally`. A bound copy (`.bind(process)`, the rule's
  usual escape) would silently change the global's identity and defeat any later re-patch
  or identity check; the filter always forwards with the receiver preserved (`.call`), so
  the "unbound `this`" the rule fears cannot happen. Suppressed with a `-- reason`.
  (2) In the `else` branch `warning` is already narrowed to `string`; the `String()` the JS
  carried defensively is provably a no-op. (3) `row == null ? undefined : row` **is**
  `row ?? undefined` — both pin bun's `null` miss to Node's `undefined`, identically.
- **Fix:** justified one-line `eslint-disable` on the capture; `String(warning)` → `warning`;
  ternary → `row ?? undefined`. No runtime behavior moved.
- **Bonus (seam validated, not a finding):** I wrote a structural `DriverHandle`/`DriverStatement`
  (rows read back as `unknown`, not either driver's row type) and cast both driver
  constructions to it. `eslint --fix` then **stripped both `as unknown as DriverHandle`
  casts as unnecessary** — i.e. Node's `DatabaseSync` *and* bun's `Database` already satisfy
  `DriverHandle` structurally, so the seam type is honest with zero coercion. It also
  rewrote my `process.versions['bun']` → `.bun`: `bun-types` (pulled in by tsconfig
  `types: ["node", "bun"]`) augments `NodeJS.ProcessVersions` with a real `bun: string`
  key, so dot access is correct and `noPropertyAccessFromIndexSignature` is satisfied
  without the bracket form the other leaves needed for `process.env[…]`.

### paste.ts — `??` would be WRONG here + `unknown`-catch errno + `process.env` index   [NOISE]
- **What:** four surfaced on the pasted-image ingest leaf. (1) `prefer-nullish-coalescing`
  flagged ``process.env.FLEETDECK_HOME || path.join(…)`` and — after a first rewrite to a
  ``configured ? configured : …`` ternary — flagged *that* too (the rule's
  `ignoreTernaryTests: false` default rewrites `a ? a : b` → `a ?? b`). (2) `errnoCode`:
  the `mkdir` catch does `err?.code !== 'EEXIST'`, but a strict `catch` binding is
  `unknown`, so `.code` doesn't type-check. (3) `tsc` TS4111
  (`noPropertyAccessFromIndexSignature`): `process.env.FLEETDECK_HOME` must be
  `process.env['FLEETDECK_HOME']`. (4) `process.getuid` is `(() => number) | undefined`
  off-Windows, so `st.uid !== process.getuid()` needs the accessor narrowed first.
- **Why it's real / why it's noise:** NOISE on all four — no runtime behavior moved — but
  **(1) is the instructive inverse of the repo-identity entry.** There, `||`→`??` was safe
  because the left operand could never be `''`. Here it is the OPPOSITE: an empty
  `FLEETDECK_HOME` (env var set to `""`) MUST fall through to the default, or the paste dir
  resolves to `/pastes`. `''` is falsy → `||` (and only `||`) selects the default; `??`
  would keep `""`. So the autofix the rule offers is a **latent bug**, and the correct move
  is NOT to take it. The others are the usual boundary/env ergonomics of maximal-strict.
- **Fix:** (1) wrote the fallthrough as an explicit compound-boolean ternary the rule cannot
  pattern-match as nullish-equivalent — ``configured != null && configured !== '' ?
  configured : path.join(…)`` — behavior-identical to the original `||` (undefined→default,
  `''`→default, non-empty→configured), with **no** `.trim()` added (whitespace handling
  unchanged; the daemon already feeds an already-trimmed HOME via config.mjs). Commented in
  place so a future reader doesn't "simplify" it to `??`. (2) a small local
  `errnoCode(e: unknown): string | undefined` that narrows via `e instanceof Error` and
  reads `(e as NodeJS.ErrnoException).code` — no `any`; kept local until a shared fs-errno
  need is proven across leaves. (3) bracket access `process.env['FLEETDECK_HOME']`.
  (4) captured `const getuid = process.getuid;` before the `typeof getuid === 'function'`
  guard so TS narrows the call. Also gave the module honest types (`ImageExt`, the
  `{status, body}` `PasteResult` discriminated union, `PasteEntry`) and switched the GIF
  sniff's `.match(re)` truthiness to `re.test(s)` (boolean, identical result). **No runtime
  behavior moved** — 19/19 paste-image tests green vs source; bundle re-parses (no
  daemon-spawn paste test exists — the suite exercises pasteImage by direct import).

### transcript.ts — `.truncated` on a generator + JSONL-boundary `unknown`   [NOISE]
- **What:** three things surfaced on the transcript reader, all from typing a module whose
  whole job is parsing an *untrusted*, possibly-mid-append JSONL file. (1) `tailLines`
  returns a generator with an extra `.truncated` boolean glued on
  (``it.truncated = start > 0``) — a plain assignment to a property TS's `Generator<T>`
  type doesn't declare, so it doesn't type-check. (2) Every field off `JSON.parse(line)`
  (`entry.type`, `entry.message.content`, `.model`, and each content block's `type`/`text`)
  is untyped; the original JS reached straight through with optional chaining
  (`entry?.message?.content`, `b.type === 'text'`). (3) `lines[i]` inside the reverse walk
  is `string | undefined` under `noUncheckedIndexedAccess`, even though `i` is a valid index.
- **Why it's real / why it's noise:** NOISE on all three — `tsc` and the type-aware lint both
  stayed green after annotation, and no runtime behavior moved. The transcript is the one
  place the daemon reads a file another process is actively appending to, so the fields
  genuinely *are* `unknown` at the boundary; typing them as such and narrowing with
  `typeof`/optional-chaining just makes the defensiveness the JS already had explicit.
- **Fix:** (1) `const it: TailIterator = Object.assign(gen, { truncated: start > 0 })` — the
  built-in `Object.assign` overload returns the intersection `Generator<TailLine, void,
  unknown> & { truncated: boolean }` (aliased `TailIterator`), attaching the flag with **no**
  unsafe cast. (2) Declared structural boundary types (`TranscriptEntry`, `ContentBlock`)
  with **`unknown`** fields and asserted the parse once (`JSON.parse(line) as TranscriptEntry
  | null`); every downstream access narrows (`typeof content === 'string'`,
  `Array.isArray(content)`, `b?.type === 'text' && typeof b.text === 'string'`). The
  array-of-blocks `.filter(...).map(...).join()` became an explicit `for` loop pushing to a
  `string[]` — behavior-identical, but each block narrows cleanly. (3) `const raw = lines[i];
  if (raw === undefined) continue;` — an in-bounds guard the runtime never hits, satisfying
  `noUncheckedIndexedAccess`. **No runtime behavior moved** — 18/18 pure tests
  (`transcript-reader` + `audit-cleanup`) green, and 25/25 daemon-spawn tests
  (`model-tracking` + `needs-you`) green vs **both** source and bundle.

### plans.ts — integer ids in templates + untrusted-body `unknown` narrowing   [NOISE]
- **What:** two things surfaced on the first daemon leaf that templates primary-key
  **numbers**. (1) `@typescript-eslint/restrict-template-expressions` (from
  `strictTypeChecked`, which pins `allowNumber: false`) flagged all four
  ``\`📚 plan #${p.plan_id} …\``` / ``\`…#${p.plan_id}…\``` sites — a `number` operand in a
  template. (2) The two request bodies (`{status, via}`, `{to, instructions}`) arrive off
  the wire, so their fields are `unknown`; `body?.via?.trim()` / `body?.instructions?.trim()`
  don't type-check until the value is narrowed to a string.
- **Why it's real / why it's noise:** NOISE on both counts — `tsc` stayed green; only the
  maximal type-aware lint fired, and no runtime behavior is at risk. An integer primary key
  stringifies losslessly (`123` → `"123"`) with none of the `[object Object]` / `"null"` /
  `"true"` hazards `restrict-template-expressions` exists to catch, and the daemon templates
  ids **everywhere** (24+ `#${…id}` sites across the still-JS modules, growing as they
  convert). The body-field narrowing is just the honest cost of typing untrusted JSON as
  `unknown` — the original JS already did the exact same `typeof`/`!= null` runtime guards.
- **Fix:** (1) a **deliberate, fleet-wide config decision** — re-allow **only numbers** in
  templates: `'@typescript-eslint/restrict-template-expressions': ['error', { allowNumber:
  true }]` in both type-aware blocks of `eslint.config.mjs` (daemon + board), commented in
  place. Every other operand (`any` / `boolean` / `nullish` / `never` / `regexp`) stays
  banned at its strict default — this narrows exactly the one operand that is provably safe,
  rather than sprinkling `String(id)` across the whole codebase. It is a lint-ergonomics
  choice, **not** a `tsc`-strictness loosening (the compiler always accepted `${number}`).
  (2) Hoisted each body field to a typed local before use
  (`const rawVia: unknown = body?.via; if (rawVia != null && typeof rawVia !== 'string')
  return 400; const via = typeof rawVia === 'string' && rawVia.trim() ? … : null`) — a
  `typeof` guard TS follows, behavior-identical to the original optional-chain. Also typed
  the HTTP result as a discriminated `{ ok:false, err } | { ok:true, … }` union so
  `assignPlan` re-uses `planMark` and short-circuits on `!marked.body.ok` without an
  index-signature access. **No runtime behavior moved** — 32/32 plan tests
  (`plans` + `accept-plan-*`) green vs both source and bundle.

### ledger.ts — tighter `LedgerKey` exposed dead `?? ''` / `?? null` fallbacks   [NOISE]
- **What:** once `ledgerKey` returned the honest `LedgerKey` (`repo_id: string`,
  `worktree: string | null`) from the just-converted repo-identity, `ledger`'s three
  `key.repo_id ?? ''` and two `key.worktree ?? null` / `t.worktree ?? null` guards became
  provably dead — `@typescript-eslint/no-unnecessary-condition` would have flagged each
  `??`/`||` whose left side can no longer be nullish.
- **Why it's real / why it's noise:** NOISE — a downstream ripple of the repo-identity
  discriminated-union entry, not a defect. `repo_id` is always a string (`''` outside git,
  never null), so `?? ''` never fired; `x ?? null` on a `string | null` is identity. The
  DB-row worktree the code normalized with `?? null` is `string | null` from node/bun
  sqlite (never `undefined`), so the normalization was dead too.
- **Fix:** dropped the fallbacks (`key.repo_id`, `key.worktree`, `t.worktree === key.worktree`)
  and `editorCard.cwd || '/'` → `?? '/'`. Typed the threaded ctx (`LedgerCtx`) and the
  DB-row shapes (`TouchRow`, `SessionStateRow`) as **provisional** structural interfaces —
  they name exactly what the still-JS statements layer must provide and will be replaced by
  its real exports when it converts. **No runtime behavior moved** — conflict.test.mjs 2/2
  green vs both source and bundle.

### repo-identity.ts — `||`→`??` on nullable git results + `Map` iterator typing   [NOISE]
- **What:** type-aware ESLint raised `prefer-nullish-coalescing` twice — on
  `git([...]) || ''` (the porcelain `worktree list`) and on
  `canon(listedMain || toplevel || cwd)` (main-tree fallback chain) — both where the
  left operand is nullable (`string | null` / `string | undefined`). `tsc` also required
  a guard where `cacheSet` evicts the LRU entry: `cache.keys().next().value` is typed
  `string | undefined` (`IteratorResult`), which `Map.delete(key: string)` rejects.
- **Why it's real / why it's noise:** NOISE — no behavior change. The `||` operands can
  never be the empty string in practice (`git()` normalizes empty output to `null`;
  `Array.find` yields `string | undefined`; a real path is never `''`), so `??` selects
  the identical branch. The eviction guard is dead at runtime — the `while (size > MAX)`
  condition proves a key exists — but the checker can't see that a non-empty Map yields a
  defined first key. Note the sibling `path.basename(mainTree).replace(/\.git$/, '') ||
  path.basename(mainTree)` was correctly **not** flagged and kept as `||`: its left
  operand is a pure `string` and the `''` fallback there is intentional.
- **Fix:** swapped the two nullable `||`→`??`; added `const oldest = …; if (oldest ===
  undefined) break;` before the evicting `delete`. Also gave the two public result shapes
  honest types: `RepoIdentity` is a **discriminated union** on `is_git`, so
  `is_git: true` guarantees non-null `repo_id`/`worktree`/… and `ledgerKey`'s git branches
  narrow without a cast; `LedgerKey.repo_id` is `string` (`''` outside git, never null).
  **No runtime behavior moved** — 8/8 repo-identity + 7/7 audit-cleanup green; bundle
  re-parses.

### tickets.ts — `unknown` params + `noUncheckedIndexedAccess` on regex/`split` results   [NOISE]
- **What:** type-aware ESLint (not `tsc`) raised three on the first daemon-only leaf:
  `restrict-template-expressions` at `:30` (``\`${m[1]}-${m[2]}\``` — `RegExpExecArray`
  indices are `string | undefined` under `noUncheckedIndexedAccess`, so the template
  "may stringify undefined"), and `no-base-to-string` at `:38`/`:59` (`String(raw)` /
  `String(callsign)` where the arg was typed `unknown`, which could be an object →
  `"[object Object]"`).
- **Why it's real / why it's noise:** NOISE — no runtime defect. `tsc` alone stayed
  green; only the maximal type-aware lint rules fire. The `unknown` params were *my*
  over-loose first-pass signatures, not the original JS contract: every call site feeds
  a known-narrow value (`ticketFromBranch` ← `branchOf()` → `string | null`;
  `normalizeTicket` ← a parsed CLI arg → `string | undefined`; `animalOf` ← a
  `callsign` string). The regex-index warning is pure `noUncheckedIndexedAccess`
  pessimism — a successful `exec` on a two-group pattern always populates `m[1]`/`m[2]`.
- **Fix:** tightened the signatures to the call-site-honest types (`string | null`,
  `string | undefined`, `string`) so the `String()` coercions drop away, and replaced
  the bare template with an explicit destructure + `undefined` guard
  (`const [, proj, num] = m; return proj !== undefined && num !== undefined ? … : null`).
  The `: null` branch is unreachable-but-honest. **No runtime behavior moved** — same
  keys extracted, same `null`s returned; 7/7 `tickets.test.mjs` green vs both source and
  bundle.

### board/useFleetState.ts — board consumed `any` across the daemon-trust boundary   [UNSOUND]
- **What:** type-aware ESLint (`@typescript-eslint/no-unsafe-*`, `no-unnecessary-condition`,
  `prefer-optional-chain`) raised 10 errors when the new `useFleetState.ts` consumed values
  that are typed `any`: `fetchState()` from the still-JS `board/src/api.js` (returns `res.json()`
  → `any`) and `JSON.parse(e.data)` on the `/ws` frame (`e.data` is `any`). Flagged at:
  `useFleetState.ts:142` (`data.lan` unsafe member/assignment), `:144`/`:182` (`apply(data)`
  unsafe `any`→`BoardSnapshot` argument), `:181` (`const data = JSON.parse(...)` unsafe
  assignment + `JSON.parse(any)` unsafe argument), `:182` (`data && data.type` → prefer optional
  chain, `.type` unsafe member), and `:125` (`prev.sessions || EMPTY.sessions` — `prev.sessions`
  is a required `SessionEntry[]`, so the `||` fallback is provably dead).
- **Why it's real / why it's noise:** UNSOUND, not a latent runtime bug — the board assumed the
  daemon's wire shape by *convention* the linter can't see (the F1 design deliberately keeps
  runtime validation in the daemon's `contracts/`, so the browser trusts the shape). Strict
  type-aware lint correctly refuses to let `any` flow silently into a `BoardSnapshot`. The dead
  `|| EMPTY.sessions` is genuine cruft the required-array type exposed.
- **Fix:** made the trust boundary explicit — cast at the two ingest points (`fetchState()` →
  `BoardSnapshot | null`; the `/ws` frame → `(BoardSnapshot & { type?: string }) | null`) with a
  comment pointing here, switched `data && data.type` to `data?.type`, and dropped the dead
  `|| EMPTY.sessions`. **No runtime behavior moved** (casts erase; the optional-chain and the
  dropped `||` are equivalent for the required, always-present values). The two casts are marked
  to be removed in Phase 8 once `api.js` converts and `fetchState()` returns the typed wire
  contract, which will let the browser narrow instead of assert.

### scripts/fleetd/ingest.ts — typed the `unknown` poll payload with a real predicate; kept one intentional `||`   [NOISE]
- **What:** `ingestAgentsPoll(records)` receives whatever `JSON.parse` produced (typed `unknown`),
  and the inline `.filter(rec => rec && typeof rec === 'object' && rec.sessionId && …)` reads
  `rec.sessionId/kind/pid/startedAt` off an `unknown` element — every access errors under strict.
  Separately, `@typescript-eslint/prefer-nullish-coalescing` flagged `const cwd = rec.cwd || null`
  because `rec.cwd` is now typed `string | null | undefined` (the earlier `|| null` survivors in
  `exec.ts`/`repo-identity.ts` have non-null `string` operands, so the rule never fired there).
- **Why it's noise:** no latent bug — the merge logic was already defensive; strict typing only
  demands the `unknown` be *narrowed* before use, and the `||` is a deliberate falsy-fold. The one
  place semantics could drift is pid ownership: the old filter passed `rec.startedAt` straight into
  `pidOwnedBy(pid, startedAt)`. Folding a non-number `startedAt` to `NaN` is behavior-identical
  because `Math.abs(startMs - x)` is already `NaN` for any non-number `x` (→ tolerance check false),
  and the existing `Number.isFinite(rec.startedAt)` guard three lines down already treats even a
  numeric *string* as non-finite — so the module already assumed `startedAt` is a real number.
- **Fix:** (1) introduced structural types local to the module — `AgentRecord` (the read fields of a
  `claude agents --json` record) and `IngestCtx` (the six ctx members this factory threads), reusing
  `Statements['q']`/`Statements['updateSession']`/`RepoIdentity`/`SqlValue` from already-converted
  modules rather than redeclaring them (contracts/ carries only wire shapes, not the internal ctx).
  (2) Replaced the inline filter with a `rec is AgentRecord` type-guard (`isFleetRecord`) that checks
  `typeof rec === 'object' && rec !== null`, a non-empty string `sessionId`, `kind === 'interactive'`,
  a numeric `pid`, and `pidOwnedBy(pid, typeof startedAt === 'number' ? startedAt : NaN)` — so `live`
  is a real `AgentRecord[]`. (3) Hoisted the `updateSession` patch object to a typed
  `const patch: Record<string, SqlValue>` so the conditional-spread build is checked against the
  writer's parameter type. (4) Kept `rec.cwd || null` verbatim behind an
  `eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing` with a rationale — an
  empty-string cwd is not a path, so `||` (not `??`) correctly folds `''` to `null`. **No runtime
  behavior moved** — 35/35 across `agents-ingest` + `audit-cleanup` + `adopt` green vs both source
  and the regenerated bundle.

### scripts/fleetd/agents-poll.ts — index-signature env reads, a redundant guard TS already narrowed, and two async/closure lint false-positives   [NOISE]
- **What:** four unrelated strict/lint complaints, none a latent bug. (1) `tsc` TS4111 ×4 on
  `process.env.FLEETDECK_AGENTS_POLL_MS` / `_IDLE_POLL_MS` / `_CMD` — `NodeJS.ProcessEnv` is an index
  signature, and `noPropertyAccessFromIndexSignature` forbids dot access on it. (2) `no-unnecessary-condition`
  "types have no overlap" on the `argv !== null` I had added to the poll guard. (3) `no-misused-promises`
  on `setTimeout(tick, …)` — `tick` is `async` (returns a floated `Promise`), and `setTimeout` wants a
  `() => void`. (4) `no-unnecessary-condition` "always truthy" on `if (!stopped)` in the reschedule step.
- **Why it's noise:** (1) is a syntax rule, not a type hole. (2) is TS being *more* precise than I was —
  `const agentsEnabled = argv !== null` is a const aliased condition, so inside `if (agentsEnabled && …)`
  TS already narrows `argv` to `string[]`; my extra `argv !== null` compared a non-null type to `null`
  (hence "no overlap"), and it was pure redundancy. (3) the floated promise was always intentional
  fire-and-forget (the original `.mjs` passed the async `tick` straight to `setTimeout`); `tick` never
  rejects — every await is wrapped and the finally reschedules — so nothing changed but the linter wants
  the discard made explicit. (4) is a genuine TS **unsoundness**, not dead code: the entry guard
  `if (stopped || running) return` narrows `stopped` to `false`, and TS *carries that narrowing across the
  two `await`s* down into the `finally`; but `stop()` can flip `stopped` during an in-flight await, so the
  reschedule guard is live at runtime — dropping it would make a stopped poller reschedule itself forever.
- **Fix:** (1) bracket access `process.env['FLEETDECK_AGENTS_POLL_MS']` etc. on all four reads. (2) deleted
  the redundant `&& argv !== null`; the guard is now `if (agentsEnabled && Date.now() >= nextAgentsPollAt)`
  and `argv` stays narrowed to `string[]` for `runOnce(argv)` via the aliased-const. (3) wrapped the
  callback as `setTimeout(() => void tick(), delayMs)` — explicit void discard, identical scheduling. (4)
  kept `if (!stopped) schedule(POLL_INTERVAL_MS)` behind an
  `eslint-disable-next-line @typescript-eslint/no-unnecessary-condition` with a rationale pointing at the
  await-carried-narrowing unsoundness (same idiom as ingest.ts's intentional-`||` disable). Timer stays
  `ReturnType<typeof setTimeout> | null`, so `stop()` keeps `if (timer) clearTimeout(timer)` (tsc needs the
  null guard; eslint agrees it's meaningful). **No runtime behavior moved** — 21/21 across `agents-ingest`
  + `audit-cleanup` + `exec-timeout` green vs both source and the regenerated bundle; the test's
  cache-busting dynamic `import('…/agents-poll.ts?audit=…')` resolves under Node type-stripping unchanged.

### scripts/fleetd/config.ts — env reads and two untyped params   [NOISE]
- **What:** the three `process.env.FLEETDECK_*` / `env.CODER*` reads tripped TS4111
  (`noPropertyAccessFromIndexSignature`), and `detectCoderWorkspaceRoot`'s destructured options bag
  plus its inner `present = v => …` helper were implicit-`any` under `noImplicitAny`.
- **Why it's noise:** pure resolver module, no logic touched; strict only wants env accessed by
  index and the two params annotated.
- **Fix:** bracket access on every env read; typed the options bag as
  `{ env?: NodeJS.ProcessEnv; probeDir?: string }` (matching the runtime defaults) and `present` as
  `(v: unknown): boolean`. Return types made explicit (`string` / `number` / `string | null`). The
  `os.homedir() || '/tmp'` stays `||` — `homedir()` is non-null `string`, so prefer-nullish-coalescing
  never fires. Nine importers (three hook scripts, check-release-gate, repos, settings, fleetd, two
  tests) repointed to `./config.ts`. 39/39 (`port-validation` + `repos`) green vs source and bundle.

### scripts/fleetd/run-nonce.ts — env read, unknown-catch, pid predicate, dead initializers   [NOISE]
- **What:** four strict frictions, all mechanical: (1) `Number(env.CLAUDE_PID)` tripped TS4111
  (`noPropertyAccessFromIndexSignature`); (2) the prune loop's `catch (err) { if (err?.code !== 'ESRCH') … }`
  reads `.code` off an `unknown` binding (`useUnknownInCatchVariables`); (3) `isPid = v => …` was
  implicit-`any` under `noImplicitAny`, and — since `runKey` calls it on `walked: number | null` — a plain
  `boolean` return would not narrow `walked` to `number` for the `{ key: walked }` return; (4) eslint
  `no-useless-assignment` flagged `let comm = ''` / `let stat = ''` because both are overwritten in the
  `try` and the `catch` returns, so the `''` seed is never read.
- **Why it's noise:** no control flow or values changed; the nonce keying, /proc walk, mint-on-first-use,
  and prune-when-provably-dead logic are byte-for-byte the original. Strict only wants the env indexed, the
  caught error narrowed, the predicate typed, and two dead seeds dropped.
- **Fix:** (1) bracket access `env['CLAUDE_PID']`. (2) added the same local `errnoCode(e: unknown)` helper
  paste.ts/db.ts already use (`e instanceof Error && typeof (e as NodeJS.ErrnoException).code === 'string'`);
  the guard is now `errnoCode(err) !== 'ESRCH'` — identical semantics (a non-Error throw yields `undefined`,
  which is `!== 'ESRCH'`, so the file is kept, exactly as `err?.code` did). (3) typed `isPid` as a predicate
  `(v: unknown): v is number => typeof v === 'number' && Number.isInteger(v) && v > 0` so `walked` narrows at
  the call site; the extra `typeof` is a runtime no-op for the numbers every caller passes. (4) declared
  `let comm: string` / `let stat: string` without the seed — TS proves definite assignment because the only
  path past each `catch` is the `try` succeeding. Params/returns annotated (`RunKey` interface for the
  `runKey` shape, `home: string`, `env: NodeJS.ProcessEnv`, `ppid: number`, prune options bag
  `{ minAgeMs?: number; now?: number }`). Four importers (fleet-sessionstart, fleet-hook, retention, the
  test) repointed to `./run-nonce.ts`. 6/6 (`run-nonce`) green vs source; hook scripts (5/5 across
  `hook-stubs` + `hook-missing-session-id`) still import it fine under type-stripping; the daemon bundle
  (prune reaches it via `retention` → `derive`) rebuilds with the `run-nonce.ts` banner and passes
  `node --check`.

### scripts/fleetd/takeover.ts — JSON.parse→unknown, three unknown-catch errno guards, semver index guards   [NOISE]
- **What:** four strict frictions, all mechanical: (1) `pidRecord` did `JSON.parse(String(text))` and
  read `parsed?.pid` / `parsed.port` off the result — `JSON.parse` is typed `any`, so `strictTypeChecked`'s
  `no-unsafe-*` rules fire on every member access; (2) three `catch (err) { … err?.code … }` sites
  (`pidIsLive` ESRCH, `livePidLooksLikeFleetd` ENOENT, `terminateDaemon` ESRCH) read `.code` off an
  `unknown` binding under `useUnknownInCatchVariables`; (3) `noUncheckedIndexedAccess` makes every
  `a.core[i]` / `a.pre[i]` in `compareSemver` (and `split('+', 1)[0]` in `parseSemver`) a `T | undefined`;
  (4) after typing `text: string`, eslint `no-unnecessary-type-conversion` flagged the now-redundant
  `String(text)` wrappers, and `no-useless-assignment` flagged `let record = null` in `verifyDaemonPid`
  (the only path past the catch is the try succeeding, so the `null` seed is dead).
- **Why it's noise:** the pidfile parse, /proc identity shape, full SemVer precedence (incl. prerelease
  ordering), takeover/verify predicates, and graceful-terminate poll are byte-for-byte the original. Strict
  only wants the parse narrowed, the caught errors narrowed, the array indices proven in-bounds, and the
  two dead expressions dropped.
- **Fix:** (1) `const parsed: unknown = JSON.parse(text)` then narrow with
  `typeof parsed === 'object' && parsed !== null` + a `{ pid?: unknown; port?: unknown }` cast + per-field
  `typeof … === 'number'` guards; result shaped by a new `interface PidRecord { pid: number; port: number | null }`.
  (2) added the same local `errnoCode(e: unknown)` helper paste.ts/db.ts/run-nonce.ts use; the guards are
  now `errnoCode(err) !== 'ESRCH'` / `!== 'ENOENT'` / `=== 'ESRCH'` — identical semantics (a non-Error
  throw yields `undefined`, which satisfies the same branches `err?.code` did). (3) new
  `interface Semver { core: number[]; pre: (number | string)[] }`; `parseSemver` uses `?? ''` on the
  build-metadata split (a no-op: splitting a non-empty string always yields ≥1 element); `compareSemver`
  captures `const ai = a.core[i]` / `const x = a.pre[i]` and `break`s on `undefined` (unreachable — both
  cores are length-3), and captures `const xNum = typeof x === 'number'` so aliased-const narrowing lets
  the `x > y` numeric compare typecheck. (4) dropped the redundant `String(...)` wrappers (param is now
  `string`; all four callers pass `readFileSync(…, 'utf8')`) and declared `let record: PidRecord | null`
  without the seed. Public predicates (`parseSemver`, `compareSemver`, `shouldTakeOver`, `verifyDaemonPid`,
  `replacementMatches`) take `unknown` where they parse/validate untrusted version strings. Four references
  (fleetd, fleet-sessionstart, check-release-gate's `WATCHED` closure list, the test) repointed to
  `./takeover.ts`. 15/15 (`takeover`) green vs source; the daemon bundle rebuilds with the `takeover.ts`
  banner and passes `node --check`.

### scripts/fleetd/mail.ts — ctx.q is Statements['q'] not Statements, unknown mail body, unicode-in-comments   [NOISE]
- **What:** the dominant friction (≈180 of ~190 lint errors, all one root cause): I first typed
  `MailCtx.q: Statements`, but `Statements = ReturnType<typeof build>` is the whole
  `{ q, FIELDS, updateSession }` bundle — the daemon threads only the nested prepared-statement map onto
  ctx (`derive.mjs: const { q } = createStatements(db)`). So `q.pendingMailStats` etc. resolved to
  `error`-typed non-existent members and every `.get()/.all()/.run()/.map()` cascaded into
  `no-unsafe-*` + `restrict-plus-operands`. The rest were mechanical: (1) the external POST body reaches
  `mail()`/`postMail()`/`hasReservedFrame()` as `unknown`, and the pre-migration `String(text ?? '')` trips
  `no-base-to-string` (an object would stringify to `[object Object]`); (2) `ackMail(ids)` took `unknown`;
  (3) `pendingMailStats.get()` is `{n,bytes} | undefined` under strict; (4) `noUncheckedIndexedAccess`
  makes `outcomes[i]` / `routes[i]` / `namedByTo[0]` a `T | undefined`; (5) `timer.unref?.()` tripped
  `no-unnecessary-condition` (`NodeJS.Timeout.unref` is non-optional); (6) three comments embedded a literal
  U+200B (ZWSP) to illustrate the BUG-032 attack and the line-terminator regex carried literal
  U+2028/U+2029 bytes — `no-irregular-whitespace` flags the ZWSPs; (7) a misplaced
  `// eslint-disable-next-line no-control-regex` sat two comment lines above `FROM_UNSAFE_RE` (out of range),
  so `no-control-regex` fired on the regex while the directive read as unused; (8) `array-type` wanted
  `{ kind: string }[]` over `Array<{ kind: string }>`.
- **Why it's noise:** the mailbox bounds (BUG-4/6/12/128), the sender/frame reservation (0.16.0 + BUG-032/
  035/036/063), the expiring-lease claim (BUG-034), owned-pane delivery, and the fanout/route reporting are
  byte-for-byte the original. Strict only wanted the ctx statement bundle named correctly, the untrusted
  body narrowed, the caught/aggregate/index values proven, one dead optional-chain dropped, and the
  attack-illustrating unicode written as escapes.
- **Fix:** (1) `q: Statements['q']` — the single change that cleared the cascade; the COUNT satisfier
  `q.pendingMailStats.get(toSession) ?? { n: 0, bytes: 0 }` then typechecks as `{n,bytes}` (aggregate always
  returns one row, so the branch is dead). (2) new `asText(value: unknown): string` helper — `value == null`
  → `''`, a `string` passes through, else a scoped `// eslint-disable-next-line no-base-to-string` guards a
  faithful `String(value)`; the reserved-frame probe, the clamp, and the truncation report all route
  through it so they never disagree on the body. (3) `ackMail`: `if (!Array.isArray(ids)) return {acked:0}`
  then per-id `typeof id === 'number' && Number.isSafeInteger(id)` before `q.ackMail.run`. (4) captured
  `const outcome = outcomes[i]` / `routes[i] ?? 'turn-boundary'` / `namedByTo[0]?.callsign ??
  namedByTo[0]?.session_id ?? 'session'`. (5) `timer.unref()`. (6) restored `\u2028\u2029` escapes in the
  line-terminator regex and rewrote the three ZWSP comments to spell `\u200b` (behavior identical — the
  regexes match `\p{Cf}`, never a literal ZWSP). (7) moved the `no-control-regex` disable to directly above
  `FROM_UNSAFE_RE`. (8) `{ kind: string }[]`. Also tightened `resolveTargets(to: string)` (dropped a
  redundant `String(to ?? '')` — `RegExp.exec` ToString-coerces its arg anyway) and closed the pane-envelope
  template nullish holes (`${pair.sp.tmux_window ?? '?'}`, `${pair.c.callsign ?? pair.c.session_id}`), and
  fixed a reserved-sender message that interpolated `${from}` instead of the resolved `${sender}`. `db` typed
  `SqliteHandle`; structural `TmuxWindow`/`PaneCommand`/`TmuxAdapter`/`MailQuestions`/`MailCtx`/`MailResult`
  interfaces describe the ctx slice (owning modules are still `.mjs`, so ctx is the contract boundary). Two
  importers (`commands.mjs`, `derive.mjs`) repointed to `./mail.ts`. 25/25 (mail-and-blocking,
  mail-delivery-lease, mail-frames, smoke-mail-gate) green vs source; the daemon bundle rebuilds with the
  `mail.ts` banner and passes `node --check`.

### scripts/fleetd/questions.ts — the needs-you/hold/re-arm engine; index-signature env read, dead defensive guards, `||`→`??`   [NOISE]
- **Where:** `questions.mjs` (1100+ lines — the largest leaf), the arbitrator behind the needs-you rail:
  the socket hold, the answer pipeline, and the re-arm grace chain that heals an unanswered card into a
  successor. Zero behavioral defects surfaced; every strict finding was mechanical.
- **Why it's noise:** the hold clamp (`resolveHoldMs`, default 600 s / ceiling 650 s), the chain cap
  (`rearmMax`), the BUG-137 2000-unit answer guard, the plan-answered flip, the grace-window activity
  cancellation, and the socket-close settle are byte-for-byte the original. Strict only wanted the
  `ProcessEnv` read bracketed, the untrusted persisted payload/answer bodies named, a handful of guards
  TS had already proven redundant dropped, and four domain-equivalent `||` swapped for `??`.
- **Fix (typing):** structural interfaces for the store row and the untrusted JSON —
  `QuestionRow` mirrors the `questions` table in `db.ts` (source of truth); `QuestionPayload` /
  `AnswerBody` / `ChoiceQuestion` carry every field optional-or-nullable so the runtime guards that
  already existed still do the narrowing. `db: SqliteHandle`; `db.prepare<QuestionRow>(...)` types the
  get/pending/resolved statements. Timers are `ReturnType<typeof setTimeout>`; the callback bag
  (`mail`/`tick`/`onChange`/`planAnswered`/`onRetired`/`callsignOf`/`planIdFor`/`resolveHoldWindow`) is a
  fully-typed `QuestionsOptions`. `create()` throws on the impossible re-read miss (the row was just
  inserted) so its return is non-nullable for every caller.
- **Fix (lint, all behavior-preserving):** (1) **TS4111 / noPropertyAccessFromIndexSignature** —
  `env.FLEETDECK_HOLD_MS` → `env['FLEETDECK_HOLD_MS']` (`ProcessEnv` is an index signature). (2)
  **no-unnecessary-type-parameters** on `safeParse<T = unknown>` — kept with a scoped inline-disable: the
  single-use generic centralizes the `JSON.parse` cast so each call site reads `safeParse<Shape>(json)`
  instead of a bare `as`; a deliberate ergonomic param, not a disguised cast. (3) **no-empty-function** ×5
  — the default no-op callbacks (`() => {}`) gained a `/* no-op default */` body. (4) **prefer-optional-chain**
  ×3 — `if (!row || row.status !== 'expired')` → `if (row?.status !== 'expired')` (fireRearm, recycleRearm,
  and the grace-map walk in the activity-correlation path). (5) **no-unnecessary-condition** ×3 — dropped
  dead `?.` the linter proved redundant: in the re-armed branch `body.action` is reached only inside
  `body?.action === 'accept' || body?.action === 'decline'` (so `body` is non-null there), and
  `payload.tool_input?.…` / `payload.text` are reached only after `payload?.rearmed === true` guards
  `payload` non-null. (6) **prefer-nullish-coalescing** ×4 `||`→`??`, each verified domain-equivalent —
  `callsignOf(...) ?? session_id` (a callsign is never the empty string), the AskUserQuestion `header ?? qText`
  (a present header is required non-empty), and the two `detectTrailingQuestion` paragraph/line fallbacks that
  target `''` (so `||` and `??` pick the same branch). `Number(stmt.run(...).changes)` wraps only the three
  relational `=== 0` / `> 0` sites (bigint-safe); bare truthy `if (...changes)` contexts left unwrapped.
- **Verify:** three importers repointed to `./questions.ts` (`settings.mjs`, `events.mjs`, `derive.mjs`), plus
  two direct-import tests (`question-rearm`, `questions-audit`). Fixed a stale `db.mjs` reference in
  `hook-auth.test.mjs`'s generated fleetd wrapper — a leftover from the `db.ts` conversion that only failed once
  a `.ts`-importing daemon child was actually spawned (Node 22.22 strips types in the child too). 103/103 across
  question-rearm, questions-audit, needs-you, dismiss, accept-plan-isolation, succession, hook-stubs,
  static-serving, hook-auth green vs source; the daemon bundle rebuilds with the `questions.ts` banner and passes
  `node --check`.

### scripts/fleetd/commands.mjs → commands.ts  [NOISE]

- **What:** The POST `/command` surface (broadcast / assign / assign auto / ticket / name / note).
  A small pure leaf (199L) whose only cross-module deps are already-converted `.ts` siblings
  (`helpers.ts` `parseCommand`/`validateNameSuffix`, `tickets.ts` `normalizeTicket`, `mail.ts`
  `MAIL_MAX_LEN`). No runtime behavior changed; every edit was a type annotation, a narrowing
  rewrite the compiler accepts as equivalent, or a dead impossible-miss guard.
- **Types added:** `CommandsCtx` for the threaded ctx — `q: Statements['q']` (the daemon threads
  only the prepared-statement bundle onto ctx, matching the mail.ts/questions.ts idiom, NOT the
  whole `Statements` object), plus fully-typed `mail`/`resolveTargets`/`tick`/`onMutate`/
  `applyTicket`/`updateSession`/`applyCustomName` callbacks. `resolveTicketTarget` returns a
  `TicketTarget = { sid: string } | { error: string }` discriminated result. `RenameResult` (the
  bag returned by `clearTicket`/`applyTicket`/`applyCustomName`) is typed `Record<string, unknown>`:
  commands.ts only ever SPREADS it into the `/command` response (`{ session_id, ...result }`) and
  never reads a field back, so the loose shape is the honest boundary contract — a narrower interface
  would be fiction about what the ctx callbacks (defined in derive.mjs, not yet converted) actually
  return. `clearTicket`'s `let result` and `command`'s `let result` are annotated `: RenameResult`
  because each is reassigned with an extra key (`previous`) that would trip excess-property checks
  against the first literal's inferred type.
- **Narrowings (behavior-preserving):** `parseCommand` returns a discriminated union where the
  `ticket`/`name` members are EITHER `{ target, ticket|suffix }` OR `{ error }`. The `.mjs` tested
  `if (parsed.error)` / `if (resolved.error)`; under the union those become `if ('error' in parsed)`
  / `if ('error' in resolved)` — `in`-narrowing removes the error member on the false branch so the
  subsequent `parsed.target`/`parsed.ticket`/`parsed.suffix` / `resolved.sid` accesses typecheck.
  Faithful: `error` is present iff the parse/resolve failed, exactly the runtime condition the truthy
  test encoded (an error string is always non-empty).
- **noUncheckedIndexedAccess:** `resolveTicketTarget` returned `found[0].session_id` after two
  length guards (`=== 0`, `> 1`) that prove exactly one element; the compiler still types `found[0]`
  as `Row | undefined`. Capture-then-guard (`const only = found[0]; if (!only) return {error…}`)
  keeps the impossible-miss honest — dead in practice, never a `!` assertion. Reuses the "no live
  session" reason string so even the unreachable path is truthful.
- **asText:** `command(text: unknown)` logs the raw command via `q.insertCommand.run(..., asText(text), ...)`.
  Added the local `asText` copy (null → `''`, string passthrough, else `String()` with a scoped
  `no-base-to-string` disable) matching mail.ts:54 / questions.ts — `asText` is deliberately NOT
  exported anywhere; each module keeps its own three-line copy. Replaces the `.mjs`'s
  `String(text ?? '')` with identical output.
- **History (honest):** commit `dcb115c2` landed `commands.ts` as a RAW untyped rename — the file kept
  `createCommands(ctx)` with no annotations and threw 37 tsc errors — while its message and this very
  entry claimed the strict typing above was applied. It was not. The typing described here was actually
  applied in a later pass (this commit), which is also when the entry's Verify numbers below became real.
- **Verify:** tsc `--noEmit` clean project-wide (0 errors); `eslint commands.ts` clean (0). One importer
  repointed (`derive.mjs` `./commands.mjs` → `./commands.ts`; the other three matches were comments).
  56/56 green vs source across fleet-command, rename, ticket-callsign, tickets, mail-frames,
  mail-and-blocking, accept-plan-snapshot, smoke-state-poll; plus 65/65 across the derive integration
  path (`derive-audit-reliability`, which imports commands.ts/snapshot.ts/retention.ts) and the
  retention-maintenance set. Daemon bundle rebuilds with the `commands.ts` banner (624.5kb) and passes
  `node --check`.

### scripts/fleetd/snapshot.mjs → snapshot.ts  [UNSOUND + NOISE]

- **What:** The `/state` snapshot builder (`snapshot()`), `fleetSize()`, and the live-terminal
  spawn resolver `terminalSpawn()` (305L). A pure read-only leaf: every `q.*` it calls is a SELECT
  already typed in `statements.ts`, and its cross-module deps (`helpers.ts` `spawnRowRevivable`/
  `sessionAdoptableNow`, `payload-capture.ts` `scrubUrlCredentials`, `contracts/index.ts`
  `WIRE_SCHEMA_VERSION`) are already `.ts`. Two genuine findings below; the rest is strict-mode
  ergonomics with no behavior change.

- **UNSOUND — SpawnRow.kind typed `string`, DDL column is nullable.** `spawns.kind` is declared
  `kind TEXT DEFAULT 'claude'` (db.ts:194) with no NOT NULL, and BOTH inserts (`insertSpawn`/
  `insertProvisionalSpawn`) bind it **positionally** — so the DEFAULT never applies and a row can be
  written NULL. The `statements.ts` `SpawnRow.kind: string` was optimistic; corrected to
  `string | null` (with a comment pointing here). This is exactly what keeps snapshot's
  `kind: sp.kind ?? 'claude'` and `revivable: sp.kind === 'shell' ? …` honest — the optimistic type
  would have made `no-unnecessary-condition` flag the `?? 'claude'` fallback as dead and invited its
  removal, silently changing a NULL-kind row's projected kind from `'claude'` to `null` on the wire.
  No runtime change (the `??` was already there); the type now matches the write path.

- **UNSOUND — dead `s.last_seen != null` guard removed.** The spike's stale-badge derivation read
  `s.last_seen != null && (now - s.last_seen > STALE_MS)`. `SessionRow.last_seen` is a write-path-
  faithful non-null (`number`): it is an original base column (no ALTER backfill → no legacy NULL
  rows), all three INSERTs bind it, and every updater stamps `Date.now()`/`now` (audited:
  events.mjs:194/597, retention.mjs:123, spawns.mjs:440, ingest.ts:161 — never null/undefined). So
  the `!= null` operand is provably `true` in every reachable state, `no-unnecessary-condition`
  flagged it, and dropping it is behavior-identical (`true && X === X`). Kept the guard's *intent*
  as a comment. (Had the audit found a null-writing path, the fix would have been to type `last_seen`
  `number | null` and KEEP the guard — the guard drops only because the type is proven correct.)

- **NOISE — Map generics + capture-restructures.** `noUncheckedIndexedAccess` + the fact that
  `Map.get()` does not narrow after a separate `.has()` forced two loops to capture-then-mutate the
  value instead of `has`/`get`-ing twice: `sparkBySid` (`let bins = m.get(id); if (!bins) { bins =
  new Array<number>(30).fill(0); m.set(id, bins); } … bins[idx] = …`) and `repoMap` (`let r =
  m.get(key); if (!r) { r = {…}; m.set(key, r); } r.total++`). Semantically identical to the
  `.has()`/`.get()` originals, and mutating the captured reference writes through to the stored
  object. All Maps got explicit generics (`Map<string, string[]>`, `Map<string, number[]>`,
  `Map<string, SpawnRow>`, `Map<string, boolean>`, the repo-tally value shape).

- **NOISE — Map-from-entries `as const`.** `new Map(rows.map(r => [k, v]))` infers `(K|V)[][]` and
  the constructor rejects it; `pendingBySid` and `callsignById` use `[…, …] as const` on the tuple.

- **NOISE — `||` → `??` (`prefer-nullish-coalescing`), each domain-verified.** `filesBySid.get(id)
  || []` → `?? []` and `sparkBySid.get(id) || new Array(30).fill(0)` → `?? …` (a Map miss is
  `undefined`, never a falsy-but-present value). `c.sessions_json || '[]'` → `?? '[]'` (the column is
  NULL or a `JSON.stringify` array string, never `''`). `terminalSpawn`'s `q.getSpawn.get(id) ||
  null` → `?? null` (a hit is a row object, always truthy).

- **NOISE — corrupt-conflict JSON guard typing.** `let ids: unknown` for the `JSON.parse`; after the
  existing `if (!Array.isArray(ids)) return []` (Array.isArray narrows to `any[]`), re-bind
  `const sessionIds = ids as unknown[]` so the `sessionIds.map(id => callsignById.get(id as string)
  ?? id)` callback is not an unsafe-any pipeline (`no-unsafe-return`). Behavior identical: a non-
  string element still misses the map and falls back to its raw self, exactly as the `.mjs`.

- **NOISE — `fleetSize()` `.get().n` → `.get()?.n ?? 0`.** `countVisibleSessions.get()` is typed
  `CountRow | undefined`; a `COUNT(*)` always returns one row, so the `?? 0` is a dead type-guard,
  noted as such.

- **Types added:** `SnapshotCtx` for the threaded ctx (`q: Statements['q']` — the prepared-statement
  bundle only, matching the commands.ts/mail.ts idiom — plus the knob scalars, the `questions`
  relay, the `hasWatchWaiter`/`ownedPaneRow`/`spawnCapability`/`spawnState`/`resolveSettings`
  callbacks). `ResolvedSettings` models the one field snapshot reads back (`browse_root.resolved`)
  with an index-signature tail, since the object is otherwise shipped verbatim and settings.mjs is
  not yet converted.

- **Verify:** tsc `--noEmit` clean project-wide; `eslint snapshot.ts` clean (0). One importer
  repointed (`derive.mjs` `./snapshot.mjs` → `./snapshot.ts`). 168/168 green vs source across
  conflict, git-stderr-detail, shell-spawn, gateway, adopt, arm-gate, mail-and-blocking, plans,
  accept-plan-snapshot, repo-identity, dismiss, model-tracking, rename — covering every field the
  rewrite touched (spawn object incl. kind/fail_detail/revivable/gateway, adopt, the conflicts JSON
  guard, mail_meta/mail_pending, plans, repo_catalog, fleetSize/visible, stale/sparkline, callsigns).
  Daemon bundle rebuilds with the `snapshot.ts` banner (624.0kb) and passes `node --check`.

### scripts/fleetd/retention.mjs → retention.ts  [NOISE]

- **What:** The non-destructive retention engine (`presumeDeadSilent` silent-dead demotion,
  `retentionSweep` the archive/forget pass, `cleanup` the manual tmux-window reclaim, and the
  per-card `dismissSession`/`dismissRetry`) — ~549L. All of its cross-module deps were already
  `.ts` (`helpers.ts` `NOT_RESUMABLE_END`, `ledger.ts` `CONFLICT_WINDOW_MS`, `run-nonce.ts`
  `pruneRunNonces`). No runtime behavior changed; every edit was a type annotation, a
  compiler-equivalent narrowing, or a dead defensive wrapper the strict rules flag.
- **Types added:** `RetentionCtx` for the threaded ctx — `q: Statements['q']` (the prepared-statement
  bundle only, matching the mail.ts/snapshot.ts idiom) plus the `updateSession`/`tick`/`onMutate`/
  `tombstoneCard`/`forgetSpawn`/`adoptSession`/`scopedPaneTarget` callbacks, the `tmuxAdapter` and
  `questions` relays, the `port`/`home` scalars, and the four retain/presume knobs. Module-local
  interfaces (deliberately NOT shared — each converted module keeps its own, since spawn.mjs/derive.mjs
  are not yet converted and their true shapes are still `.mjs`): `TmuxWindow`, `PaneCommand`,
  `KillResult`, `KillOpts`, `TmuxAdapter`, `RetentionQuestions`, `AdoptResult`, `TombstoneOpts`.
  `SessionRow`/`SpawnRow` imported from `statements.ts`. Nested closures/functions annotated
  (`presumeDeadSilent(s: SessionRow, now)`, `retentionSweep(now = Date.now())`,
  `dismissSession(sid: string)`, `dismissRetry(sid: string)`, `incomplete(reason: string)`).
- **NOISE — `NOT_RESUMABLE_END.has(s.end_reason ?? null)` → `.has(s.end_reason)`.** `SessionRow.end_reason`
  is already `string | null` and `NOT_RESUMABLE_END` is `Set<string | null>` (helpers.ts:103), so the
  `?? null` coalesce is a provable no-op that `no-unnecessary-condition` flags. Dropping it is
  behavior-identical — the optimistic wrapper was defensive against a type that turned out already
  nullable.
- **NOISE — `.catch((err: unknown) => …)`.** The revive-adopt `.catch` param defaulted to `any`
  (`no-unsafe-member-access` on `err.message`); annotated `unknown` and narrowed with the house idiom
  `err instanceof Error ? err.message : String(err)` (matches exec.ts:145). Identical output.
- **NOISE — corrupt-conflict JSON typing.** `cleanup`'s stale-conflict prune parses `sessions_json`;
  `let parsed: unknown` for the `JSON.parse`, then `const ids: unknown[] = Array.isArray(parsed) ?
  (parsed as unknown[]) : []` (Array.isArray narrows `unknown`→`any[]`, so re-bind), and
  `alive.has(id as string)` per element. `row.sessions_json ?? '[]'` (`||`→`??`: the column is NULL or
  a `JSON.stringify` array, never `''`). Behavior identical to the `.mjs`.
- **NOISE — collection generics + tuple inference.** `new Map(q.allSpawns.all().map((r) => [r.tmux_window,
  r] as const))` (`as const` so the constructor accepts the tuple, not `(string|SpawnRow)[][]`);
  `new Set<string>()` for `reclaimed`; `const window_errors: string[] = []` (×3 spots); the spawned
  accumulator `const spawned: { s: SessionRow; sp: SpawnRow }[] = []`.
- **NOISE — `out.error ?? 'kill failed'` (×3) and `cur?.spawn_id !== sp.spawn_id`.** `killWindowVerified`'s
  `KillResult.error` is `string | undefined`, so `||`→`??` (`prefer-nullish-coalescing`); the
  `stillOurs` guard `!cur || cur.spawn_id !== sp.spawn_id` collapses to an optional chain
  (`prefer-optional-chain`) — a Map miss (`undefined`) `!== sp.spawn_id` is `true`, exactly the miss
  branch's original result.
- **NOISE — dropped three `Number()` wrappers.** `questions.expireAllForSession(…)` (×2) and
  `questions.purgeResolved()` are both typed `(): number` in questions.ts (verified: `expired`
  counter / `return Number(out.changes)`), so the outer `Number(…)` is `no-unnecessary-type-conversion`.
  The `Number(q.*.run().changes)` wrappers stay — `.changes` is `number | bigint` from the sqlite seam,
  where the coercion is real.
- **Verify:** `eslint retention.ts` clean (0). tsc `--noEmit` clean project-wide (0 errors) — the
  `commands.ts` regression that had been blocking project-wide green (committed at `dcb115c2` as a raw
  untyped rename despite its bug-log claiming tsc-clean) is now fixed in this same commit. One importer
  repointed (`derive.mjs` `./retention.mjs` → `./retention.ts`; the other four matches were prose
  comments). 64/64 green vs source across dismiss, daemon-maintenance, audit-cleanup, adopt. Daemon
  bundle rebuilds with the `retention.ts` banner (624.5kb) and passes `node --check`.

### scripts/fleetd/worktrees.mjs → worktrees.ts  [NOISE + one corrected assumption]

- **What:** The worktree-custody module (~559L) behind `GET /api/worktrees` (the bounded real-git-state
  inspector) and `POST /api/worktrees/remove` (allow-listed destruction with the BUG-059 ownership proof,
  BUG-060 per-path claim, and the CAS-against-inspected-tip branch delete). No runtime behavior changed;
  every edit is a type annotation, a compiler-equivalent narrowing, or a dead defensive branch the strict
  rules force. Cross-module deps were already `.ts` (`helpers.ts`, `exec.ts`, `payload-capture.ts`,
  `statements.ts`, `sqlite.ts`).
- **Types added:** `WorktreeItem` (the inspection record — note `base_is_local?: boolean` is optional, not
  nullable: it is written only on the no-remote fallback path), `WorktreeLastCommit`, `RefreshResult`
  (`{ok:true} | {ok:false; err}`), `RemoveBody` (all-`unknown` fields — untrusted JSON), `RemoveResult`
  (`{status; body: Record<string, unknown>}`), and `WorktreesCtx` — `q: Statements['q']` plus the
  optional `db?: SqliteHandle`/`acquireWorktreePathLock?`/`claimWorktreeCustody?` and the `tick`/`onMutate`
  callbacks (the same ctx idiom as mail/snapshot/retention). `WorktreeSpawnRow` exported from
  `statements.ts` (was module-private) and imported here for `worktreeRows`/`inspectWorktree`/`claimsPath`.
- **CORRECTED ASSUMPTION — `noUncheckedIndexedAccess` DOES add `undefined` to array *destructuring*.**
  Earlier migration notes claimed tuple/array destructuring was exempt (that `const [a, b] = someStringArray`
  binds plain `string`). It is not: `const [sha, subject, at] = log.out.trimEnd().split('\0')` binds each as
  `string | undefined`, and assigning `sha`/`subject` into the `string` fields of `WorktreeLastCommit` was a
  real `tsc` error (TS2322 ×2). Fixed with destructuring defaults on the two string bindings —
  `const [sha = '', subject = '', at] = …` — which is behavior-faithful (the `%h%x00%s%x00%ct` format
  guarantees three NUL-separated fields, so the defaults are unreachable) and leaves `at` untouched
  (`Number(at)` accepts `string | undefined`, so the timestamp coercion is byte-identical to the `.mjs`).
- **Nullable `worktree_path` / `cwd` from the row type.** `WorktreeSpawnRow.worktree_path` and `.cwd` are
  `string | null` even though `worktreeSpawns`' SQL filters `worktree_path IS NOT NULL`. `inspectWorktree`
  captures `const worktreePath = row.worktree_path` then early-returns the "gone" shell on
  `worktreePath == null || !exists` (narrows to `string` for the ~10 downstream `-C` argv uses — matches
  runtime, where `existsSync(null)` throws → caught → gone). `removeWorktree` threads
  `const worktreePath = body.path` after the `typeof body?.path !== 'string'` guard (provably equal to
  `row.worktree_path`, since rows are filtered on that equality). For `row.cwd`, a `row.cwd == null` guard
  returning `409 'main repository unavailable'` was added before the `git -C row.cwd rev-parse`: verified
  behavior-faithful by reading `exec.ts` — `execFileP` CATCHES the synchronous invalid-argv throw
  (exec.ts:144-146) and resolves `{ok:false}`, so the `.mjs`'s null-cwd path already produced the identical
  409/reason. In practice `cwd` is never null for a worktree spawn.
- **NOISE — dropped three `?? null` in `worktreeShell`.** `callsign`/`session_id`/`status` are copied
  straight from `WorktreeSpawnRow` into fields of the same nullable type (`callsign: string | null`,
  `session_id: string`, `spawn_status: string | null`), so the coalesces were `no-unnecessary-condition`.
  Identical output.
- **NOISE — `blocked[0].owner` → captured.** `noUncheckedIndexedAccess` makes `blocked[0]` possibly
  `undefined` even after a truthy `blocked.length` (length does not narrow the element), so
  `const firstBlocked = blocked[0]; if (firstBlocked) { … firstBlocked.owner … }` replaces `if (blocked.length)`.
  Same branch, same body.
- **NOISE — two `unknown`-catch errno guards.** `rmSync`'s catch used `err.code || err.message`; under
  `useUnknownInCatchVariables` this becomes
  `err instanceof Error ? ((err as NodeJS.ErrnoException).code ?? err.message) : String(err)` (`??` not `||`
  — errno codes are never `''`, so faithful). The purge-rollback catch's `err.message` similarly narrowed to
  `err instanceof Error ? err.message : String(err)`.
- **NOISE — `${branch}` in the tick string.** At the `tick(…)` call `branch` is `string | null`; guarded the
  interpolation as `branch_deleted && branch ? ` and branch ${branch}` : ''` so `branch` narrows to `string`
  inside the template. `branch_deleted` is only ever set true when `branch` was truthy, so this is
  behavior-identical (and sidesteps `restrict-template-expressions` on a nullish interpolation).
- **NOISE — empty-arrow fallback keeps a comment.** `releasePath = acquireWorktreePathLock ? … : () => {}`
  would trip `no-empty-function`; wrote the no-op as `() => {/* no path lock wired (direct-drive tests) */}`
  (the rule ignores a body containing a comment). Same no-op.
- **Verify:** `eslint worktrees.ts` clean (0). tsc `--noEmit` clean project-wide (0). One importer repointed
  (`derive.mjs` `./worktrees.mjs` → `./worktrees.ts`; the three other matches — in `repos.mjs`, `spawns.mjs`,
  and derive's own body comments — are prose) and the test import (`tests/worktrees.test.mjs`) repointed to
  `.ts`. 44/44 green vs source across worktrees, worktree-chmod-symlink, revive, takeover. Daemon bundle
  rebuilds with the `worktrees.ts` banner (625.8kb) and passes `node --check`.

### scripts/fleetd/files.mjs → files.ts  [NOISE + one behavior-faithful contract refinement]

Security-critical, read-only working-tree browser (fs/list, fs/read, fs/search for a session plus the
global browse root). The conversion is byte-faithful — no widening of what the endpoints expose — and all
the credential-denylist / `.git` / `fleetHomeReal` / realpath-containment walls and their verbatim
comments are preserved. Strict mode surfaced one genuine typing decision and a handful of noise.

- **`validateRelPath` void → returning the validated string (behavior-faithful refinement, NOT a behavior
  change).** In the `.mjs`, `validateRelPath(relPath)` was an assertion-style throw-or-return-void, and
  `listAt`/`readAt` did `try { validateRelPath(relPath); } catch { return failure } … safeJoin(root, relPath)`
  reusing the raw (now-validated) `relPath`. TS assertion narrowing does **not** survive a
  `try { assertFn(x) } catch { return }` boundary — after the catch, `relPath` is still `unknown`, so every
  downstream `entryPath(relPath, …)` / `path: relPath` use fails `noImplicitAny`/type errors. Fix: give the
  validator a return value (`validateRelPath(relPath: unknown): string`) and bind it
  (`let rel: string; try { rel = validateRelPath(relPath); } catch (err) { return failure(err); }`); the
  rest of the scope then uses the definitely-`string` `rel`. Runtime is identical (the returned string IS
  the input `relPath` that already passed every check) and the two throwing call sites — `safeJoin` and the
  `listAt`/`readAt` heads — are unchanged in what they reject.
- **`noUncheckedIndexedAccess` on the git-grep regex groups.** `parseGitGrep` did `match[1]`/`match[2]`/
  `match[3]` (each `string | undefined` on a `RegExpExecArray`). Rewrote as
  `const [, file = '', lineStr = '', text = ''] = match;` — a successful `/^(.*?):([0-9]+):(.*)$/` match
  guarantees all three groups, so the destructuring defaults are unreachable. `perFile.get(file) || 0`
  became `?? 0` (`prefer-nullish-coalescing`; a count is never `0`-as-falsy-sentinel here anyway).
- **`noUncheckedIndexedAccess` in the walk loop.** `names[i]` (`string | undefined`) guarded with
  `if (name === undefined) continue;`; `stack.pop()` (`T | undefined`) guarded with `if (!current) break;`;
  the content-scan `lines[line]` captured as `const lineText = lines[line]; if (lineText === undefined) …`
  before the two `.toLocaleLowerCase()`/`.replace()` uses. All guards are on branches the runtime never
  reaches (bounded `for` indices, non-empty stack), so behavior is unchanged.
- **`ignored.has(rels[i])` → `ignored.has(rels[i] ?? '')`.** `rels[i]` is `string | undefined` under
  `noUncheckedIndexedAccess`; `.has` wants `string`. `rels` is `entries.map(…)` so it is 1:1 with `entries`
  and the index is always in-range — the `?? ''` is unreachable. Also rewrote the index `for` as
  `entries.forEach((entry, i) => …)` to bind `entry` without a second `entries[i]` index read.
- **`no-unnecessary-type-conversion` — dropped two `String(...)`.** `deniedName(name: string)` and
  `deniedRelPath(rel: string)` had `String(name)`/`String(rel)` (defensive in the untyped `.mjs`); with the
  params now typed `string` the wrappers are provably no-ops. Removed. Same output.
- **`process.env.FLEETDECK_HOME` → `process.env['FLEETDECK_HOME']` + `??`.** `noPropertyAccessFromIndexSignature`
  forces bracket access on `process.env`; the `|| path.join(…)` fallback became `??`
  (`prefer-nullish-coalescing`; `FLEETDECK_HOME` set-to-empty-string is not a case we distinguish from
  unset, and both fall through to the default — faithful).
- **`no-useless-assignment` — dropped one `stop = true`.** `walkSearch`'s top-of-`while` deadline check did
  `truncated = true; stop = true; break;`, but that `break` exits the `while` directly and `stop` is only
  read by the `while` condition, so the assignment was dead. Removed it (added a comment) — the **inner-loop**
  `stop = true; break;` sites are kept, because they exit only the `for` and rely on the `while` re-check.
- **`require-await` on `readAt` — SUPPRESSED on purpose (runtime contract).** `readAt` does only synchronous
  fs work, so eslint wants `async` dropped — but `http.mjs` dispatches all three fs operations uniformly as
  `operation.then(...).catch(...)` (http.mjs:842), so a non-Promise return would crash on `.then`. Kept the
  function `async` (identical to the `.mjs`) with a one-line
  `// eslint-disable-next-line @typescript-eslint/require-await` and a comment explaining the caller
  contract. This is the one place the lint rule and the real behavior disagree; behavior wins.
- **Typedefs added (no runtime effect):** `FilesCtx` (threads `q: Statements['q']` + `browseRootChoice`),
  `FsResult`, a discriminated `RootResolution` (`{ error } | { root, git }`) + `RootResolver` thunk,
  `SearchMode`/`SearchHit`, `RunOptions`/`RunResult`, `OpenedFile` (a `notFile` | `buf` union), and a
  `DirEntry` for the listing rows. `runBounded`'s `spawn` catch narrowed to
  `error instanceof Error ? error.message : String(error)`; the stream handlers typed `(chunk: Buffer)` and
  `child.stdout?/stderr?/stdin?` optional-chained (they are `| null` on the `ChildProcess` type); dropped
  the now-unnecessary `timer.unref?.()` `?.` (`unref` is always present on a `Timeout`).
- **Verify:** `eslint files.ts` clean (0). tsc `--noEmit` clean project-wide (0). Both importers repointed
  (`derive.mjs` `./files.mjs` → `./files.ts`; `tests/session-fs.test.mjs` `../scripts/fleetd/files.mjs` →
  `.ts`). 16/16 green in `session-fs.test.mjs` (session + home/browse-root fs, credential denylist,
  BUG-114/115/116, LAN token walls). Daemon bundle rebuilds with the `files.ts` banner (626.2kb, no stale
  `files.mjs` banner) and passes `node --check`.

### scripts/fleetd/settings.mjs → settings.ts  [NOISE + one cross-module type widening]

The whitelisted durable-settings surface (repos_dir/transport/default_org, browse_root, fav_dirs,
repo_setup(+patch), hold_ms, and the credential-bearing gateway_* profile). SECURITY-CRITICAL: `gateway_token`
is a live credential with exactly one reader (`resolveGatewayEnv`, the spawn path); `resolveSettings`/
`resolveGateway` serve only `token_set: true`. The conversion is behavior-faithful — no widening of what any
endpoint exposes — and every masking / BUG-147 / BUG-047 / path-gate comment is preserved verbatim.

- **`namedError(status, message)` → a `SettingError extends Error { readonly status: number }` class.** The
  `.mjs` did `const e = new Error(msg); e.status = status; return e;` — under strict, a plain `Error` has no
  `.status` property to assign, and every `catch` reads it back off an `unknown`. A one-field subclass carries
  the tag with a real type. `namedError` is kept as a thin constructor wrapper so the ~30 throw sites are
  untouched. Same runtime shape (an `Error` with a numeric `.status`).
- **`errStatus(err: unknown)` / `errMessage(err: unknown)` helpers for `useUnknownInCatchVariables`.** Every
  `catch (err)` now sees `err: unknown`. Two spots read the status tag: the two `if (err?.status) throw err;`
  re-throws inside `validatePathSetting` (→ `if (errStatus(err)) throw err;`) and `setSettings`' outer
  `const status = err.status || 500` (→ `errStatus(err) ?? 500`); the message read `err.message || String(err)`
  → `errMessage(err)`. Behavior identical — a tagged 400 stays a 400, an untagged storage failure stays 500.
- **The `HANDLERS` correlated-union dispatch (the one real type-design problem).** `setSettings` indexes
  `HANDLERS[k]` by a runtime string `k` and calls `.prepare(body[k])` then `.commit(prepared)`. A naive object
  literal makes each entry a *different* `{prepare,commit}` type, so the union at the index site collapses
  `prepare`'s param to `never` and `commit`'s to the intersection — uncallable. Resolved with a
  `SettingHandler<T>` interface declared in **method syntax** (`prepare(value): T; commit(prepared: T): void`),
  which opts each handler into parameter *bivariance*, so a concrete `SettingHandler<string[]>` upcasts to
  `SettingHandler<unknown>` at the dynamic-dispatch site. An identity helper `defineHandler<T>` fixes each T
  from its literal (the bodies keep precise types); `handlerFor(k): SettingHandler<unknown> | undefined` is the
  one widening cast (`HANDLERS as Record<string, SettingHandler<unknown>>`), justified because `setSettings`
  already proved `k` is whitelisted. This keeps every credential-carrying `commit` body cast-free.
- **~5 pass-through `prepare` casts `return v as string | null`.** The transport/default_org/gateway_bool
  handlers validate-for-throw (`if (v != null && v !== 'ssh' …) throw`) then persist the raw value. After the
  throw, `v` is still typed `unknown`, so the return needs an explicit `as string | null`. Each cast is
  immediately preceded by the validator that makes it sound — no new trust, the `.mjs` did the same narrowing
  implicitly.
- **Type-predicate filters on the guarded parses.** `fav_dirs`: `.filter((v): v is string => typeof v === 'string')`.
  `repo_setup`: `.filter((e): e is [string, string] => typeof e[0] === 'string' && typeof e[1] === 'string')`
  over `Object.entries`. These make the resolvers return `string[]` / `Record<string,string>` instead of
  `unknown[]` — same defensive drop-the-junk behavior the `.mjs` had, now with a type to show for it.
- **`process.env.X` → `process.env['X']`** (`noPropertyAccessFromIndexSignature`) at the two env reads
  (`FLEETDECK_BROWSE_ROOT`, `FLEETDECK_HOLD_MS`), and the gateway env map is a `Record<string, string>` written
  by bracket key (`env[auth_style === 'api-key' ? 'ANTHROPIC_API_KEY' : 'ANTHROPIC_AUTH_TOKEN'] = token`,
  `env['CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY'] = '1'`). `resolveGatewayEnv` is typed
  `Record<string, string> | null` — the `null` (not-fully-configured) branch is unchanged.
- **`no-control-regex` (2×) suppressed on purpose.** `CONTROL_RE` / `SETUP_CONTROL_RE` embed C0/DEL ranges —
  refusing those bytes in path & credential values is the entire point of the gate. Same
  `// eslint-disable-next-line no-control-regex` pattern already used in `mail.ts` / `exec.ts`.
- **`no-dynamic-delete` suppressed once.** `delete merged[name]` on the `__delete` tombstone path of
  `repo_setup_patch` — the dynamic key is the whole mechanism of the BUG-147 read-merge-write; disabled with a
  one-line justification rather than rewritten (a filter-rebuild would change nothing but obscure the intent).
- **`no-useless-assignment` (2×) — dropped redundant catch reassignments.** `browseRootChoice`'s
  `let home = null; try { home = os.homedir() } catch { home = null }` and `validateFavDirs`'
  `let isDir = false; … catch { isDir = false }` each re-assigned the initializer in the catch, so the linter
  saw the initial value as dead. Emptied the catch bodies (kept the initializer, which is now genuinely the
  throw-path value) — behavior identical.
- **CROSS-MODULE: widened `questions.ts` `resolveHoldMs`'s `fallback` param.** `settings.ts`'
  `resolveHoldMsRaw` reads the `hold_ms` k/v **row**, which comes back `string | null`, and passes it as the
  fallback thunk. `questions.ts` had typed that param `(() => number | null | undefined) | null`, but the
  function's own body already `Number(fallback?.())`-coerces it — so the string return was always fine at
  runtime; only the type was too narrow. Widened to `(() => number | string | null | undefined) | null` with a
  comment naming `resolveHoldMsRaw` as the real caller. This is the one genuine strict-typing find of the
  file: a pre-existing type that under-described a value the function already handled. `derive.mjs`'s existing
  `() => ctx.resolveHoldMsRaw?.() ?? null` caller is unaffected (still fits the wider signature).
- **Typedefs added (no runtime effect):** `SettingChoice` (`{ value: string | null; source: string }`),
  `SettingsCtx` (the `db: SqliteHandle | null` + `q: Statements['q']` + repos-provider dependency surface
  derive.mjs hands in), and the `SettingHandler<T>` interface above. `setSettings(body: unknown)` narrows via
  `const record = body as Record<string, unknown>` after the object/array guards.
- **Verify:** `eslint settings.ts questions.ts` clean (0). tsc `--noEmit` clean project-wide (0). Both
  importers repointed (`derive.mjs:19` `./settings.mjs` → `./settings.ts`; `settings-transaction.test.mjs:22`
  `../scripts/fleetd/settings.mjs` → `.ts`). 6/6 green across `settings-transaction.test.mjs` (BUG-047 atomic
  rollback) + `smoke-settings.test.mjs`; 40/40 green in `derive-audit-reliability` + `agents-ingest` (derive
  integration through the repointed import). Daemon bundle rebuilds with the `settings.ts` banner (627.5kb, no
  stale `settings.mjs` banner) and passes `node --check`.

### scripts/fleetd/termbridge.mjs → termbridge.ts  [NOISE + one corrected @types/node model]

The tmux control-mode bridge: one shared control client per port, byte-exact `%output` demux, viewer
lifecycle. All doctrine comments (CONTRACT, WHY ONE CLIENT, keystroke doctrine, M-R4/R5/P6, R1-4, H-R3,
LATIN-1, CRLF, BUG-055/056/158/159) preserved verbatim. Every finding is strict-typing noise except one
correction to how `@types/node` types a `spawn`'d child — worth recording because it inverted an assumption.

- **CORRECTED: `spawn` with a `stdio` TUPLE returns NON-NULL streams.** I expected `child.stdout`/`.stderr`/
  `.stdin` to be `Stream | null` and reached for `?.`. eslint's `no-unnecessary-condition` flagged all of them.
  Root cause: `spawn(cmd, args, { stdio: ['pipe','pipe','pipe'], windowsHide: true })` — the 3-tuple literal
  selects `@types/node`'s `SpawnOptionsWithStdioTuple` overload, which returns
  `ChildProcessByStdio<Writable, Readable, Readable>` with the three streams typed **non-null**. So the local
  `const child = spawn(...)` uses `child.stdout.on(...)` / `child.stderr.on(...)` with NO `?.`. The nullability
  lives only on the `Client.child` FIELD (typed `ChildProcess | null` because it starts null and is set later):
  the teardown kill-guard is `if (c.child?.exitCode === null && !c.child.killed)`, and the write path narrows
  through the R-guard `if (c.closed || !c.child?.stdin?.writable) { reject; return; }` so `c.child.stdin.write`
  is cast-free after it. Behavior identical — this only removed dead `?.` the runtime never exercised.
- **`.unref()` is always present — dropped `?.` (2x).** `setTimeout`/`setInterval` return
  `ReturnType<typeof setTimeout>` = `NodeJS.Timeout`, whose `.unref()` is non-optional under `@types/node`.
  `timer.unref?.()` → `timer.unref()` (close-recheck timer) and `t.unref?.()` → `t.unref()` (repaint delay),
  plus `c.deadTimer.unref()`. `no-unnecessary-condition` again — the guards were defensive against a
  browser-`setTimeout` shape that never applies in the daemon.
- **`no-empty-function` — comment bodies for noop arrows (3x).** `log = () => { /* silent by default */ }`,
  `onClose = () => { /* no-op by default */ }`, and the `readyResolve`/`readyReject` `let` initializers
  (`() => { /* replaced by the executor below */ }`). Matches the house pattern already in the file
  (`ready.catch(() => { /* ... */ })`). A param-less arrow stays assignable to the typed callback signatures.
- **`no-unnecessary-type-conversion` — dropped `String()` (2x).** In `unescapeControlData(value: string)` and
  `ControlModeParser.feed(chunk: … )` the inputs are already `string` on the text path, so `String(value)` /
  `String(chunk)` were redundant. (`String(chunk)` in the stderr log KEPT — there `chunk` is a `Buffer`, so it
  is a real conversion, not a no-op.)
- **`??=` for lazy-init (`prefer-nullish-coalescing`).** `ensureClient`'s `if (!client) client = createClient()`
  → `client ??= createClient()`. Same once-only attach.
- **`prefer-optional-chain`.** teardown's `if (c.child && c.child.exitCode === null …)` →
  `if (c.child?.exitCode === null …)`.
- **`||` KEPT (2x) with a one-line disable.** `exit[1] || 'tmux control client exited'` and
  `override || 'tmux'` are deliberately falsy-coalescing (an empty capture / empty env override must fall
  through to the default), so each carries `// eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing`
  rather than a semantics-changing `??`.
- **`noUncheckedIndexedAccess` on regex captures → `?? ''`.** The control-line parser's `match[1]`/`match[2]`
  are `string | undefined`; each is coalesced to `''` (block time/number) or fed through `Number(match[n])` for
  the cursor-position init. `cursor.lines.at(-1)?.trim() ?? ''` for the last-line probe.
- **`noPropertyAccessFromIndexSignature` → bracket env reads.** `process.env['FLEETDECK_TERM_CMD']`,
  `process.env['FLEETDECK_TERM']`.
- **`useUnknownInCatchVariables`.** `openViewer`'s terminal-open catch narrows
  `const detail = err instanceof Error && err.message ? err.message : 'terminal open failed';`.
- **Escape-sequence hazard (tooling, not TS).** The ESC bytes in `CLEAR_SCREEN` and the cursor-home init
  string were silently rewritten to literal ESC (0x1b) by the Write tool; caught with `grep … | cat -v` (showed
  `^[`) and restored to the `^[` source form with perl. No behavior change, but the committed source now
  reads as an escape, not a raw control byte.
- **Typedefs added (no runtime effect):** `ControlEvent` discriminated union
  (`Response|Output|Exit|WindowClose|SessionChanged`), and the `Waiter`/`PaneStream`/`Client`/`Viewer`/
  `TermFrame`/`TermSend`/`OpenViewerOptions`/`ViewerHandle`/`TermBridgeOptions` interfaces. `SpawnRow` imported
  from `statements.ts` for the `resolveSpawn` result (`status`/`tmux_session`/`tmux_window`). `command` (arrow)
  and `deadTimer` (`setInterval`) live in the `const c: Client = {…}` literal via closures referencing `c`.
- **Verify:** `eslint termbridge.ts` clean (0). tsc `--noEmit` clean project-wide (0). 3 importers repointed
  (`http.mjs:22`, `termbridge-parser.test.mjs:4`, `termbridge-capture-race.test.mjs:6` → `./termbridge.ts`).
  24/24 green across `termbridge-parser` + `termbridge-capture-race` (BUG-056 same-chunk replay) +
  `terminal-ws` (live WS lifecycle). Daemon bundle rebuilds with the `termbridge.ts` banner (628.7kb) and
  passes `node --check`.

### scripts/fleetd/repos.mjs → repos.ts  [NOISE + 2 genuine lint findings only surfaced once linted]

Durable repo catalog/settings + clone/branch materialization for repo-mode spawns. All credential-scrubbing
doctrine preserved verbatim (originSecrets ≥8-char needle filter + accepted sub-8 residual; gitFailureText's
"ONE hardening pass, BOTH outputs"; recordFailDetail's "must never DISPLACE the diagnostic"; cloneRepo's
0600-fleetd.log residual note + argv-safety re-gate; normalizeRemoteOrigin's "three doors into ONE repository"
forge-unification + generic-host conservatism; the CVE-2017-1000117-class `@`-split; materializeBranch's
redactGitText-vs-scrubUrlCredentials rationale). Every finding below is strict-typing/lint noise except two
lint rules that only fire now the file is type-linted (it was eslint-ignored as `.mjs`).

- **`no-control-regex` (2x) — control-char class regexes need an explicit disable.** `CONTROL_RE`
  (`/[\x00-\x1f\x7f]/`) and `SPACE_OR_CONTROL_RE` (`/[\s\x00-\x1f\x7f]/`) match C0/DEL **on purpose** — that IS
  the argv/path-safety gate. As `.mjs` they were never linted; under the TS ruleset each takes a
  `// eslint-disable-next-line no-control-regex -- …purpose of this gate` line, the exact house pattern already
  in `settings.ts:44-46`, `exec.ts:260`, `mail.ts:109-147`.
- **`no-useless-assignment` — dropped a dead `= null` seed in `normalizeRemoteOrigin`.** `let user: string |
  null = null;` then BOTH the url and scp branches assign `user` before it is read at the final return, so the
  `= null` initializer's value is never observed. Changed to an uninitialized `let user: string | null;` (TS
  definite-assignment is satisfied — every path to the read assigns it — matching the sibling `let host:
  string; let rest: string;` already there). Pure lint; runtime identical.
- **`no-unnecessary-condition` — `!last.ok` was provably redundant after the worktree-add loop.** In
  `materializeBranch`'s final throw, `redactGitText(last && !last.ok ? last.err : null)` — TS narrows `last`
  (`ExecResult | null`) to the **failure variant** `{ ok: false } | null` on loop exit, because a successful
  attempt `return`s from inside the loop, so `!last.ok` is always true when `last` is non-null. Simplified to
  `last ? last.err : null` (`last` stays genuinely nullable — the loop body can run zero times if `candidates`
  is empty, so the outer check is NOT redundant). Behaviourally identical; the `|| 'git worktree add failed'`
  falsy-coalescing default is preserved with its disable comment.
- **Error plumbing: `RepoError extends Error` replaces `namedError`'s Error-with-`.status` stamp.** The old
  `const e = new Error(msg); e.status = status` is a strict-TS type error (`Property 'status' does not exist on
  type 'Error'`). A 4-line subclass carries `readonly status: number` as a typed field; `namedError(status,
  message)` is unchanged at every call site. `errStatus(err: unknown): number | undefined` /
  `errMessage(err: unknown): string` read a `useUnknownInCatchVariables` catch value back — the same helper pair
  used in `settings.ts`, so the two surfaces stay behaviourally paired ("already has a status → it's ours,
  rethrow" and `err.message || String(err)`).
- **Dead-defensive `String()` / `|| ''` removals (typed-string inputs).** `String(err ?? '')` in
  gitFailureText, `String(value)` in repoNameOf / unsafeDashSegment, `String(porcelain || '')` in parseWorktrees
  / dirtyNames, `String(spawn_id)` in cloneRepo, `String(sid)` in materializeBranch, `String(origin_url ?? '')`
  → `origin_url ?? ''` in originSecrets — every one had a parameter already typed `string`, so both the
  conversion and the `|| ''`/`?? ''` null-guard were `no-unnecessary-type-conversion` / dead code. (No
  `String(buffer)` existed here to keep — unlike termbridge.)
- **`!!clone` → `clone` (3x, `no-unnecessary-type-conversion`).** `materializeBranch`'s `created: { clone: !!clone }`
  — `clone` is a typed `boolean` (defaulted `clone = false`), so the double-negation is a no-op.
- **`noUncheckedIndexedAccess` on regex captures & array indices.** `ORIGIN_USERINFO_RE.exec(origin)?.[1] ?? ''`;
  `match[1] ?? ''` in the `matchAll` loop; `parts[parts.length - 1] ?? ''` fed to repoNameOf; `url[1]/url[3]`,
  `scp[1]/scp[3]` captured into locals and `?? ''`-defaulted (with `h = url[2]; if (h === undefined) return
  null` narrowing the host); `roots[0]` captured as `const soleRoot` and `!== undefined`-narrowed; the
  case-fold `.exec(origin)?.[0]` guarded with `!== undefined`.
- **`noPropertyAccessFromIndexSignature` → bracket env reads.** `process.env['FLEETDECK_REPOS_DIR' /
  '_CLONE_CONCURRENCY' / '_CLONE_TIMEOUT_MS' / '_DEFAULT_ORG']`.
- **Kept `||` falsy-coalescing (with per-line disables), NOT `??`.** `user = url[1] || null` / `scp[1] || null`
  (an empty userinfo capture must fall to null, not be kept as `''`); `result.out.trim() || null` in originOf;
  `result.err || 'default'` messages (empty stderr → default); `sid.slice(0,4) || 'repo'`; the four
  `redactGitText(x.err) || 'git … failed'` note defaults. Each is deliberate `''`→default coalescing that `??`
  would silently change.
- **`requireBaseRef` closure — a non-null invariant TS cannot carry to the argv literals.** The compound guard
  `if (!local.ok && !remote.ok && !base) throw` proves `base` non-null on the branch-create paths, but TS does
  not propagate that to the `['…','switch','-c',branch, base.ref]` / `['…','worktree','add','-b',branch,
  candidate, base.ref]` literals three statements later. A `const requireBaseRef = () => { if (!base) throw …;
  return base.ref; }` closure over the `const base` re-asserts it at each use (the throw is unreachable at
  runtime once the guard has run). Same shape used elsewhere for post-guard invariants TS can't see.
- **Typedefs added (no runtime effect):** discriminated `RepoParseResult = RepoParseError | RepoParsed`
  (callers narrow via `'error' in parsed`) and `ResolvedTarget = {mode:'local'…} | {mode:'clone'…}`; interfaces
  `RepoParsed`/`ReposCtx`/`ResolveTargetBody`/`TouchRepoArgs`/`WorktreeEntry`; `type RepoKind`. `ReposCtx { q:
  Statements['q'] }` — `Statements = ReturnType<typeof build>` and `build()` returns `{ q, FIELDS,
  updateSession }`, so `Statements['q']` is the prepared-statements bag; `createRepos(ctx)` receives the full
  derive.mjs ctx, which structurally satisfies `{ q }`. `ExecResult` imported for `let last: ExecResult | null`.
  `resolveTarget`'s `body.repo_host ?? undefined` maps an explicit null to undefined so parseRepoInput's
  `'github'` default applies; `validateRepoDefaultOrg(value: string | null)` needs no cast (its only caller
  passes a `!= null`-narrowed string).
- **Verify:** `eslint repos.ts` clean (0). `tsc --noEmit` clean project-wide (0). 2 importers repointed
  (`derive.mjs:18`, `tests/repos.test.mjs:8` → `./repos.ts` / `../scripts/fleetd/repos.ts`). 36/36 green in
  `tests/repos.test.mjs` (incl. BUG-044 full-redaction-pass on worktree-add stderr). Daemon bundle rebuilds with
  the `// scripts/fleetd/repos.ts` banner (630.5kb) and passes `node --check`.

### scripts/fleetd/mdns.mjs → mdns.ts  [NOISE + 2 genuine lint findings + 1 Write-hazard caught]

A dependency-free mDNS (RFC 6762) + DNS-SD (RFC 6763) responder: a hand-rolled DNS wire codec over one udp4
socket. All doctrine preserved verbatim — the "mDNS is a CONVENIENCE, never a dependency" degrade-safely contract
(every failure logged once, module goes no-op, daemon untouched); the RFC choices block (§12 additionals, §10.2
cache-flush only on records we own, §6.7 legacy unicast, §8.3 optimistic-announce with reactive §9 conflict
handling); the per-interface scoped egress rationale (BUG-130/131 — a link advertises only its own A record). Two
lint rules fire only now the file is type-linted (it was eslint-ignored as `.mjs`), plus the escape-byte Write
hazard the migration has hit before.

- **Escape/control-byte Write hazard — caught via `cat -v`, fixed with a raw perl edit.** `label()`'s two regex
  literals — the DNS-label safety class `/[.\x00-\x1f\x7f]/g` (strips dot/C0/DEL) and the truncated-multibyte
  strip `/�+$/` — were authored with `\uXXXX` escapes, but the JSON tool-arg layer DECODED those escapes into
  RAW control bytes (0x00, 0x1f, 0x7f) and the raw U+FFFD glyph inside the written file. Semantically identical to
  the original (the char class is still dot + C0 range + DEL; the strip is still U+FFFD), so behaviour and tests
  were unaffected — but raw control bytes in source are fragile (editors/diffs/terminals mangle them), which is
  exactly why the original used escapes. Restored the escape form via `perl -i -pe` (raw Bash edits skip the
  formatter, and hex-match the bytes without typing them). Verified `cat -v` clean and `grep -aP` finds no raw
  C0/DEL/FFFD bytes remaining in either the source OR the regenerated bundle.
- **`no-control-regex` — `label()`'s control-char class needs an explicit disable.** `/[.\x00-\x1f\x7f]/g`
  matches C0/DEL **on purpose** — that IS the gate that keeps control bytes off the DNS wire. As `.mjs` it was
  never linted; under the TS ruleset it takes a `// eslint-disable-next-line no-control-regex -- …purpose` line,
  the house pattern already in `settings.ts:44-46`, `exec.ts:260`, `mail.ts`, `repos.ts`. ESLint detects the
  escape form too, so the disable stays "used". (The sibling `/�+$/` strip needs NO disable — U+FFFD is a
  printable replacement glyph, not a control char.)
- **`no-unnecessary-condition` (3x) — Set-based dedup one-liners were provably always-truthy.** `records.filter(r
  => !seen.has(keyOf(r)) && seen.add(keyOf(r)))` (once in buildAnnouncement, twice in buildResponse) used
  `Set.add()` returning the Set (always truthy) as the "keep" signal. TS proves the `&&` RHS is always truthy →
  the short-circuit is dead code. Converted each to an explicit-predicate block (`const k = keyOf(r); if
  (seen.has(k)) return false; seen.add(k); return true;`). Behaviourally identical, and the dedup key is now
  computed ONCE per record instead of up to three times.
- **`prefer-nullish-coalescing` — `ttlFor`'s `override === undefined ? DEFAULT_TTL[type] : override` IS `??`.**
  `override` is `number | undefined`; the ternary is exactly `override ?? DEFAULT_TTL[type]`. Simplified. This is
  also MORE correct than a `||` would have been: 0 is a legal TTL (a goodbye), and `??` preserves it where `||`
  would swallow it back to the default.
- **`setTTL?.` is load-bearing — `no-unnecessary-condition` disabled INLINE (survives prettier reflow).**
  `@types/node` types `dgram.Socket.setTTL(ttl: number): number` as NON-OPTIONAL, so on the narrowed `sock:
  Socket` TS proves `sock.setTTL?.` can never short-circuit → "unnecessary optional chain". But the injected test
  socket (`inject.dgram`'s MockSocket) OMITS setTTL, and the `?.` lets it degrade instead of throwing — the seam
  the mdns-dgram-loader tests drive. Kept with a disable. First attempt used `// eslint-disable-next-line`, but
  prettier split the one-liner `try { sock.setTTL?.(255); } catch {…}` across lines, which detached the directive
  from its target (it then pointed at `try {`) and `--report-unused-disable-directives` deleted it as unused.
  Fixed by moving to an inline `// eslint-disable-line` on the `sock.setTTL?.(255);` statement itself, which
  survives the reflow.
- **`noUnusedParameters` — `ptrRecord`'s leading `ad` param, kept for builder symmetry, prefixed `_ad`.** A PTR
  names the service type, not the host, so ptrRecord never reads `ad` — but the sibling builders
  aRecords/srvRecord/txtRecord all take `ad` first and both call sites pass it positionally. eslint's
  `no-unused-vars` default (`args:'after-used'`) does NOT flag a leading unused param (which is why eslint passed),
  but tsc's `noUnusedParameters` flags ANY unused param regardless of position unless `_`-prefixed. Renamed to
  `_ad` to keep the symmetric call shape.
- **`sendRaw` callback signature simplified under strict typing.** The original forwarded the send error to its
  optional callback (`callback?.(err)`); every caller (settled/done) is a zero-arg `() => void` that ignores it,
  and in the catch path `err` is `unknown` (uncatchable-typed, not assignable to any error param). Narrowed the
  callback to `() => void` and call `callback?.()` — the per-packet error is still logged via note(); only the
  dead err-forwarding to a callback nobody reads was dropped.
- **Dead-defensive removals (typed inputs).** `String(value ?? '')` → `(value ?? '')` in label();
  `encodeTxt`'s `... || {}` guard dropped (param typed `string[] | Record<string,string>`); `normalize`'s
  `...(options.txt || {})` → `...(options.txt ?? {})`. `addressInterfaces` dropped the legacy `|| entry.family
  === 4` numeric branch — `os.NetworkInterfaceInfo.family` is typed `'IPv4' | 'IPv6'` (a string union), so the
  numeric compare was statically dead; kept `entry.family === 'IPv4'`. (KEPT `String(options.port)` — port is
  `number | string | undefined`, a real conversion — and the two `String(type)` fallbacks — type is number.)
- **Kept `||` falsy-coalescing (per-line disables), NOT `??`.** `normalize`'s `options.host || …` / `options.instance
  || 'Fleet Deck'` / `Number(options.port) || 0` (empty string / NaN / 0 must fall through); label()'s `text ||
  fallback` and `bytes.toString() || fallback` (an empty label must fall back); `encodeMessage`'s `q.class ||
  CLASS_IN` (class 0 = unspecified → IN); `encodeRecord`'s `Number(record.ttl) || 0` (NaN guard); the
  `TYPE_NAME[type] || String(type)` fallbacks. Each is deliberate `''`/NaN/0→default coalescing that `??` would
  silently change.
- **`noUncheckedIndexedAccess` on buffer/array indices throughout the codec.** `decodeName`: `const len =
  buf[pos]; if (len === undefined) throw new RangeError(…)` (replacing the old bounds-check-then-index), and the
  compression pointer's `const next = buf[pos + 1]; if (next === undefined) throw…`. `decodeRecords`' TXT loop:
  `const len = buf[p] ?? 0`. `parseQuestions`/`decodeRecords`/`decodeMessage` use `let decoded: { name: string;
  offset: number }` for the try/catch-assign of `decodeName`. All `readUIntBE`/`toString` calls stay range-guarded
  before use.
- **`useUnknownInCatchVariables` — `errMessage(err: unknown): string` module helper.** `err instanceof Error &&
  err.message ? err.message : String(err)` — used in sendRaw/announce/onMessage/update catch clauses (the same
  helper shape as settings.ts/repos.ts). `die()` inlines the `err instanceof Error && err.message` check to build
  its `: <detail>` suffix.
- **Closure-mutated `socket: Socket | null` → capture-after-guard.** `socket` is reassigned across the responder's
  life (start/die/stop), so TS narrowing evaporates after any intervening call. Every consumer captures `const
  sock = socket; if (!sock) return;` before use (sendRaw/sendMulticastAll/onBound/withdrawAndDie/update/stop),
  matching the pattern used for other closure-mutated lets in the migration. `alive()` returns `started && !dead
  && socket !== null`.
- **Typedefs added (no runtime effect):** wire-model interfaces (SrvData/DnsRecord/Question/OutQuestion/DnsHeader/
  DecodedMessage/EncodeMessageInput/AdService/Advertisement) and the responder surface
  (MdnsOptions/CreateMdnsOptions/MdnsResponder); `RecordData` union covers every rdata shape the codec
  emits/parses. `TYPE` stays a plain object literal (no index signature) so `.A`/`.ANY` dot-access is legal under
  noPropertyAccessFromIndexSignature; `TYPE_NAME: Record<number,string>` is built via `Object.fromEntries(Object.
  entries(TYPE).map(([k,v]): [number,string] => [v,k]))` with the tuple return annotated to defeat array-vs-tuple
  inference; `typeNumber` reads `(TYPE as Record<string, number|undefined>)[type.toUpperCase()]` and throws on
  undefined. `import type { RemoteInfo, Socket }` alongside the default `dgram` import.
- **Verify:** `eslint mdns.ts` clean (0). `tsc --noEmit` clean project-wide (0). 4 importers repointed
  (`scripts/fleetd/fleetd.mjs:22` → `./mdns.ts`; `tests/mdns.test.mjs:36` and `tests/fleetd-audit-regressions.test.mjs:7`
  → `../scripts/fleetd/mdns.ts`; loader `tests/helpers/mdns-dgram-loader.mjs:14` & `:20` `.mjs → .ts`;
  `tests/lan-mdns-state.test.mjs` does NOT import mdns → untouched). 53/55 green across mdns.test.mjs +
  fleetd-audit-regressions.test.mjs + lan-mdns-state.test.mjs — the 2 SKIP are the tests' OWN WSL2
  multicast-loopback guards ("cannot observe probes on this host"), identical to the `.mjs` run; **# fail 0**.
  Daemon bundle rebuilds with the `// scripts/fleetd/mdns.ts` banner (631.9kb), passes `node --check`, and carries
  no raw control/FFFD bytes.

### scripts/fleetd/events.mjs → events.ts  [NOISE + 3 genuine lint findings]

The hook state machine: `applyEvent` (the faithful port of the spike's derivation switch) plus the eight hook
endpoints that wrap it — SessionStart brief, UserPromptSubmit, Pre/PostToolUse whisper, Stop mail-block, SessionEnd
tombstone, and the F3a/b/c hold-open intake with v1.3 plan capture. All doctrine preserved verbatim
(BUG-024/025/034/102/104/112/122/166/204, the M-B1/B2/B5/B6/G2 correlation invariants, the F1/F3a–e/F4 needs-you
machinery, the 0.2.0→0.7.1 /clear-succession comments, and the SECURITY-CRITICAL composeBrief comment forbidding the
gateway token from ever entering a session brief — briefs land in shared transcripts). The file was `eslint`-ignored
as `.mjs`; type-linting it for the first time surfaced ~40 findings, almost all the mechanical `|| → ??` conversion,
plus three that carried real signal.

- **The `|| '' / || null` payload-fallback discipline is now type-visible (mass `prefer-nullish-coalescing`).** The
  module reads a deliberately loose `HookEvent` (every field optional — http.mjs authenticates the request, not its
  shape), so `ev.source || 'startup'`, `ev.tool_name || 'tool'`, `ev.message || …`, `ev.session_id || ''`,
  `ev.reason || 'end'`, `input.file_path || input.notebook_path`, `serverBranch || ev.git_branch || null`, etc. were
  everywhere. Converted freely to `??`: these are `string | undefined` payload fields and an empty string is never a
  real hook input (the CLI omits absent keys), so `??` is behaviour-identical for every value that actually arrives
  while being the safer operator.
- **Kept `||` (one inline disable) at the single site where `??` would change behaviour — the AskUserQuestion
  headline.** `const first = (Array.isArray(qs) && qs[0]?.question) || 'structured question'` — the `&&` evaluates to
  the literal `false` when the payload carries no questions array, and `??` does NOT coalesce `false`, so it would
  leak `false` into the note. The `||` is load-bearing; disabled inline with a `-- reason`. (Line 466's
  `path.basename(ev.cwd ?? '') || 'cwd changed'` needed NO disable: `path.basename` returns a non-nullable `string`,
  so `prefer-nullish-coalescing` never fires on its `||` — eslint --fix auto-deleted the disable I first added there
  as unused, a useful confirmation of the "only nullable LHS is flagged" rule.)
- **`no-useless-assignment` — the M-B6 plan-capture transaction's three `let x = null` seeds were dead code.** `row`
  / `planRowId` / `callsign` are each assigned unconditionally inside the `BEGIN IMMEDIATE` try before any read, and
  the catch always `ROLLBACK`s and `return null`s — so the `= null` seeds (and their `| null` unions) were never read
  on any reachable path. Dropped all three to bare `let row: { id: number }` / `let planRowId: number` /
  `let callsign: string`. TS's definite-assignment analysis proves the later reads are safe: when a try's catch
  cannot fall through (this one returns), the post-try state equals the try's end state, where all three are
  assigned.
- **`no-base-to-string` — `String(ev.tool_input?.plan ?? '')` stringified an `unknown`.** `ToolInput.plan` is typed
  `unknown` (ExitPlanMode's plan is opaque on the wire), and `String()` on `unknown` is exactly what the rule guards
  — a non-string would stringify to the useless `[object Object]`. Rewrote to a
  `const rawPlan = ev.tool_input?.plan; const planMd = typeof rawPlan === 'string' ? rawPlan : ''` guard. The real
  string path is unchanged; the dropped `String(… ?? '')` else-branch only ever produced `''` for the inputs that
  occur (undefined/absent plan), never the `[object Object]` a non-string plan ExitPlanMode never sends.
- **`no-unnecessary-type-conversion` / `no-unnecessary-condition` / `prefer-optional-chain` — mechanical tidy-ups
  from narrowing.** Dropped redundant `String()` on already-`string` values: `String(input.command)` (narrowed by the
  `&& input.command` guard), `String(first)`, and `String(ev.transcript_path)` (narrowed by the `&&` in the
  succession guard) — but KEPT `String(ev.cwd)` in the ClearSuccessionRefused log, where cwd is `string | undefined`
  and String() is a real conversion that preserves the diagnostic's `cwd undefined` rendering. The model-extraction
  expression's `typeof m === 'object' && m ? …` shed its dead `&& m` (the payload type `string | { display_name?;
  id? }` has no null, so the object branch is always truthy) — rewritten as an `if/else if` that also folds its inner
  `display_name || id` to `??`. The clear-succession placeholder guard `existing && existing.events === 0 && …`
  collapsed to `existing?.events === 0 && …` (TS narrows `existing` non-null through the rest of the `&&` chain after
  the `=== 0` compare).
- **Typedefs added (no runtime effect):** `ToolInput` / `HookEvent` (the on-the-wire payload, every field optional by
  design), `Conflict` (a structural mirror of ledger's un-exported handle, keeping events decoupled), `ApplyResult`,
  and `EventsCtx` — the consumer's view of the core ctx typed from usage (derive.mjs is still JS/unchecked), with `q`
  as `Statements['q']` and the questions relay's payload params left `unknown` (events passes both a `HookEvent` and a
  `{ text }` freeform shape, persisted opaquely). `errMessage(err: unknown): string` for the
  `useUnknownInCatchVariables` catch clauses; the `FLEETDECK_TEST_FAIL_PLAN_INSERT` seam reads via bracket access
  (`process.env['…']`, TS4111).
- **Verify:** `eslint events.ts` clean (0). `tsc --noEmit` clean project-wide (0). Sole importer
  `scripts/fleetd/derive.mjs:28` repointed `./events.mjs → ./events.ts` (events has no test that imports it directly;
  it is driven through derive). 133/133 green across the events-exercising suites — hook-auth +
  hook-missing-session-id + plans + derive-audit-reliability (67) and mail-and-blocking + dismiss + fleet-bugs (66);
  **# fail 0**. Daemon bundle rebuilds with the `// scripts/fleetd/events.ts` banner (632.4kb), passes `node --check`.

### scripts/fleetd/spawn.mjs → spawn.ts  [1 typing REGRESSION a pinned test caught + preserve-caught-error ×5 + NOISE + Write-hazards]

The v1.2 tmux adapter — 1082 lines, the single most security-load-bearing module in the daemon. Every doctrine
comment is preserved verbatim: the `FIELD_SEP` C-locale contract (a TAB separator collapses under a C/POSIX-locale
tmux server, so the shipped separator must survive the `display-message` round-trip — the bug tmux-locale-separator
pins), the generation **death-certificate** reasoning (BUG-046 / BUG-053 — an owner-only generation file records
`{generation, serverPid}` so a normally-exited owner is retired and a survivor is never usurped), the `newWindow` `-e`
security block (the gateway credential travels through tmux's OWN environment, so it never enters the pane's argv and
never shows in `ps` for the multi-hour life of the pane — the exact claim gateway-newwindow asserts on), the
`sanitizePaneText` bracketed-paste breakout defense, and the `killWindowVerified` invariant that a scoped-window kill
must NEVER fall back to a bare `kill-server` (the fleet shares the default tmux socket). The file was `eslint`-ignored
as `.mjs`; type-linting it for the first time surfaced the usual `|| → ??` noise plus five real `preserve-caught-error`
findings — and the strict `string` annotation on `sanitizePaneText` introduced a genuine runtime regression that only
its pinned test caught.

- **THE REGRESSION: narrowing `sanitizePaneText(text)` to `: string` silently dropped the documented `String()`
  coercion.** The `.mjs` took an untyped `text` and opened with `String(text).replace(/\r\n?/g, '\n')`. The contract
  (pane-paste-sanitize.test.mjs) explicitly pins `sanitizePaneText(12345) === '12345'` and
  `sanitizePaneText(null) === 'null'` — mail bodies reach this chokepoint with no static type guarantee. Typing the
  parameter `string` and writing `text.replace(...)` compiled clean, type-checked clean, and **threw
  `text.replace is not a function`** on the coerced-input test at runtime. Restored the contract honestly: the
  parameter is now `unknown` (it genuinely accepts anything), and the body re-opens with `String(text)`. This is the
  migration's cautionary shape in miniature — a `: string` annotation is a *claim the runtime never made*; when the
  real contract is "coerce whatever comes in," `unknown` + an explicit `String()` is the faithful port, and here only
  the test stood between the silent narrowing and a crash on the first non-string mail body. `no-base-to-string` does
  NOT fire on `String(unknown)`, so no disable was needed.
- **`preserve-caught-error` ×5 — every re-thrown generation error now carries `{ cause: err }`.** The five
  generation-file operations (read / persist / replace / record-retired / retire) each wrap a failing fs or
  `JSON.parse` call and re-`throw new Error(<human message>)`; as an eslint-ignored `.mjs` the dropped cause chain
  never surfaced. Linted as `.ts`, `preserve-caught-error` flagged all five (`cannot read persisted tmux generation`,
  `cannot persist tmux generation`, `cannot replace persisted tmux generation`, `cannot record retired tmux
  generation`, `cannot retire persisted tmux generation`). Fixed by appending `, { cause: err }` to each `Error` — the
  diagnostic text is byte-identical and the originating errno / parse error is now preserved for the daemon's log.
- **Two latent-NPE guards — `replacePersistedGeneration` / `persistGeneration` can return `null`.** Both are typed
  `Promise<PersistedGeneration | null>`, so `noUncheckedIndexedAccess`-style discipline surfaced that
  `expected = await replacePersistedGeneration(...)` (and the identical `persistGeneration` call) could hand a `null`
  into the `expected.serverPid` reads below. Added `if (expected === null) return { enabled: true, expected: null,
  verified: false };` after each — a faithful "could not claim, stay unverified" fall-through that matches the
  surrounding generation-lock protocol rather than crashing.
- **`readPersistedGeneration` restructured to guard-then-cast around `JSON.parse`.** `JSON.parse` returns `any`; under
  strict typing that `any` would silently poison every downstream field read. Rewrote as `let record: unknown = null;
  try { record = JSON.parse(value); } catch { /* strict error re-thrown below */ }`, then null / `typeof` / `Array`
  guards, then `const rec = record as { generation?: unknown; serverPid?: unknown }`, then the existing shape
  validation (`keys.length !== 2 || keys[0] !== 'generation' || …`). Same accept/reject decisions as the `.mjs`, but
  the malformed-file path is now type-visible instead of riding an `any`.
- **Write-tool control-byte hazard — `\xHH` / `\x1b` / `\x00` only, never `\uXXXX`.** The regexes and format strings in
  this file carry real control bytes (the bracketed-paste markers, the C0/C1 strip, the FIELD_SEP round-trips). Written
  through the Write tool, a `\uXXXX` escape DECODES at the JSON layer into a raw control byte in the file on disk,
  whereas `\xHH` and `\n`/`\t`/`\r` stay literal source. Every control escape here is the `\xHH` / `\x1b` / `\x00`
  form; the whole file scans clean (`LC_ALL=C grep -aPc '[\x00-\x08\x0b-\x1f\x7f]'` → 0), as does the regenerated
  bundle.
- **`no-control-regex` inline disables (3), each with a doctrine reason.** `sanitizePaneText` keeps two — on the
  bracketed-paste-marker delete (`/\x1b\[20[01]~/g`) and on the load-bearing C0/C1/DEL strip
  (`/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g`) — and `killWindowVerified`'s scope regex (`/^fd(\d+)-[^\x00-\x1f\x7f]+$/`) keeps
  one. eslint DOES recognize the `\xHH` escape form, so these disables stay "used"; stripping the control matches is the
  entire point of each pattern, which the reason comments state.
- **NOISE — the mechanical residue of narrowing.** Mass `prefer-nullish-coalescing` `|| → ??` across the option and
  result plumbing; `restrict-template-expressions` `String()` wraps only on the genuinely-nullable branch
  (`${String(state.expected.serverPid)}` where `serverPid` is `number | null`, left bare for the always-`number`
  `${port}` / `${process.pid}`); `require-await` dropped the unused `async` from `sendBringupEnter` (now
  `export function sendBringupEnter(target): Promise<boolean> { return sendEnter(target); }`); `no-unnecessary-condition`
  dropped a dead `typeof name` guard TS already proves; `noUncheckedIndexedAccess` added the `scopePort = scope[1]; if
  (scopePort === undefined) …` and `hit = matches[0]; if (hit === undefined) …` guards on regex-group / array index
  reads; a `certHome` null-guard; and `RD_NOFOLLOW` was hoisted to a module const with an inline
  `no-unnecessary-condition` disable on `(fsConstants.O_NOFOLLOW ?? 0)` (the fallback is real — `O_NOFOLLOW` can be
  undefined on some platforms even though the `@types/node` model types it non-optional).
- **Typedefs added (no runtime effect):** `Port = number | string`; the `TmuxResult` / `GenResult` result unions
  (`{ ok: true; out }` vs `{ ok: false; code?; error }`, with `GenResult` widening it with the generation fields);
  `ServerGeneration`, `PersistedGeneration`, `GenerationRecord`, `PrepareState`, `TmuxCapability` / `ProbeState`,
  `ScopedWindow`, `KillResult`, `NewWindowSpec`; the module-level `generationLocks: Map<string, Promise<PrepareState>>`;
  and `errMessage` / `errDetail` / `errCode` helpers over `unknown` catch bindings (`errDetail` reads
  `(err as NodeJS.ErrnoException).code` then `.message`).
- **Verify:** `eslint spawn.ts` clean (0). `tsc --noEmit` clean project-wide (0). Control-byte scan 0 on both source
  and bundle. All 7 real importers repointed `./spawn.mjs → ./spawn.ts` — `scripts/fleetd/termbridge.ts`,
  `scripts/fleetd/derive.mjs`, and the five tests gateway-newwindow / tmux-adapter / pane-paste-sanitize /
  tmux-locale-separator / daemon-maintenance (prose mentions of "spawn.mjs" in comments and assertion messages left
  as-is, per the events precedent). 85/85 green across the six spawn-exercising suites — tmux-adapter,
  gateway-newwindow, pane-paste-sanitize, tmux-locale-separator, daemon-maintenance, derive-audit-reliability;
  **# fail 0** (the coercion regression was red on the first run and green after the `unknown` + `String()` fix).
  Daemon bundle rebuilds with the `// scripts/fleetd/spawn.ts` source in the banner (633.7kb), passes `node --check`.

### scripts/fleetd/derive.mjs → derive.ts  [NOISE + 6 genuine cross-module strict-typing findings + 3 corrected assumptions]

The core: `applyEvent` (the faithful port of the spike's state-derivation switch) plus `createCore`, which assembles
the one giant closure state and threads it — cast `as unknown as CoreCtx` — into every extracted module-factory
(`createEvents`, `createLedger`, `createRetention`, `createSettings`, `createQuestions`, `createRepos`, …). All
doctrine preserved verbatim: the callsign/animal assignment, the 0.2.0→0.7.1 `/clear`-succession heirs
(`findClearedPredecessor` / `succeedForwardFromClear`), the offline-tombstone + model-memo bookkeeping, and the
`CoreCtx`-must-be-a-superset-of-every-`FooCtx` doctrine. The `as unknown as CoreCtx` cast is deliberate: the literal
fields are not individually checked, only the CONSUMERS (`createX(ctx)`) are — so `BaseCtx` carries only the
locally-produced / primitive fields and the intersection with the factory `ReturnType`s has no colliding keys. Because
those consumers ARE checked, wiring derive's ctx to them surfaced six genuine cross-module type mismatches — the
interesting half of this entry; the rest is the usual `|| → ??` residue.

- **Array destructuring IS subject to `noUncheckedIndexedAccess` — it does NOT escape the check (corrected a wrong
  prior assumption).** `const [first, second] = candidates` yields `first, second: SessionRow | undefined`, exactly as
  `candidates[0]` would. The runtime length guards (`if (!candidates.length) return null`, `if (candidates.length ===
  1) …`) prove presence, but the compiler can't connect a `.length` test to a destructured binding, so each read
  errored possibly-undefined. Added explicit `if (first === undefined) return null` (and `second !== undefined &&` in
  the ambiguity compare) in `findClearedPredecessor`, and `const [heir] = cands; if (heir === undefined) return null`
  after the `cands.length !== 1` guard in `succeedForwardFromClear`. These `=== undefined` checks are DEAD for the live
  callers (the length guards already guarantee presence) — they only re-prove it to tsc. NOISE, but the corrected
  mental model matters: nothing about `const [a] = arr` is exempt from the index-access check.
- **Property-narrowing does NOT lift to the object (a `!= null` check on `c.callsign` does not re-type `c`).** The
  `/clear`-rename callers guard `if (c.callsign == null) return …` upstream, which narrows the *property-access
  expression* `c.callsign` to `string` for later READS (so `animalOf(c.callsign)` and `${c.callsign}` templates are
  clean), but it does NOT re-type the non-union `SessionRow` object `c` to `SessionRow & { callsign: string }`. So the
  first draft — `renameCallsign(c: SessionRow & { callsign: string })` — could not be called with a plain `c`. Fixed by
  giving `renameCallsign` a plain `SessionRow` param and re-proving callsign INSIDE it (`const previous = c.callsign;
  if (previous == null) return { ok: false, reason: … }`). The early return is dead for the live callers (all guard it
  upstream); it re-earns the `string` the compiler forgot at the call boundary. NOISE / faithful port.
- **An `interface` has no implicit index signature → not assignable to `Record<string, unknown>`; and the linter forbids
  the `type`-alias that would fix it.** `RenameOutcome` is consumed by commands.ts as `RenameResult` (=
  `Record<string, unknown>`) and by events.ts as `{ ok: boolean }`. As an `interface` it is NOT assignable to
  `Record<string, unknown>` (interfaces get no implicit string index signature; a `type X = {…}` alias WOULD, verified
  in an isolated repro). But switching to a `type` alias is auto-reverted on save by the linter's
  `consistent-type-definitions: 'interface'` rule (pulled in by `stylistic-type-checked`). Resolved by keeping the
  `interface` and adding an explicit `[key: string]: unknown` index signature. Tradeoff (documented at the type): an
  explicit index signature relaxes excess-property checking on construction — acceptable here, the outcome object is
  built in one place. NOISE (compiler/linter tension, no runtime move).
- **Cross-module callsign nullability — ledger.ts `recordFile` assumed a non-null callsign that events threads
  nullable.**  `createEvents(ctx)` calls `recordFile(sid, file, c)` with the raw session row `c`, whose `callsign` is
  `string | null`, but ledger's `recordFile` typed its `editorCard` as `CardRow` (`extends SessionRef { callsign:
  string }`) — a claim the wire never made. Widened the param to `SessionRef & { callsign: string | null }` (ledgerKey
  consumes only the `SessionRef` identity; callsign is display-only), and coalesced its three display templates to
  `${editorCard.callsign ?? sid}`. `CardRow` itself is unchanged (still the `card:(id)=>CardRow` provider's return).
  UNSOUND → tightened: the `.mjs` would have rendered a literal `null` callsign into a conflict whisper/mail for a
  not-yet-named session; the `?? sid` fallback is the honest display.
- **Cross-module `Conflict.severity` union — events.ts mirrored ledger's exact `'warning' | 'info'` as a loose
  `string`, which broke a contravariant param.** events.ts declares its own structural `Conflict` (to stay decoupled
  from ledger's un-exported handle) and had `severity: string`. But the ctx also exposes ledger's `whisperText(conflict:
  Conflict)`, whose param is contravariant: a WIDER `severity` on events' `Conflict` makes ledger's `whisperText`
  un-assignable to events' `whisperText` ctx method. Narrowed events' `Conflict.severity` to `'warning' | 'info'` to
  match ledger's `Severity` exactly. NOISE (structural-mirror drift; the union was always the real contract).
- **Duplicate `TmuxAdapter` types (derive vs retention) diverged on `spawnOverrideCmd` optionality.** derive derives
  its `TmuxAdapter.spawnOverrideCmd` as OPTIONAL (production spawn.ts ships it; the narrower test adapters omit it),
  while retention.ts had declared its own `TmuxAdapter.spawnOverrideCmd` as REQUIRED — so `CoreCtx` (optional) was not
  assignable to `RetentionCtx` (required). Aligned retention's to optional and switched its one call site to
  `tmuxAdapter.spawnOverrideCmd?.()`. NOISE (two independent structural mirrors of one runtime shape must agree on
  optionality/nullability/unions or the intersection fails).
- **NOISE — the mechanical residue.** Mass `prefer-nullish-coalescing` `|| → ??` across the ctx plumbing;
  `restrict-template-expressions` `?? sid` coalesces on nullable-callsign display templates; `noUncheckedIndexedAccess`
  guards on the destructures above; index-signature `process.env['…']` env reads; `errMessage(err: unknown)` for the
  `useUnknownInCatchVariables` catch clauses. Typedefs added (no runtime effect): `CoreCtx` / `BaseCtx`, `ModelMemo`,
  the `SpawnModule = typeof import('./spawn.ts')`-derived `TmuxAdapter` / `ScopedWindow`, and `RenameOutcome`.
- **Verify:** `eslint` clean (0) and control-byte scan 0 across all touched files (derive.ts, ledger.ts, events.ts,
  retention.ts + the five carried from the prior window: repos, plans, settings, snapshot, questions). `tsc --noEmit`
  clean project-wide (0). All 16 real importers repointed `./derive.mjs → ./derive.ts` — the entry `fleetd.mjs`, the
  dynamic `import()` in network-refresh, the `path.join(FLEETD_DIR, 'derive.ts')` spawned-harness string in
  hook-auth:103, and 13 static test imports (prose mentions of "derive.mjs's …" in comments left as-is, per the
  events/spawn precedent). `scripts/fleetd/derive.mjs` git-rm'd — no functional consumer of the `.mjs` remains. Daemon
  bundle rebuilds with the `// scripts/fleetd/derive.ts` banner (634.6kb), passes `node --check`. 230/230 green across
  the 15 derive-exercising suites (derive-audit-reliability, dismiss, fleet-bugs, mail-and-blocking,
  mail-delivery-lease, repos, adopt, worktrees, revive, spawn-setup, shell-spawn, lan-mdns-state, hook-auth,
  daemon-maintenance, network-refresh); **# fail 0**.


---

## Consolidated per-area staging logs (folded in 2026-08-10)

During the parallel `.mjs`→`.ts` conversion, strict-typing consequences were
accumulated in per-area staging files to avoid concurrent-append races. The
conversion phase has landed, so each is folded in verbatim below and its
standalone file removed. (The board pillar is separate; its log remains in
`ts-migration-bugs.board.md`.)


---

### ⇩ merged from `ts-migration-bugs.bin.md`

<!-- STAGING for bin/ (fleetdeck.ts + tmux-version.ts) — merge into ts-migration-bugs.md
     (newest-first, right below the "entries appended below" marker) once the concurrent
     board subagent has finished its own append. Kept separate only to avoid a
     concurrent-append race on the shared log. -->

### bin/fleetdeck.ts + bin/tmux-version.ts — the install/serve/service CLI (~1010 loc): one PRE-EXISTING latent bug surfaced-and-preserved, otherwise all NOISE, every security invariant intact   [LATENT BUG — preserved]

- **What:** the security-critical CLI (env-file + systemd-unit writing, supervisor/stop
  identity gates, token rotation, tmux-version probe). Converted `bin/fleetdeck.mjs` →
  `bin/fleetdeck.ts` and `bin/tmux-version.mjs` → `bin/tmux-version.ts`, then bundled the CLI
  back to a committed self-contained `bin/fleetdeck.mjs` artifact (`bun run bundle:bin`, new
  CI drift gate). `tsc` (root maximal-strict) + type-aware `eslint` driven to **0**,
  control-bytes 0, **zero intended runtime move**. Every security surface preserved verbatim:
  env-value BARE-SAFE/UNQUOTABLE quoting (read by both POSIX `.`-source and systemd
  `EnvironmentFile`), 0600 chmod-on-write, `%`→`%%`-first systemd escaping, always-quote
  ExecStart args refusing control/`"`, EnvironmentFile-path refusal of whitespace/quotes/
  backslash, POSIX single-quote `shQuote` (BUG-079 no-expansion), the SUPERVISOR-IDENTITY
  (`argvIsOurSupervisor` via /proc cmdline) and STOP-TARGET-IDENTITY
  (`healthIsOurManagedDaemon`/`healthPidIsOurDaemon` → managed + FLEETD_PID ownership + port
  match + `verifyDaemonPid`) contracts, 0600 token rotation, lazy (never top-level) takeover
  import, `resolveCliPort` 1..65535 clamp, and ExecStart/SUPERVISE `path.join(HERE,
  'fleetdeck.mjs')` (the artifact must keep that basename).

- **THE LATENT BUG — supervised `service start` accepts a foreign responder (surfaced by strict
  typing, deliberately NOT fixed):**
  - **Where:** `serviceStart()` non-systemd branch, `bin/fleetdeck.ts:775`:
    `const h = await waitForHealth({ expect: healthIsOurManagedDaemon });`
  - **The defect:** `healthIsOurManagedDaemon` is `async` (`:714`, returns `Promise<boolean>` —
    it must be, it `await import()`s takeover's `verifyDaemonPid`). `waitForHealth`'s loop tests
    the predicate **un-awaited**: `if (h && (!expect || expect(h))) return h;` (`:700`). A
    Promise is *always* truthy, so `expect(h)` is satisfied for **any** non-null health answer —
    the managed-identity gate is effectively a no-op inside the wait loop.
  - **Consequence:** on the supervised (no-systemd) path, if an **unmanaged/foreign** daemon
    already owns `:PORT`, `waitForHealth` returns that squatter's health on the first probe,
    `if (!h)` (`:776`) is false, and `service start` reports the board as up — precisely the
    failure mode the `healthIsOurManagedDaemon` gate was written to prevent (see the contract
    comment at `:705–713`). The takeover port-election still runs and the wrapper still exits 3
    on a lost election, so this mis-reports *success* rather than corrupting state; the
    user-facing lie is "✓ up" instead of the intended "✗ no MANAGED daemon for this
    FLEETDECK_HOME" + squatter diagnostic.
  - **Pre-existing, not migration-introduced:** the pre-migration `.mjs` had an untyped `expect`
    param and called an `async` predicate the same un-awaited way — identical runtime. The
    conversion reproduces it byte-for-byte.
  - **Why strict typing is the hero here:** typing the param as it *reads* —
    `expect?: (h: Health) => boolean` — makes `tsc` reject `:775` immediately
    (`Promise<boolean>` is not assignable to `boolean`, i.e. "you're passing an async function
    to a sync-boolean slot"). That is the exact class of bug maximal-strict is meant to catch.
  - **Why I did NOT fix it in this commit:** the `/goal` is a faithful `.mjs`→`.ts` conversion
    with **no silent behavior move**; awaiting the predicate (`if (h && (!expect || await
    expect(h)))`) would change the supervised-start success/refusal outcome for a real
    squatter — a behavior change that must land as its own reviewed fix, not smuggled inside a
    type migration. I therefore typed the param `expect?: (h: Health) => unknown` (`:695`),
    which is honest about the current call (an async predicate's return is treated as an opaque
    truthy token) and keeps `tsc` green without masking the finding. **This log IS the
    hand-off:** the follow-up fix is to make `waitForHealth` await the predicate and type it
    `(h: Health) => boolean | Promise<boolean>`.

- **Why the rest is noise:**
  1. **TS4111 ×6 — `process.env.X` bracket access.** `noPropertyAccessFromIndexSignature` requires
     `process.env['FLEETDECK_HOME']` etc. Pure syntax; no behavior. (FLEETDECK_HOME, FLEETDECK_PORT,
     FLEETDECK_MANAGED, XDG_CONFIG_HOME, FLEETDECK_PROXY_AUTH, FLEETDECK_TRUSTED_ORIGINS.)
  2. **`prefer-nullish-coalescing` on load-bearing `||`.** Three sites intentionally fall an
     **empty string** back to a default (`FLEETDECK_HOME || default`, `XDG_CONFIG_HOME || default`,
     `spawn.reason || 'unknown'`) — `??` would *keep* `''`, changing behavior. Kept `||` with a
     scoped `// eslint-disable-next-line … -- <empty-string reason>`, the sibling idiom
     (ingest/mdns/events/settings/mail/commands). NB: a `x ? x : y` ternary trips the same rule,
     so the disable — not a rewrite — is the correct move.
  3. **`no-control-regex` ×2 — the whole point of the gate.** `ENV_VALUE_UNQUOTABLE`
     (`/[\u0000-\u001f\u0027\u005c]/`) and `EXEC_ARG_UNQUOTABLE` (`/[\u0000-\u001f"]/`) exist to
     *refuse* NUL/C0 controls in env values and ExecStart args. Scoped disable-with-reason.
     (Editor note: these lines carry literal control-range escapes; the disables were inserted via
     `sed` anchored on the const names because the Edit layer decodes `\uXXXX` before matching.)
  4. **`no-base-to-string` — `String(output ?? '')` in `parseTmuxVersion`** (`tmux-version.ts:15`):
     intentional coercion of untrusted `tmux -V` output, matching the `.mjs`. Scoped disable.
  5. **`no-unnecessary-type-conversion` — `Number(h.pid)` kept** (`:665`). `h` is an unchecked
     cast of wire `/health` JSON; `Number()` coerces a stringy pid *before* the integer guard —
     load-bearing for the kill-target identity check. Kept with a disable. (Contrast: dropped a
     provably-redundant `String(s)` in `shQuote`, whose param is a typed `string` path, never wire.)
  6. **Trivia:** `takeoverPidHelpers ??=` memo, one optional-chain (`record?.pid`), removed a
     useless `= null` init, and a comment inside the detached-supervisor `child.once('error', …)`
     empty handler (failures surface via the health probe) — all sibling idioms, no runtime move.

- **Fix:** all in place. Isolated `tsc` clean on `bin/fleetdeck.ts`, `bin/tmux-version.ts`,
  `scripts/fleetd/spawn.ts`; `eslint` exit 0; control-bytes 0. Importer repointed:
  `scripts/fleetd/spawn.ts:36` → value-imports `{ MIN_TMUX_VERSION, tmuxVersionCapability }` from
  `../../bin/tmux-version.ts`; `tests/cli.test.mjs:57–58` → `../bin/fleetdeck.ts` +
  `../bin/tmux-version.ts` (in-process source lane), its `:611` subprocess still runs the built
  `.mjs` artifact; `tests/cli-serve-paths.test.mjs` packs only the self-contained `bin/fleetdeck.mjs`
  (tmux-version inlined, no sibling copy). Suites green: `cli.test.mjs` + `cli-serve-paths.test.mjs`
  → 48 pass / 0 fail. Daemon rebundle from `.ts` for the `spawn.ts` value-import rides task #6.


---

### ⇩ merged from `ts-migration-bugs.fleetd.md`

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


---

### ⇩ merged from `ts-migration-bugs.floor.md`

<!-- STAGING for the Node engine-floor raise (22.13 -> 22.18) that the TS migration
     forced. Merge into ts-migration-bugs.md (newest-first) once the concurrent board
     subagent has finished its own append; kept separate only to avoid a concurrent-
     append race on the shared 140k log. This is a PREREQUISITE for the test-conversion
     phase (task #11): the suite cannot run on the old floor at all (see below). -->

### Engine floor raised 22.13.0 -> 22.18.0 — the TS migration's one published-contract change   [MIGRATION CONSEQUENCE, not a code bug — but it is the thing that unblocks the whole test phase]

- **What forced it.** Running the project's TypeScript sources directly requires Node's
  native, unflagged type-stripping, which first shipped in **22.18.0**. The daemon `start`
  script already points at `fleetd.ts`, and — the binding discovery — the EXISTING
  `tests/cli.test.mjs` already does `await import('../bin/fleetdeck.ts')` and
  `'../bin/tmux-version.ts'`. So the test suite in its CURRENT `.test.mjs` form ALREADY
  needs >=22.18: on the old CI floor lane (pinned `22.13.0`) `cli.test.mjs` would throw on
  the `.ts` import before asserting anything. The floor raise is not merely future-proofing
  for the `.test.ts` rename — it repairs the branch's present state.

- **Why not keep the floor at 22.13 for consumers?** Considered and rejected by the
  maintainer (informed choice). The shipped artifact is the plain-JS esbuild BUNDLE, which
  genuinely runs on 22.13 (node:sqlite loads unflagged from 22.13.0), so a "declared 22.18,
  tolerated 22.13" split was technically possible — but the codebase deliberately locks
  `doctor`/guard == `package.json engines` (a `cli.test.mjs` invariant: "doctor text and
  engines must not drift apart"). A split would fight that invariant for a range (22.13–22.17)
  that, by Aug 2026, essentially no one runs — especially Claude Code users. Raising the
  single floor is simpler and keeps CI structurally unchanged.

- **CONSEQUENCE to flag: the runtime guard now hard-blocks 22.13–22.17.**
  `bin/fleetdeck.ts` `nodeVersionSupported` gates whether the CLI boots (`major===22 ->
  minor>=18`). Because the shipped bundle is plain JS, those versions would *technically*
  still run fleetd — but they are now out of the supported/declared/enforced range, and the
  guard refuses them with a clear message rather than silently supporting an untested range.
  This is the honest reading of "drop declared support for 22.13–22.17." If a future need to
  tolerate-but-not-support that range arises, decouple the guard from `engines` (and relax
  the `cli.test.mjs` drift assertion) — but that is a deliberate reversal, not the default.

- **Files changed (all mine; no comet surface touched):**
  - `package.json` — `engines.node` `^22.13.0 || >=24.0.0` -> `^22.18.0 || >=24.0.0`.
  - `bin/fleetdeck.ts` — `MIN_NODE_RANGE` + the `minor>=13`->`minor>=18` check + the doctor
    message (dropped the misleading "for node:sqlite" clause — node:sqlite is NOT the binding
    constraint at 22.18) + the rationale comments + the source-require floor note (source-run
    floor and supported floor are now equal, so that path never needs a newer Node than the
    CLI already requires).
  - `bin/fleetdeck.mjs` — rebuilt from the edited `.ts` via esbuild (`bundle:bin` args,
    byte-for-byte the shipped artifact); floor now reads `>= 18`.
  - `tests/cli.test.mjs` — the floor test rewritten: `22.13.0`/`22.17.1` now expected FALSE,
    `22.18.0` the first TRUE; the `MIN_NODE_RANGE` equality assertion bumped. 43/43 green
    locally on v22.22.2.
  - `.github/workflows/ci.yml` — matrix `['22.13.0','24']` -> `['22.18.0','24']`; the pin
    rationale rewritten (22.18 = first unflagged type-strip); the "floor CANNOT strip .ts"
    comment block rewritten (22.18 CAN strip — the floor lane still runs the BUNDLE because
    that is what SHIPS, not because the floor can't strip source).
  - `scripts/fleetd/http.ts` — a one-line comment (`engine floor (22.13)` -> `(the engine
    floor)`), accuracy only.
  - `README.md` — badge `>=22.13`->`>=22.18`, requirements line, `doctor` comment.
  - `CONTRIBUTING.md` — the floor bullet + the "README promises Node 22.13+" line.

- **DEFERRED (coordination):** `docs/CODER.md` still says `^22.13.0 || >=24.0.0` (line ~191).
  It is a SHIPPED doc (in `package.json files`) and currently clean, but it lives under
  `docs/**`, the boundary the concurrent comet-55e1 session is actively editing. Left for the
  final docs pass (task #9) / once comet's docs work settles, to avoid a clobber. Same for
  `docs/v1/ts-migration.md` and `docs/v1/phase6-ci-publish-bun.md` (comet's untracked v1 docs
  that mention 22.13 in migration-planning context).

- **Verification:** `tsc -p tsconfig.json` clean for `bin/**`; `eslint` clean on
  `bin/fleetdeck.ts`; `bin/fleetdeck.mjs` rebuilt and floor-checked; `cli.test.mjs` 43/43 on
  node v22.22.2. This lands as its own green checkpoint BEFORE any `.test.mjs -> .test.ts`
  rename.


---

### ⇩ merged from `ts-migration-bugs.hooks.md`

<!-- STAGING for scripts/fleet-*.ts (the three plugin hook shims) — merge into
     ts-migration-bugs.md (newest-first, right below the "entries appended below"
     marker) once the concurrent board subagent has finished its own append. Kept
     separate only to avoid a concurrent-append race on the shared log. -->

### scripts/fleet-hook.ts + scripts/fleet-sessionstart.ts + scripts/fleet-watch.ts — the three plugin hook shims (~550 loc): NO real bug, one instructive TS flow-analysis limitation surfaced-and-worked-around, otherwise all NOISE, every security invariant intact   [ALL NOISE / TS-LIMITATION]

- **What:** the three command-hook shims Claude Code runs — `fleet-hook.ts` (authenticated
  shim for every hook event except SessionStart + the Stop rewake leg), `fleet-sessionstart.ts`
  (the ONE election+spawn+takeover+brief hook), `fleet-watch.ts` (the F3d-2 asyncRewake Stop
  watcher). Converted all three `.mjs` → `.ts` under root maximal-strict, then added a
  `bundle:hooks` esbuild step producing self-contained committed `.mjs` artifacts (inlining the
  four `scripts/fleetd/` deps — config.ts, run-nonce.ts, env-scrub.ts, takeover.ts — which
  import only node builtins, so the bundles carry zero bare imports). New CI drift gate
  (`bun run bundle:hooks` + `git diff --exit-code` on the three artifacts). `tsc` (root
  maximal-strict) + type-aware `eslint` (`--report-unused-disable-directives`) driven to **0**,
  control-bytes 0 on both sources and artifacts, **zero intended runtime move**.

- **Every security surface preserved verbatim:** the LOCKSTEP hold-window invariant
  (`HOLD_EVENTS` → 660 s watchdog under hooks.json's 720 s), the re-armable SessionStart
  watchdog (3.8 s → 8 s on takeover, always < hooks.json's 15 s ceiling), run-nonce tagging
  (BUG-025, keyed on CLAUDE_PID), the takeover identity gates (`shouldTakeOver` +
  `verifyDaemonPid` + managed-daemon no-evict + `replacementMatches` re-arbitration with the
  round≥1 anti-flap cap), the `bootEnv` scrub list (Claude/agent markers + gateway + spawn +
  the test seams that must never ride a tmux server's global env — the 2026-07-11 ghost-daemon
  scar), the 0600 chmod-on-open of `fleetd.log`, the detached-spawn `child.once('error', …)` +
  `unref()` + fd release in `finally`, the single-flight NEWEST-WINS pid ownership + BUG-105
  `wg` generation token + BUG-034 `/mail/ack` lease + 64 KB/1 MB stdin ceilings + listener
  cleanup in fleet-watch, and the silent-exit-0 failure contract in all three. Artifacts keep
  their `fleet-*.mjs` basenames, so **hooks.json needs no repoint** (verified: it still names
  `fleet-hook.mjs` / `fleet-sessionstart.mjs` / `fleet-watch.mjs`), and `spawn` stays an
  external named `node:child_process` import in the sessionstart artifact (the audit spawn-error
  test's `syncBuiltinESMExports()` depends on that).

- **THE INSTRUCTIVE FINDING — TS narrows a closure-mutated module `let` to its initializer
  (`fleet-sessionstart.ts`, worked around, NOT a runtime bug):**
  - **Where:** `let replacedVersion = null` / `let managedVersionDrift = null` at module scope,
    assigned ONLY inside `ensureServer()` (`replacedVersion = health.version` on a committed
    takeover; `managedVersionDrift = health.version` on a managed-daemon drift), then READ at
    top level after `await ensureServer()` (`if (replacedVersion) payload.fleet_takeover = …`;
    `if (managedVersionDrift) { … v${managedVersionDrift} … }`).
  - **The symptom:** with a plain `let x: string | null = null`, TS flow analysis narrows every
    top-level read to the literal `null` — it cannot see the assignment across the
    `ensureServer` closure boundary (a call does not reset a local's narrowing, and the
    "assigned-in-nested-function ⇒ widen to declared type" heuristic only fires for reads
    *inside* a nested function, not for reads in the declaration's own scope). So
    `no-unnecessary-condition` reports the real takeover guards as **"always falsy"**, and the
    `if`-body narrows `managedVersionDrift` to **`never`**, which `restrict-template-expressions`
    then rejects inside the drift-warning template. `tsc` itself stayed green — `null`-in-a-
    condition and `never`-in-a-template are lint findings, not type errors — so this is exactly
    the class of latent mis-modeling maximal-strict + type-aware lint is meant to expose.
  - **Why it is NOT a bug:** the runtime is correct — `ensureServer()` really does set both
    before the reads; only the *type checker's* model was wrong. Confirmed empirically in a
    throwaway scratch: `let x: string|null = null` reproduces the always-falsy/never errors;
    `let x = null as string | null` does not.
  - **Fix (faithful, zero runtime move):** widen the initializer with `null as string | null`
    so the read-site type stays `string | null` (guard necessary, `if`-body narrows to
    `string`), with a comment naming the cross-closure assignment as the reason. Chosen over a
    blanket `no-unnecessary-condition` disable because the cast *documents the shared-state
    reality* and keeps the guard genuinely type-checked, and over restructuring
    `ensureServer` to return the values because `replacedVersion` is consumed mid-function by
    `bootEnv()` (it must be module state, not a return value).

- **Why the rest is noise:**
  1. **`no-dynamic-delete` — `delete env[k]` in `bootEnv`** (sessionstart). The scrub loop
     deletes a *dynamic* key list; `delete` (not `env[k] = undefined`) is load-bearing — it
     guarantees the key is ABSENT from the child's env rather than passed as an empty string.
     Scoped disable-with-reason.
  2. **TS4111 ×2 — index-signature dot access.** `noPropertyAccessFromIndexSignature` requires
     `process.env['FLEETDECK_TEST_DAEMON_SCRIPT']` and `env['FLEETDECK_REPLACED']` (ProcessEnv is
     an index signature). Pure syntax; no behavior.
  3. **`prefer-nullish-coalescing` on load-bearing `||` ×2** (sessionstart). `FLEETDECK_TEST_DAEMON_SCRIPT || bundle`
     (an empty/unset test seam must fall back to the bundle, not be kept as `''`) and
     `payload.hook_event_name || 'SessionStart'` (a missing OR empty event name must default) —
     `??` would keep `''`, a behavior change. Kept `||` with scoped disables, both confirmed
     load-bearing via `--report-unused-disable-directives`. (fleet-hook.ts carries the same
     idiom on `withRun(raw) || '{}'`.)
  4. **exactOptionalPropertyTypes forbids `headers: undefined` / `body: undefined`.** In
     sessionstart's `api()` the request body is spread in only when present
     (`...(body ? { body: JSON.stringify(body) } : {})`) and `headers` is always an object
     (empty ≡ no headers on the wire); fleet-watch/​fleet-hook use the sibling shared-object
     (`authHeaders`) / typed-object patterns. No explicit-undefined property anywhere.
  5. **Wire-JSON typed as trusted shapes, cast to keep `?.` guards necessary.** `api<T>()`
     returns `(await res.json()) as T` with `T` = `Health` / `Registration` / null; the daemon
     has minted these fields at every boot, so a mismatch reproduces the pre-migration untyped
     behavior rather than a new failure mode (same idiom as bin/fleetdeck.ts's `as Health`).
     `Registration.upgrade_lines`/`.brief` stay `unknown` and are re-validated at the read site
     (`Array.isArray`, `typeof brief === 'string'`) — the daemon-garbage defense the `.mjs`
     already had. In **fleet-watch.ts** the same rule bit the other way: casting `res.json()` to
     a *non-null* `WatchResponse` let TS prove `out` never-null and turned the runtime null-body
     guards into `no-unnecessary-condition` errors → fixed by casting to `WatchResponse | null`.
  6. **`restrict-template-expressions` bans `null`/`any` in templates.** `String(ownVersion())`
     (ownVersion is `string | null`) and `${String(line)}` (an `any` upgrade line off the
     Array.isArray narrowing) coerce explicitly; both are clean under `no-base-to-string`
     (neither is an object type). `String()` of the wire values matches the `.mjs`.
  7. **Trivia:** fleet-watch dropped a dead `let out = null` init (`no-useless-assignment`) and a
     single-assignment `let timer` → `const timer` (`prefer-const`); the sessionstart poll uses
     `await new Promise<void>((r) => { setTimeout(r, 250); })` (braced executor); `child.once('error', …)`
     carries a comment inside the empty handler (`no-empty-function`) — all sibling idioms, no
     runtime move.

- **Fix:** all in place. Isolated `tsc` clean on the three sources; `eslint
  --report-unused-disable-directives` exit 0 (every disable load-bearing); control-bytes 0 on
  sources AND the regenerated artifacts; artifacts self-contained (node-builtin imports only),
  shebang line 1 + banner line 2 preserved. `bundle:hooks` npm script + CI drift gate added.
  Hook-affected suites green: `accept-scripts-hook-wiring` + `audit-hardening` + `hook-auth` +
  `filechanged-watch` (33/0), `watch-rewake` + `max-turns-abort` (19/0), `takeover` +
  `smoke-project-isolation` (17/0), `plugin-payload-gate` + `release-gate` (20/0) → **89 pass /
  0 fail** against the freshly-bundled `.mjs` artifacts.


---

### ⇩ merged from `ts-migration-bugs.http.md`

<!-- STAGING for http.ts — merge into ts-migration-bugs.md (newest-first, right below the
     "entries appended below" marker) once the concurrent board subagent has finished its own
     append. Kept separate only to avoid a concurrent-append race on the shared log. -->

### http.ts — the HTTP + WebSocket surface (~1960 loc): all NOISE, one LOAD-BEARING near-miss avoided, every security invariant intact   [NOISE]
- **What:** the daemon's whole request surface — the `authorized()` gate, the CSRF wall
  (`crossSiteReason`), DNS-rebinding `hostHeaderOk`, the PROXY_AUTH trust semantics, gateway-bearer
  waivers, `serveBoardAsset` traversal safety, both WS upgrades (`/ws` state feed, `/ws/term`
  terminal bridge) with their backpressure/keepalive caps, and the hook fail-open contract.
  `tsc` (root maximal-strict) + type-aware `eslint` driven to **0**, control-bytes 0,
  **zero runtime move**. Every finding was maximal-strict ergonomics or a lint idiom already
  established by sibling `spawns.ts`/`derive.ts`/`settings.ts`. The security-critical surfaces were
  preserved verbatim: the unconditional `/hook/*` auth leading the loopback block; timing-safe
  `tokenMatches` only after a length match; the CSRF wall on state-changing POSTs, both WS upgrades
  and the two mutating GETs (`/mail`, `/api/watch`); `hostHeaderOk` rebinding defense; the
  no-Origin proxy-hole fix (`arrivedViaTrustedProxy` checks Host, not just Origin); the gateway
  bearer gate keyed off `isLoopbackAddress(req.socket.remoteAddress)`; `serveBoardAsset` staying
  strictly inside `BOARD_DIST`; CSP_SHELL on HTML only; the WS eviction caps
  (`MAX_WS_BUFFER`/`MAX_TERM_WS_BUFFER`/`MAX_TERM_FRAME_BYTES`); `spawnFailureReason` redaction on
  the `/api/spawn` catch; and hooks always failing open with `200 {}`.

- **The one that mattered — a LOAD-BEARING `|| null` that `??` would have silently broken (near-miss, NOT shipped):**
  `prefer-nullish-coalescing` flagged the `/api/watch` watch-generation read
  `const wg = url.searchParams.get('wg') || null`. A naive `?? null` autofix looks identical but is
  **not**: `searchParams.get()` returns `'' | string | null`, and the empty string is reachable
  (`?wg=`). Downstream, `claimMail(sid, gen)` (mail.ts:620) refuses to claim when `gen !== null &&
  !isWatchGen(sid, gen)` — so `''` is a *present-but-invalid* generation that **blocks mail
  delivery**, whereas `null` means "no generation, claim freely." `|| null` folds `''`→`null`
  (deliver); `?? null` would keep `''` (refuse) — a real mail-delivery regression on any client that
  sends `?wg=`. Preserved the behavior without a disable and without the rule's suggested
  behavior-changing shape: `const wgParam = url.searchParams.get('wg'); const wg = wgParam === ''
  ? null : wgParam;`. (Note: the rule *also* flags the ternary `x ? x : y` form and offers the same
  unsound `x ?? y` autofix — do not take it.) Verified against mail.ts:620 before editing.
  watch-rewake (18/18), mail-and-blocking (9/9), mail-delivery-lease (5/5), mail-frames (9/9) green.

- **Why the rest is noise (the interesting ones):**
  1. **CFA over-narrowed a callback-mutated `let` to `false`, tripping `no-unnecessary-condition`
     "always falsy".** The `/ws/term` bridge kept `let socketClosed = false`, set it in a
     `ws.on('close')` closure, and re-read it after an `await` (the R5 abort check). TS narrowed the
     bare `let` to the literal `false` at the read and declared the guard dead. The honest fix is a
     **holder object** — `const abort = { closed: false }`, mutate `abort.closed = true` in the close
     handler, read `abort.closed` after the await — because a property read across an `await` re-widens
     to the declared `boolean` (a function boundary defeats the literal narrowing). No disable, no
     behavior move; `isAborted: () => abort.closed` still fires and a late-arriving handle still closes.
  2. **`no-misused-promises` on an async WS `connection` listener.** `termWss.on('connection', async
     (ws, req) => {…})` handed a promise-returning listener to an EventEmitter. The body is fully
     try/caught and never rejects, so the sanctioned fix (same as spawns.ts's `setTimeout(async)`
     nudge) is a sync listener wrapping the body in `void (async () => { … })();`. The file was
     briefly unbalanced between the open and close edits — PostToolUse prettier skips a
     syntactically-broken file and reindented once the closing edit balanced it.
  3. **`req.socket.remoteAddress` optional-chain was flagged unnecessary.** `@types/node` types
     `IncomingMessage.socket` as a non-nullable `Socket`, so `req.socket?.remoteAddress` trips
     `no-unnecessary-condition`. Dropped the `?.` at all three sites (the loopback gate, the LAN
     log `from`, and the `bearerWaived` chain) — the gateway bearer gate still keys off
     `isLoopbackAddress(req.socket.remoteAddress)` exactly as before.
  4. **`e?.reason || e?.message` on a WS error is first-non-empty, NOT nullish.** The `/ws/term`
     error branch surfaces the closer reason, then the message, then a default. `??` would keep an
     empty-string `reason`. Preserved with string-normalization:
     `const failReason = typeof e?.reason === 'string' ? e.reason : ''; const failMessage = …;
     send({ t: 'err', reason: failReason || failMessage || 'terminal unavailable' });` — the
     remaining `||` chain is now over provably-string operands where empty-means-fall-through is the
     intended semantics. terminal-ws (18/18) green.
  5. **`no-base-to-string` on `unknown` spawn-plan id.** The spawn-accept log builds a ` plan=${…}`
     suffix from a value typed `unknown`. Guarded to a stringifiable primitive rather than coerce a
     possible object: `const spawnPlanSuffix = typeof spawnPlanId === 'string' || typeof spawnPlanId
     === 'number' ? \` plan=${spawnPlanId}\` : '';`.
  6. **`String(x)` on an already-`string` and `String(x || '')` folds.** Removed the redundant
     `String()` wrappers and simplified empty-string folds that are identical either way
     (`url.searchParams.get('session') ?? ''` for the `/mail` and watch `sid` keys, `req.url ?? ''`
     in `.startsWith('/hook/')` and `new URL(req.url ?? '/', …)` contexts) to the established `??`
     idiom — all behavior-identical because the consumer only ever does string ops.
  7. **Assorted maximal-strict ergonomics (pure NOISE, no behavior):** `use-unknown-in-catch-
     callback-variable` → `.catch((err: unknown) => …)` on the worktree-inspector / session-fs /
     home-fs detach paths; `no-unused-expressions` ternary-as-statement → `if (…) json(…); else
     json(…);` at the hook-vs-403 forks (`hostHeaderOk`, `crossSiteReason`, request-error catch);
     `no-empty-function` → explanatory-comment bodies on the initial no-op `unregister` and
     best-effort `.catch()`s; `timer.unref()` / `flushTimer.unref()` retained; the fire-and-forget
     `spawnLivenessTick()` cast to `Promise<unknown> | undefined` before `?.catch()` (the ctx seam
     types it loosely — NOISE, the tick is reconcile-only and its rejection is intentionally
     swallowed).

- **Seam notes (NOISE, no fix needed, flagged for the derive/ctx typing pass):** the `ControlResult`
  envelope, `LiveSocket.isAlive`, and the `{ raw: Buffer }` WS-frame assumption are all typed
  loosely at the `CoreCtx`/ws boundary; http.ts consumes them defensively and unchanged. No latent
  bug — recording only so the eventual ctx-contract tightening knows these three are the remaining
  loose edges the HTTP surface leans on.

- **Fix:** all in place; no runtime behavior moved. Rename staged `http.mjs -> http.ts`; importers
  repointed to `.ts` (`scripts/fleetd/fleetd.mjs:19`, `tests/lan-mdns-state.test.mjs:15`,
  `tests/network-refresh.test.mjs:24`, the generated wrapper in `tests/hook-auth.test.mjs:104`, the
  source-text read in `tests/board-util.test.mjs:617`, and the ESM loader specifier in
  `tests/helpers/mdns-dgram-loader.mjs:17`). Affected suites green on the source lane:
  lan-mdns-state 2/2, network-refresh 3/3, fleetd-audit-regressions 10/10 (loader repoint),
  hook-auth 11/11 (generated import), csrf-guard 42/42, loopback-gates 14/14, require-token 7/7,
  lan-auth 9/9, ws-hardening 5/5, static-serving 9/9, gateway 19/19, mail-and-blocking 9/9,
  mail-delivery-lease 5/5, mail-frames 9/9, takeover 15/15, terminal-ws 18/18, question-rearm 8/8,
  watch-rewake 18/18. (board-util / board-termdiag can only run once the concurrent board
  conversion lands — they top-level-import board sources that are mid-rename to `.ts`; the http.ts
  cache-control invariant they assert is present at http.ts:217 and both files are self-consistent
  at this commit's tree.)

- **Bundle deliberately NOT regenerated in this commit:** `bun run bundle` inlines *every* daemon
  source — including `scripts/fleetd/derive.ts`, which is the concurrent board session's uncommitted
  WIP — into `fleetd.bundle.mjs`. Rebundling now would bake that WIP into a committed artifact.
  Because http.ts is behavior-identical to the old http.mjs (zero runtime move), the shipped bundle
  stays behaviorally correct; the regeneration rides the fleetd.mjs-entry conversion once the board
  WIP has landed.


---

### ⇩ merged from `ts-migration-bugs.spawns.md`

<!-- STAGING for spawns.ts — merge into ts-migration-bugs.md (newest-first, right below the
     "entries appended below" marker) once the concurrent board subagent has finished its own
     append. Kept separate only to avoid a concurrent-append race on the shared log. -->

### spawns.ts — the board-spawn lifecycle giant (~3270 loc): all NOISE, no latent bug, security invariants intact   [NOISE]
- **What:** the largest daemon module (spawn / revive / harvest / kill / setup-wrapper). `tsc`
  (root maximal-strict) + type-aware `eslint` driven to **0**, control-bytes 0, **zero runtime move**.
  Every finding was ergonomics of maximal-strict on dynamic JS or a lint idiom already established by
  sibling `derive.ts`/`settings.ts`. The security-critical surfaces were preserved verbatim:
  `spawnFailureReason` still routes through `redactGitText`; `stallDiagnosticExcerpt` keeps its
  scrub/redact + 2 KB bound with `exactSecrets`; the arm gate, gateway/remote-control mutual
  exclusion, `SETUP_CONTROL_RE` shell-injection guard, and prompt-as-last-argv all unchanged.

- **Why it's noise (the interesting ones):**
  1. **`SpawnsCtx` method-shorthand members tripped `unbound-method` when destructured.** `createSpawns`
     destructures the ctx (`const { tick, logEvent, onMutate, … } = ctx`); with method-shorthand
     interface members (`tick(msg): void`) the rule fires on the free reference. Every member is a
     plain closure wired by derive's `CoreCtx` and none reads `this`, so the honest structural fix is
     arrow-property syntax (`tick: (msg: string) => void`) — exactly what the sibling `CoreCtx` already
     uses (derive.ts:183/189). Converted the interface; no disable, no runtime move.
  2. **`no-base-to-string`: `?? ''` widened `unknown` → `{}`.** `spawnFailureReason` builds
     `String((err as {message?: unknown} | null)?.message ?? err ?? '')`. The `?? ''` widened the
     operand to `{} | string`, tripping `no-base-to-string` — even though the sibling `errMessage`
     does `String(err)` on bare `unknown` cleanly. Fix: annotate the coalesced value `: unknown` so
     `String()` stays the sanctioned coercion. Behavior-identical.
  3. **`setup_cmd` insert paths disagreed on `|| null` vs `?? null`.** `prefer-nullish-coalescing`
     surfaced that the primary path (`const setupCmd = body.setup_cmd ?? null`, :928) used `??` while
     two other insert paths (:1447, :1695) used `|| null`. The only divergence is when a client POSTs
     `setup_cmd: ""`: `||` stores `null`, `??` stores `""`. Confirmed **every** consumer keys off
     truthiness — env injection `setupCmd ? {FLEETDECK_SETUP_CMD…} : {}` (:931), `!!row.setup_cmd`
     (:2886), `row.setup_cmd ? … : …` (:2911) — so `""` and `null` are functionally identical
     downstream. Harmonized all three to `?? null`. No behavior move; the setup command is still
     inert when empty.
  4. **HTTP-status fallbacks `errStatus(err) || N` → `?? N`.** `errStatus` returns `number | undefined`
     and never a real `0`, so the operators are behavior-identical here; switched to `??` to match the
     established `errStatus(err) ?? 500` idiom in settings.ts:755. Same for object/`undefined`
     fallbacks (`gatewayEnv ?? {}`, `activeSpawnBySession.get() ?? provisioning…`) and empty-string
     folds that are identical either way (`process.env['SHELL'] ?? ''`, `body.cwd ?? ''`,
     `forbidden ?? 'worktree'`, kill-result `error ?? '…'` — `KillResult.error` is only ever unset or
     a non-empty diagnostic).
  5. **`no-misused-promises`: `setTimeout(async () => {…})`.** The stall-nudge scheduled an async
     callback (a floating promise). Extracted the async body verbatim into `const nudge = async () => {…}`
     and scheduled `setTimeout(() => void nudge(), NUDGE_MS)` — the timer callback now returns `void`;
     the body (fully try/caught, never rejects) is untouched. Detach callbacks in the repo-provision
     path got the same `void`-operator treatment; empty best-effort `.catch(() => {…})` bodies got an
     explanatory comment (sibling idiom, derive.ts:1290).
  6. **`use-unknown-in-catch-callback-variable` / `prefer-optional-chain`.** `.catch((err: unknown) => …)`
     annotation; `!row || row.status !== 'spawning'` → `row?.status !== 'spawning'` (and the twin
     `SessionEnd` guard) — the narrowing still flows because `=== 'literal'` proves non-null.

- **Fix:** all in place; no runtime behavior moved. Importers repointed to `.ts`
  (`http.mjs:20`, `tests/daemon-maintenance.test.mjs:10`, `tests/git-stderr-detail.test.mjs:432`).
  Affected suites green (see commit).


---

### ⇩ merged from `ts-migration-bugs.tests.F.md`

# TS migration — strict-typing consequences (tests group F)

Group F files: revive, shell-spawn, spawn-unsupervised, spawn-setup,
watch-rewake, needs-you. Newest-first. Each entry: the strict flag / lint rule,
the shape it bit, the fix, and whether behavior shifted.

## `assert.equal` (node:assert/strict) back-narrows an optional-chain base

`node:assert/strict`'s `equal` is `strictEqual`, typed `asserts actual is T`.
So `assert.equal(card.spawn?.kind, 'shell')` narrows `card.spawn` itself to
non-nullish (its `.kind` can only be `'shell'` if `spawn` is defined). The very
next line `assert.equal(card.spawn?.status, 'live')` then trips
`no-unnecessary-condition` ("unnecessary optional chain on a non-nullish
value"). Fix: drop the now-redundant `?.` on the FOLLOWING access only
(`card.spawn.status`). Behavior-identical — line N must pass (proving `spawn`
defined) before line N+1 executes. (shell-spawn.test.ts:224)

Corollary: `assert.notEqual` / `assert.deepEqual` do NOT carry `asserts`, so
they do not narrow. After `assert.notEqual(x, 'TIMEOUT')`, `x` keeps its full
union → cast (`(x as JsonResponse).json`). (needs-you.test.ts cap test)

## `require-await` on an await-free node:test callback → drop `async`

A `test('...', async (t) => { ...sync only... })` with no `await` trips
`require-await`. Fix: drop `async`. node:test accepts a sync `(t) => void`
callback; a thrown assertion fails the test identically to a rejected promise.
Do NOT reach for `Promise.resolve()` padding here — there is no async contract
to honor on a test body. (spawn-setup.test.ts:420)

## `noPropertyAccessFromIndexSignature` → bracket a property off an index sig

`core.snapshot().settings.repo_setup` — `repo_setup` comes from an index
signature on `settings`, so it must be `settings['repo_setup']`. Named optional
props (e.g. `env.FLEETDECK_SETUP_CMD`) are unaffected; only index-signature
members need brackets. Behavior-identical. (spawn-setup.test.ts:176)

## `JsonResponse.json` is `unknown` → one named facet per read

Every `res.json.<x>` needs a cast to a locally-declared facet interface naming
exactly the fields the assertions read (RegisterAck, PermissionHoldResponse,
UpsResponse, StopBlockResponse, QuestionPayload, DismissAck, MailListResponse).
Convention: cast `as Facet | null` when a null-guarding `?.` must stay legal
(`(reg.json as RegisterAck | null)?.callsign`); cast `as Facet` for a direct
read (`(res.json as DismissAck).ok`). `(await getJson(...)).json` casts
`as StateResponse` from contracts. (needs-you.test.ts)

## `noUncheckedIndexedAccess` → guard the first element even after a length check

`qs[0]`, `firstFourPending[0]`, `promises[0]` are all `T | undefined`. A prior
`assert.equal(qs.length, 1)` does NOT narrow the index access, so add
`const q = qs[0]; assert.ok(q);` (or `const oldestId = arr[0]?.id;
assert.ok(oldestId)`). These added asserts are guaranteed-pass given the
preceding length/emptiness assertion → behavior-preserving. (needs-you.test.ts)

## `no-unnecessary-condition` / `prefer-nullish-coalescing` on dead fallbacks

- `state.questions || []` and `(state.questions || []).find(...)` — `questions`
  is a REQUIRED `QuestionEntry[]`, so `|| []` is dead → drop it.
- `.json.mail || []` where `mail: unknown[] | undefined` — `prefer-nullish-
  coalescing` wants `?? []`; arrays are always truthy so `||`/`??` are identical
  here. (needs-you.test.ts)

## `no-unnecessary-condition` on redundant null/undefined guards — NEAR-EQUIV

`assert.ok(q.id !== undefined && q.id !== null, ...)` where `q.id: string`
(always defined) → both comparisons are constant-true, flagged. Replaced with
`assert.ok(q.id, ...)`. FLAG: near-equivalence — the original also passed for
`''`; `assert.ok('')` is falsy, so the new form rejects an empty-string id. IDs
are non-empty in practice, so no live behavior change. (needs-you.test.ts x2)

## `restrict-template-expressions` bans null/undefined in `${...}`

`${deliveryChannel}` (`string | null`) → `${String(deliveryChannel)}`;
`` `Bearer ${daemon.token}` `` (`string | null`) → `${String(daemon.token)}`.
`String(null) === 'null'` reproduces the original runtime string exactly.
`String()` of a genuinely nullable value is NOT double-flagged by
`no-unnecessary-type-conversion` (the arg is not already a string). numbers in
templates are allowed (config: `allowNumber: true`), so `${expectedCount}` etc.
stay bare. (needs-you.test.ts)

## `no-unnecessary-type-conversion` → drop `String()` around a string

`String(x.id) === String(q.id)` where both `id: string` → flagged; dropped to
`x.id === q.id`. Behavior-identical under the string-typed id contract.
(needs-you.test.ts, several `.find` predicates)

## `no-confusing-void-expression` → brace single-expression void arrows

`setTimeout(() => resolve('TIMEOUT'), 1500)` → `setTimeout(() => {
resolve('TIMEOUT'); }, 1500)` (resolve returns void). Same for a Promise
executor returning the setTimeout handle: brace the executor body too.
(needs-you.test.ts cap test)

## `no-unsafe-return` → brace an arrow that returns `any`

`assert.doesNotThrow(() => JSON.parse(text), ...)` — `JSON.parse` returns `any`,
so the arrow returns `any` → flagged. Braced to discard:
`() => { JSON.parse(text); }`. `doesNotThrow` only observes throw/no-throw, so
identical. (needs-you.test.ts malformed-body test)

## `loadFixture` tokens arg has no `token` key → excess-property error

`FixtureTokens` is `{ session_id?; session?; cwd? }`. A stray
`loadFixture(name, { token: daemon, session_id, cwd }, ...)` is an
excess-property error. Removed `token: daemon` at 4 call sites; behavior-
identical (the fixture token substituter only reads session_id/session/cwd; the
handle was already ignored). (needs-you.test.ts)

## Local helper `findSession` re-typed to assert-internally (sibling of findCard)

`function findSession(state: StateResponse, sid): SessionEntry` now asserts the
row is present and returns it, so call sites reach `.col`/`.lastTool` directly
(the raw `.find()` is `SessionEntry | undefined`). Consequence: the Notification
test's `assert.ok(card, 'session card should exist')` is now logically
redundant (card is non-null). KEPT — `no-unnecessary-condition` inspects
conditional positions, not `assert.ok(...)` call args, so it does not fire; kept
for faithfulness to the original. (needs-you.test.ts)

## `questionsFor` optional `kind` param — keep `!kind`

`function questionsFor(state, sid, kind?: string)` — `!kind` stays meaningful
(`kind` may be undefined) and is NOT flagged: `strict-boolean-expressions` is
off, and `no-unnecessary-condition` sees `kind` as genuinely optional. Only the
`state.questions || []` fallback inside it was dropped. (needs-you.test.ts)


---

### ⇩ merged from `ts-migration-bugs.tests.G.md`

<!-- STAGING for the test-conversion phase (task #11), "Group G" batch:
     tests/{plans,worktrees,derive-audit-reliability}.test.mjs -> *.test.ts under the full
     strict + strictTypeChecked + stylisticTypeChecked gate. MY files alone (kept separate
     from ts-migration-bugs.tests.md to avoid a concurrent-append race). Merge newest-first
     into ts-migration-bugs.md when the phase lands. Entries are strict-typing CONSEQUENCES
     of the conversion, terse, newest-first. -->

### `no-unnecessary-condition` vs `noUncheckedIndexedAccess`: a genuine two-rule standoff, resolved by narrowing once   [MIGRATION CONSEQUENCE — derive-audit-reliability]

- **The standoff.** For a value that is legitimately `T | undefined` — either an indexed read
  under `noUncheckedIndexedAccess` (`snap.conflicts[0]`) or a `.get()` result
  (`afterCleanup`, `adopted`) — tsc DEMANDS `?.` on every access, but
  `@typescript-eslint/no-unnecessary-condition` flags the `?.` on the SECOND (and later)
  consecutive access to that same stable reference as "unnecessary optional chain on a
  non-nullish value". The FIRST access in the run is never flagged; only the repeats are.
  So `foo?.a` passes and the next line's `foo?.b` errors — you cannot satisfy both rules by
  toggling the one `?.`.
- **Why removing `?.` is wrong.** Dropping the flagged `?.` to appease eslint immediately
  reintroduces the tsc error it was there to silence (`Object is possibly 'undefined'`).
  Round-trip trap: the two gates disagree about the exact same token.
- **Fix (the choke-point narrow).** Bind the value to a `const`, `assert.ok(it, '…')` once,
  then use plain `.a`/`.b` everywhere after. The assert narrows `T | undefined` -> `T` for the
  whole tail, so tsc is happy (no possibly-undefined) AND eslint is happy (no `?.` at all).
  Behaviour-faithful: the `.mjs` accessed the fields unconditionally too, so an assert that the
  row/element exists is exactly the implicit precondition the original relied on.
    - `snap.conflicts[0]?.rel_path` + `…?.sessions`  ->  `const survivor = snap.conflicts[0]; assert.ok(survivor, …); survivor.rel_path; survivor.sessions` (x2: M-B4, R2-6).
    - `afterCleanup?.status` + `…?.archived_at`  ->  `assert.ok(afterCleanup, …)` then `.status`/`.archived_at`.
    - `adopted?.status` + `adopted?.body?.reason`  ->  `assert.ok(adopted, …)` then `.status` / `.body?.reason` (`.body` stays optional -> its `?.` is real and unflagged).

### `@typescript-eslint/prefer-includes` autofix rewrites `/str/.test(x)` -> `x.includes(str)`, which then trips `prefer-optional-chain`   [MIGRATION CONSEQUENCE — derive-audit-reliability]

- Ticker assertions filtered/some'd on `/tmux server died/.test(x.msg)` etc. `x.msg` is
  `string | null`. Under the PostToolUse format hook (`prettier --write` then `eslint --fix`),
  `prefer-includes` (in `strictTypeChecked`) AUTOFIXES the plain-substring `.test()` into
  `.includes()` — so `.includes` is the terminal, stable form, not a style choice.
- But `x.msg.includes(…)` on a nullable string needs a null guard, and the obvious
  `x.msg != null && x.msg.includes(…)` is itself flagged by `prefer-optional-chain`. Terminal
  form that survives both the autofixer and the linter: **`x.msg?.includes(…)`**. The predicate
  return becomes `boolean | undefined`, which `.some`/`.filter` treat identically to `false`
  for a nullish msg — same elements selected as the original `.test()` (all the anchors are
  plain substrings, no regex metacharacters), so assertion semantics are unchanged.

### `adoptSession` is hand-declared 3-arg on the core surface but the `.mjs` called it with one   [MIGRATION CONSEQUENCE — derive-audit-reliability]

- `derive.ts`'s `SpawnsSurface` types `adoptSession(sid, opts, meta)` with all three required,
  whereas the impl (`spawns.ts`) defaults `body = {}` and `{ deferred = false } = {}`, so the
  `.mjs` `core.adoptSession('adopt-unknown')` type-errored as "Expected 3 arguments, but got 1".
- Faithful call: `adoptSession('adopt-unknown', { dangerously_skip_permissions: false }, { deferred: false })`.
  These reproduce the runtime defaults exactly — `dangerously_skip_permissions === true` is the
  only arm trigger (`skip=false` takes the same unarmed path as `body={}`), and `deferred:false`
  matches the defaulted meta — so the UNKNOWN-lookup 503 path is identical to the 1-arg call.

### `revived.body.spawn_id` is optional on `ReviveResult` -> narrow before use as SQL param and as a row source   [MIGRATION CONSEQUENCE — derive-audit-reliability]

- `const newId = revived.body.spawn_id` is `string | undefined`. It is then passed to
  `prepare<StatusRow>(…).get(newId)` (arg must be `SqlValue`, so `undefined` is rejected) and its
  result field is read. Fix: `assert.ok(newId, 'a 200 revive minted a fresh spawn_id')` right
  after the 200 assertion (narrows the param to `string`), and read the row as
  `.get(newId)?.status` (the `.get()` result is `StatusRow | undefined`). The `.mjs` assumed
  both — a 200 revive always carries a `spawn_id`, and the row always exists — so the asserts
  just make the precondition explicit. (x2: H-R5, R2-5.)

### Smaller strict-typing consequences worth the pattern (Group G)

- **`noPropertyAccessFromIndexSignature` (TS4111) on `process.env`.** `process.env.PATH` ->
  `process.env['PATH']`; and because `restrict-template-expressions` allows only string/number
  in a template, the interpolation is `${process.env['PATH'] ?? ''}` (derive) /
  `${String(process.env['PATH'])}` (worktrees) — `env[k]` is `string | undefined`. Same TS4111
  applies to every dot-access on the `SqlRow` index signature.
- **`prepare<Row>` generic is mandatory for dot-accessed rows.** The sqlite seam defaults
  `prepare<R = SqlRow>` and `SqlRow = Record<string, SqlValue>` is an index signature, so
  `.get().status` is a TS4111 dot-access-on-index-signature error. Every row shape gets an
  explicit interface and `prepare<StatusRow>(…)` / `prepare<ColRow>(…)` etc. (~41 sites in
  derive alone). Side benefit: the row field types are then real (`status: string`,
  `archived_at: number | null`) instead of `SqlValue`.
- **Fake tmux adapter -> core adapter via the documented double-cast.** The test's
  `FakeTmuxAdapter` is a deliberately narrow stand-in; bridging it to the core's
  `CoreTmuxAdapter` param uses `tmux.adapter as unknown as CoreTmuxAdapter` (4 sites) — the
  permitted `as unknown as X` sibling idiom, not a suppression. No `: any`/`as any`/`@ts-*`/
  `eslint-disable`/`!` anywhere in the three files.


---

### ⇩ merged from `ts-migration-bugs.tests.H.md`

<!-- STAGING for the test-conversion phase (task #11, wave-2 group H): tests/*.test.mjs ->
     *.test.ts under the full strict + strictTypeChecked + stylisticTypeChecked gate. Merge into
     ts-migration-bugs.md (newest-first) once the phase lands; kept separate to avoid a
     concurrent-append race on the shared log. Group H = fleet-bugs, tmux-adapter, mdns.
     Entries are migration CONSEQUENCES of strict typing (authoring friction with a general
     rule), newest-first. -->

### tsc and eslint DISAGREE on a repeated `arr[0]?.x`: tsc needs the `?.`, eslint calls it unnecessary — the fix is a captured, `assert.ok`-narrowed const   [MIGRATION CONSEQUENCE — the sharpest gate tension in the whole file; mdns]

- **What breaks.** Under `noUncheckedIndexedAccess`, `msg.answers[0]` is `DnsRecord | undefined`, so
  the FIRST `msg.answers[0]?.data` needs its optional chain (tsc: object is possibly undefined).
  But the moment you touch the same element-access reference a SECOND time in the same flow
  (`msg.answers[0]?.flush`, then `?.class`, …), TS flow-analysis has refined that reference to
  non-nullish, so `@typescript-eslint/no-unnecessary-condition` fires on every repeat with
  "Unnecessary optional chain on a non-nullish value" — while tsc still demands the `?.` on the
  first one. You cannot satisfy both by adding or by removing `?.`: the first occurrence wants it,
  the rest forbid it. Observed exactly: line N (`?.data`) clean, N+1/N+2 (`?.flush`/`?.class`)
  flagged; same shape on `answers[0]`, `txt.answers[0]`, `sends[0]`.
- **Why both tools agree on the types yet disagree on the verdict.** eslint here runs type-aware
  via `parserOptions.projectService`, so it reads the SAME tsconfig (`noUncheckedIndexedAccess`
  on). The divergence isn't a config mismatch — it's that tsc reports *possibly-undefined access*
  (a type error) and only on the first read, whereas no-unnecessary-condition reports *provably
  non-null optional chain* (a lint smell) on the flow-refined repeats. Same type facts, opposite
  direction of complaint.
- **Fix (the one form that satisfies both).** Capture the element once, narrow it, then use the
  narrowed local for every field: `const rec = msg.answers[0]; assert.ok(rec); rec.data; rec.flush;`.
  A fresh `const` gets the *declared* `T | undefined` type (flow-refinement doesn't carry into a
  new binding), so `assert.ok(rec)` is a meaningful condition to BOTH tools (not flagged), and all
  subsequent `rec.x` reads carry zero `?.`. This is the same idiom already used for `srv0/txt0/a0`
  in the same file — proof it lints clean. Rule of thumb: **two or more field reads off one
  `arr[i]` → capture-and-`assert.ok`, never a chain of `arr[i]?.a` / `arr[i]?.b`.** (A single read
  can keep the lone `?.` — it's the first-and-only, so neither tool objects.)

### `String(x)` on a value that is ALREADY `string` is a lint error, not a harmless belt-and-braces   [MIGRATION CONSEQUENCE — recurs at every typed callback param; mdns]

- **What breaks.** Defensive `logs.push(String(m))` / `downs.push(String(reason))` in `log`/`onDown`
  callbacks. Because the source types those callbacks (`log?: (message: string) => void`,
  `onDown?: (reason: string) => void`), `m`/`reason` are already `string`, so
  `@typescript-eslint/no-unnecessary-type-conversion` flags "Passing a string to String() does not
  change the type or value of the string" — 15 sites here.
- **Fix.** Drop the wrapper: `logs.push(m)`. Keep `String(x)` ONLY where the operand is genuinely
  not-yet-a-string: `String(err)` where `err: unknown` (in the `errText` helper) is fine, and
  `${String(r.typeName)}` / `${String(r.ttl)}` on `string | undefined` / `number | undefined` is
  fine (the union is not pure `string`, and `restrict-template-expressions` needs the coercion for
  the `undefined` arm). The rule is precise: it fires on pure-`string` arguments only.
- **Watch the indentation when batch-fixing.** An `old_string` that includes the leading whitespace
  (`        downs.push(String(reason));`, 8-space, inside a `for`) will NOT match the 6-space
  sibling elsewhere in the file — a `replace_all` silently leaves the odd-indent occurrence behind,
  and it resurfaces on the next lint pass. Match on the bare statement or fix per-site.

### `String(recordData)` in a failure diagnostic trips `no-base-to-string`; use `JSON.stringify`   [MIGRATION CONSEQUENCE — diagnostic-only, behaviour-preserving; mdns]

- **What breaks.** A debug template `` `${String(r.data)}` `` where `r.data` is the codec's
  `RecordData` union (`string | string[] | Record<string,string> | SrvData | Buffer`).
  `@typescript-eslint/no-base-to-string`: "'r.data' may use Object's default stringification
  ('[object Object]') when stringified" — correct, the object arms would render `[object Object]`.
- **Fix.** `` `${JSON.stringify(r.data)}` ``. `JSON.stringify` returns `string` (clean for
  `restrict-template-expressions`), is defined for every union arm, and yields a STRICTLY more
  useful diagnostic than the `.mjs`'s implicit `[object Object]`. It appears only inside an
  `assert.ok(x, <msg>)` failure string, so pass/fail behaviour is identical — a diagnostic-quality
  change, not a semantic one.

### Consuming an unexported source shape: derive it from the function signatures, don't re-declare it   [MIGRATION CONSEQUENCE — mdns]

- **What breaks.** `scripts/fleetd/mdns.ts` keeps `DnsRecord` / `DecodedMessage` / `Question`
  file-local (unexported). The converted test needs those exact types to annotate its collections
  and helpers, but must NOT force new `export`s on the source (a source edit that has to ship in the
  same commit — see the termbridge entry) when a pure derive suffices.
- **Fix.** Reconstruct from the exported functions' own signatures, so the test's types are, by
  construction, exactly what the codec produces/consumes:
  `type DnsRecord = ReturnType<typeof buildAnnouncement>[number];`
  `type DecodedMessage = NonNullable<ReturnType<typeof decodeMessage>>;`
  `type Question = ReturnType<typeof parseQuestions>[number];`. Same idiom already in
  `tests/fleetd-audit-regressions.test.ts`. Only reconstruct a shape by hand (`interface SrvData`)
  when it is a nested rdata arm no exported signature surfaces directly. **Zero source changes,
  zero bundle impact.**

### A `decodeMessage(): T | null` seam wants a one-line asserting helper, not `?.` everywhere   [MIGRATION CONSEQUENCE — mdns]

- **What breaks.** The codec's `decodeMessage(buf): DecodedMessage | null` is called ~40× and every
  call site then reads `.answers` / `.questions`. Propagating the `| null` through each site
  (`decodeMessage(x)?.answers`) is noise and re-triggers the repeated-`?.` tension above.
- **Fix.** A local `const decode = (buf): DecodedMessage => { const m = decodeMessage(buf);
  assert.ok(m, 'expected a decodable DNS message'); return m; };`. `assert.ok` is `asserts value`,
  so the helper returns the non-null type and every call site is clean. Same move for the tri-state
  socket helpers: `bindShared(): Promise<Socket | null>` stays honest, and each caller does
  `assert.ok(asker, '...')` (one call site — the PTR-browse test — GAINED an `assert.ok(asker)` the
  `.mjs` lacked; behaviour-preserving, since the original would have thrown on the null deref one
  line later anyway).

### `no-confusing-void-expression` bans BOTH `return t.skip(x)` and the bare void arrow `() => sock.start()`   [MIGRATION CONSEQUENCE — recurs across every skip and every void-returning stub; mdns]

- **What breaks.** Two idioms the `.mjs` used freely: (1) `if (!port) return t.skip('reason');` —
  `t.skip()` returns `void`, so `return t.skip(...)` is "Returning a void expression from an arrow
  function"; (2) `assert.doesNotThrow(() => mdns.start())` where `start()` returns `void` — the
  arrow-shorthand "confusingly returns a void".
- **Fix.** (1) split to a statement + bare `return`: `{ t.skip('reason'); return; }`. (2) give the
  arrow a block body: `() => { mdns.start(); }`. Do NOT block-body a NON-void arrow — `() =>
  mdns.update(x)` returns `boolean`, and `t.after(() => mdns.stop())` / `() => close(sock)` return
  `Promise<void>`; those keep the shorthand (they're not void expressions). The tell is the callee's
  return type, not the syntax.

### `noPropertyAccessFromIndexSignature`: dot access on a `Record<string,string>` is a tsc error, bracket is the fix (and eslint's `dot-notation` allows it)   [MIGRATION CONSEQUENCE — mdns]

- **What breaks.** `(rec.data as Record<string, string>).path` → tsc TS4111 "Property 'path' comes
  from an index signature, so it must be accessed with ['path']".
- **Fix.** `(rec.data as Record<string, string>)['path']`. `@typescript-eslint/dot-notation`
  (stylistic) does NOT then demand dot-notation back: it detects `noPropertyAccessFromIndexSignature`
  and permits bracket access for index-signature members. Same interplay already relied on by
  `tmux-adapter.test.ts`'s `process.env['X']`.

### Injecting a fake `node:dgram`: cast `as unknown as typeof dgram`, and DON'T name the local `dgram`   [MIGRATION CONSEQUENCE — mdns]

- **What breaks.** The socket-egress tests inject a hand-rolled fake socket via `createMdns({ inject:
  { dgram } })`. A partial fake is not structurally assignable to the full `typeof import('node:dgram')`,
  and naming the fake `const dgram = ...` shadows the real import so the very `typeof dgram` cast that
  types it resolves to the fake.
- **Fix.** Build the fake object, then `{ createSocket: () => socket } as unknown as typeof dgram`
  (two-step cast — the only sanctioned escape hatch, no `any`), and bind it to a non-colliding local
  (`fakeDgram`). Empty stub method bodies must carry a comment (`setMulticastTTL() { /* no-op */ }`)
  or `@typescript-eslint/no-empty-function` fires. When the `.mjs` fake used `this.iface`, convert to
  a closure `let iface: string | undefined` — avoids typing `this` on an object literal, single
  socket so behaviour is identical.

### Smaller strict-typing consequences worth the pattern (group H)   [MIGRATION CONSEQUENCE]

- **`as const` only where a widened literal breaks a shorthand.** `for (const phase of ['probing',
  'announced'] as const)` so `{ phase }` types as the `'probing' | 'announced'` union the source
  wants. A DIRECT literal (`uniqueConflict(msg, opts, { phase: 'probing' })`) needs NO `as const` —
  contextual typing already narrows the argument. Only the loop-variable case needs it.
- **`(r.ttl ?? 0) > 0` for a relational compare on an optional number.** `r.ttl` is `number |
  undefined`; a bare `r.ttl > 0` is a tsc error and `??` is required by `prefer-nullish-coalescing`
  (over `||`). Equality is fine as-is: `r.ttl === 0` / `r.flush === false` need no guard.
- **A `const` arrow used before its line → make it a `function`.** `close` was `const close = (s)
  => …` defined mid-file but referenced by tests above it. Deferred (inside test callbacks) usage
  does not trip TS2448 at runtime, but converting to `function close(s: Socket): Promise<void>`
  hoists it alongside its siblings `bindShared`/`collect`/`foreignResponderOn5353`, removes any
  ordering doubt, and is behaviourally identical.
- **Unused-but-required param → `_`-prefix, and know which linter honours it.** `waitFor(predicate,
  label, timeoutMs)` never reads `label`, but callers pass it positionally so it can't be dropped.
  `_label` silences tsc `noUnusedParameters` (tsc exempts leading `_`). eslint's `no-unused-vars`
  is `after-used` and does NOT honour `_`, but a middle param before a USED trailing one
  (`timeoutMs`) is exempt regardless — so `_label` clears both. (Contrast a `_`-prefixed TRAILING
  param, which eslint still flags — drop it instead.)
- **`restrict-template-expressions` (allowNumber): `${q.name}/${q.type}/class ${q.class}` is fine.**
  string + number only. Anything else needs `String(...)` (see the String() entries above) — but
  only when the operand isn't already a string/number.

---

### Carried from group H's earlier files (tmux-adapter.test.ts) — recorded here so the phase merge has them   [MIGRATION CONSEQUENCE]

- **`no-unnecessary-type-parameters` on a return-only generic.** A helper `readJson<T>(path):
  T` whose type parameter appears only in the return position is flagged (the generic is a
  disguised `as`, giving false safety). Fix: split into concrete-typed readers (one per call-site
  shape) rather than one lying generic.
- **`assert.ok(!x.ok)` narrows a discriminated result.** For a `KillResult = { ok: true; … } | {
  ok: false; error: string }`, `assert.ok(!x.ok)` narrows `x` to the error arm so `x.error` is
  reachable without a cast — `assert.ok`/`node:assert/strict` propagate discriminant narrowing.
- **`process.env['X']` (bracket) + `Reflect.deleteProperty(process.env, 'X')`.** `process.env` is
  an index signature: dot access is TS4111 (bracket required), and `delete process.env.X`
  (computed/dynamic-ish restore loops) trips `no-dynamic-delete` → use `Reflect.deleteProperty`. A
  STATIC `delete obj.optProp` on a declared-optional property is still fine.
- **`process.getuid?.() ?? 0`.** `process.getuid` is optional (undefined on Windows in the typings);
  call it optionally and coalesce, rather than asserting it exists.
- **`waitUntil(fn)` generic-callback rewrite.** A polling helper whose callback returned an untyped
  value needed its predicate typed `() => boolean` (or `() => T | undefined` with a narrowing
  return) so the awaited result isn't `any` — mirrors mdns's `collect().waitFor` inline typing.
- **Shell-command template byte-identity.** Where a test asserts on an exact spawned command string,
  the conversion must keep the template BYTE-for-byte (no `String()` insertions, no reflow of the
  literal) — the assertion is the contract; a formatter-reflowed or coercion-padded template is a
  silent behaviour change. (General cousin of the board-cluster "anchor on stable payload text"
  lesson.)


---

### ⇩ merged from `ts-migration-bugs.tests.I.md`

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


---

### ⇩ merged from `ts-migration-bugs.tests.J.md`

<!-- STAGING for test-conversion GROUP J (tests/{cli,daemon-maintenance,git-stderr-detail,
     fleetd-audit-regressions}.test.mjs -> *.test.ts) under the full strict +
     strictTypeChecked + stylisticTypeChecked gate. My file alone — do NOT concurrent-append
     the shared ts-migration-bugs.tests.md; merge this in (newest-first) once the phase lands.
     Entries are strict-typing CONSEQUENCES, not authoring friction. Two of these four files
     (git-stderr-detail, fleetd-audit-regressions) are security/audit surface — every
     redaction/leak/keep/token BYTE and every assertion verdict was diffed against HEAD and
     proven identical; the notes below record only where a strict rule FORCED a shape change and
     why that change preserves the verdict. -->

### A `string | null` DB column forces a narrowing assert before `assert.match`, and the narrowing IS the old throw   [MIGRATION CONSEQUENCE — security file, verdict-preserving]

- **What breaks.** `git-stderr-detail.test.ts` reads `fail_detail` back out of the daemon's
  SQLite by name. As `.mjs` the row was untyped, so `assert.match(detail, /…/)` compiled. Typing
  the row (`interface FailDetailRow { fail_detail: string | null }`, and the read `.get<FailDetailRow>()`)
  makes `detail: string | null`; `assert.match` demands a `string`, so tsc rejects it.
- **Fix + equivalence.** Insert `assert.ok(detail !== null)` (and one `assert.ok(out !== null)`)
  immediately before each `.match`. This is verdict-identical to the `.mjs`: on the real path
  `detail` is always the recorded string, so the assert is a no-op and the same `.match` runs;
  on the impossible null path the `.mjs` threw `ERR_INVALID_ARG_TYPE` from `assert.match(null,…)`
  and the `.ts` throws `AssertionError` from `assert.ok(false)` — both FAIL the test, neither
  passes. The narrowing only relocates the failure a line earlier and changes its error class.
- **Same pattern, `.find()` edition.** `fleetd-audit-regressions.test.ts` derefs `send`,
  `packet`, `aRecord`, `goodbye`, `firstGoodbye` — each a `T | undefined` from `.find()` /
  `decodeMessage(): Decoded | null` / an index. The `.mjs` derefed them bare; the `.ts` adds
  `assert.ok(send, '…')` etc. before each use. Same equivalence: the reachable path never trips
  the assert (a preceding `assert.equal(goodbyeSends.length, 1)` even guarantees the index), and
  the unreachable path fails either way (TypeError vs AssertionError).

### `packet?.answers.length > 0` is exactly `packet !== null && packet.answers.length > 0` — the explicit form is the type-narrowing one   [MIGRATION CONSEQUENCE]

- `strict`+`noUncheckedIndexedAccess` on `decodeMessage(): Decoded | null` means the optional
  chain `packet?.answers.length > 0` still leaves `packet` as `Decoded | null` in the following
  `&&` clauses, so `packet.answers.every(…)` errors. Rewrote to `packet !== null &&
  packet.answers.length > 0 && packet.answers.every(…)`. Value-identical: when `packet` is null
  the `.mjs` computed `undefined > 0` (→ `false`) and the `.ts` computes `false` directly; when
  non-null both evaluate the same three conjuncts. The `!== null` guard is what actually narrows
  the union for the `.every` call the optional chain could not.

### `.filter(Boolean)` doesn't narrow `(Decoded|null)[]` → hand-write the type-guard predicate   [MIGRATION CONSEQUENCE]

- `records.map(decodeMessage).filter(Boolean)` stayed `(Decoded | null)[]` under TS (Boolean is
  not a narrowing guard), so downstream `.some`/`.answers` errored. Replaced with
  `.filter((packet): packet is DecodedMdnsMessage => packet !== null)`. Runtime-identical for
  this element type: `decodeMessage` only ever yields an object or `null`, and both `Boolean(x)`
  and `x !== null` drop exactly the nulls (no `0`/`''`/`NaN` in the array to diverge on). The
  predicate merely adds the static narrowing. (`DecodedMdnsMessage = NonNullable<ReturnType<typeof
  decodeMessage>>` recovers the non-exported decoder shape without forcing a source export.)

### `prefer-nullish-coalescing` rewrites `|| fallback` to `?? fallback`, and each site was proven non-divergent   [MIGRATION CONSEQUENCE — audit file]

- `strictTypeChecked` flags `a || b`. Converted three: `${env['NODE_OPTIONS'] ?? ''}`,
  `(map.get(a) ?? 0) + 1`, `(r.ttl ?? 0) > 0`. `||` and `??` differ only when the left operand is
  falsy-but-not-nullish (`''`/`0`/`false`). Each here is safe BECAUSE:
  - NODE_OPTIONS: the only divergent input is `''`, and `'' || '' === '' ?? '' === ''` — the
    fallback IS `''`, so the branch collapses.
  - the Map counter: divergent input is `0`, and `0 || 0 === 0 ?? 0 === 0`; it is then `+1`'d the
    same, and the map is write-once-then-increment so it never even stores `0`.
  - `r.ttl`: divergent input `0` gives `0 > 0` either way, and undefined gives `0 > 0`(??) vs
    `undefined > 0`(||) — both `false`.
  When the fallback and the falsy value coincide, `||`→`??` is a no-op; that is the only reason it
  was allowed to land in a byte-for-behavior file.

### `.map(JSON.parse)` → `.map((line): T => JSON.parse(line) as T)` is behavior-identical (the index arg was never a reviver)   [MIGRATION CONSEQUENCE]

- `strictTypeChecked` (`no-unsafe-*`) rejects the point-free `.map(JSON.parse)` because it returns
  `any`. The explicit arrow types the row (`MdnsLogItem`). Equivalence rests on a JS detail:
  `.map` calls `JSON.parse(value, index, array)`, and `JSON.parse`'s 2nd parameter is `reviver`,
  which is honoured only if it's a **function** — the numeric `index` is ignored. So point-free
  and explicit forms parse identically. (Same rewrite recurs at every JSONL read in the audit file.)

### `RegExp.test(x)` on a union member needs a `typeof … === 'string'` guard; the guard is always-true under the preceding discriminant   [MIGRATION CONSEQUENCE — audit file, verdict-preserving]

- `answer.data` is a union across mDNS record types; `no-base-to-string`/the string-arg
  requirement made `/…/.test(answer.data)` error. Added `typeof answer.data === 'string' &&`
  ahead of it. Verdict-preserving because the clause is already gated by `answer.typeName === 'PTR'`,
  and PTR `data` is always the target-name string — so the `typeof` is always true on the
  reachable path. On a hypothetical non-string `data` the `.mjs` would `String()`-coerce inside
  `.test` and the exact `^Fleet Deck [0-9a-f]{6}\._fleetdeck\._tcp\.local$` anchor would still
  not match, and the `.ts` short-circuits to `false` — same result. Same class:
  `detail.match(/\[redacted\]/g)?.length` gained a `?.` (`String.match` is `… | null`);
  match-count is always 5 on the real path, and the never-taken no-match path fails either way
  (`null.length` TypeError vs `assert.equal(undefined, 5)` AssertionError).

### Deliberately-malformed inputs: derive the param type and funnel junk through `unknown`, never `any`   [MIGRATION CONSEQUENCE]

- `cli.test.ts` feeds `healthPidIsOurDaemon` / `healthIsOurManagedDaemon` partial/junk bodies
  (`{}`, `{ pid: 'not-a-number', managed: true }`, `{ managed: 0, … }`) to prove they reject them.
  `Health` is file-local (not exported) in `bin/fleetdeck.ts`, so I recovered the exact param type
  with `type HealthArg = Parameters<typeof healthPidIsOurDaemon>[0]` and a boundary caster
  `const asHealth = (h: unknown): HealthArg => h as HealthArg`. Each junk literal goes through
  `asHealth({…})` — the runtime object is passed verbatim (the cast erases), so the guards still
  see the same malformed input; bare `null` args were left untouched. Companion:
  `tmuxVersionCapability('unknown').reason` needed `Extract<ReturnType<typeof tmuxVersionCapability>,
  { available: false }>` to reach `.reason` on the `available:false` arm of the discriminated
  union — a pure type assertion, zero runtime change.

### `noUncheckedIndexedAccess` tuple loops: annotate the table `as [string, string][]` (recurs)   [MIGRATION CONSEQUENCE]

- `for (const [a, b] of [ [x, y], … ])` over an inline literal infers `string[][]`, so `a`/`b`
  come out `string | undefined`. Annotating the literal `] as [string, string][])` (cli.test:
  two agents-cmd tables; git-stderr-detail: the redaction `inputs` table, also hoisted to a typed
  `const inputs: unknown[]` / `const cases: {stderr;leak;secrets:string[]}[]`) makes the
  destructured elements `string` again. Type-only; the table CONTENTS (every secret/leak/keep byte)
  were diffed against HEAD and are identical.

### `no-confusing-void-expression`: `return t.skip(msg)` → `{ t.skip(msg); return; }` (recurs, all four files)   [MIGRATION CONSEQUENCE]

- `TestContext.skip()` returns `void`; `return t.skip(...)` returns a void expression, which the
  rule (at default) flags. Split into a statement + bare `return`. Semantically identical — skip
  the subtest, then stop — since node:test ignores a `TestFn`'s return value. Where the callback
  used `t`, its type comes from `import test, { type TestContext } from 'node:test'` and an
  explicit `(t: TestContext)`; where `t` was unused (cli.test serviceInstall) the param was
  dropped (`async () =>`), which stays assignable to node:test's `TestFn`.

### `noPropertyAccessFromIndexSignature` + `no-dynamic-delete` + `unbound-method`: env/kill/stdout mechanics   [MIGRATION CONSEQUENCE]

- **`process.env.X` → `process.env['X']`** everywhere (dot access on an index signature is
  banned); value-identical. Bracket access with a *variable* key was already fine.
- **`delete process.env[k]` / computed deletes → `Reflect.deleteProperty(process.env, k)`**
  (`no-dynamic-delete`); identical delete semantics.
- **`const write = process.stdout.write` → `.bind(process.stdout)`** (`unbound-method`) for the
  `token --rotate` stdout capture; the method is only ever called as `process.stdout.write(...)`,
  so binding its receiver changes nothing, and the bound value reassigns back cleanly in `finally`.
- **`process.kill(x.pid, …)` → `if (x.pid !== undefined) process.kill(x.pid, …)`** inside the
  existing `try { … } catch { /* already gone */ }` cleanups: `ChildProcess.pid` is `number |
  undefined`; on a spawned child it is always set, and the catch already swallowed a dead-pid
  throw, so guarding the undefined case (which cannot occur here) is verdict-neutral.

### Boundary casts that carry NO runtime effect (type-only)   [MIGRATION CONSEQUENCE]

- `(srv.address() as net.AddressInfo).port` (address() is `AddressInfo | string | null`; a bound
  TCP server is always `AddressInfo`); `new Promise<number>`; `JSON.parse(readFileSync(pkg)) as {
  engines: { node: string } }`; `.get<PragmaColumnInfo>()` / `.all<…>()` DB generics;
  `.find(…)`-result narrowed via `assert.ok(execLine !== undefined, …)` before `.match`. All
  erase at emit.

### Two `.mjs` COMMENT-reference decisions (the rule: don't repoint prose)   [PROCESS NOTE — one revert made]

- The task says comment/prose references to `.mjs` are NOT repointed (only imports are). The
  daemon-maintenance / fleetd-audit files correctly LEFT their cross-references
  (`spawn.test.mjs`, `fleet-bugs.test.mjs`, `mdns.test.mjs`, `scripts/fleetd/*.mjs`,
  `payload-redaction.test.mjs`) as `.mjs`, and cli.test left `agents-poll.mjs` / `fleetdeck.mjs` /
  the `'fleetdeck.mjs'` path const and the `fleetd.mjs` argv fixture as `.mjs`.
- A stray repoint in `git-stderr-detail.test.ts` — a comment citing `tests/payload-redaction.test.mjs`
  — was reverted back to `.mjs` for rule-compliance and consistency with the identical reference
  in daemon-maintenance (even though payload-redaction is in fact now `.ts`; the rule says leave
  comment refs alone regardless).
- KEPT (flagged, not a cross-reference): each file's own first-line self-identity banner was
  updated to its real new name (`// tests/cli.test.ts`, `// tests/git-stderr-detail.test.ts`) —
  a banner naming a now-deleted `.mjs` file would be wrong; this matches the convention already in
  the landed `cli.test.ts`. Import specifiers to `../bin/fleetdeck.ts` / `../bin/tmux-version.ts`
  use string-literal dynamic-import (not `new URL(...)`) so tsc resolves the module types; the
  CLI-path const stays `fleetdeck.mjs` because `bin/fleetdeck.mjs` is the real entry the source
  (`UNIT()`/`SUPERVISE()`) still emits.

### String quote-restyle by the biome PostToolUse hook is value-identical (proven, not assumed)   [MIGRATION CONSEQUENCE — security file]

- The formatter renormalizes single-quoted-with-escapes to double-quoted where that drops
  backslashes: `'…the \'"\'"\' idiom'` → `"…the '\"'\"' idiom"` (cli.test shQuote message) and
  `'Cloning into \'x\'…'` → `"Cloning into 'x'…"` (git-stderr-detail error input). Decoded both
  literals with node and confirmed byte-identical runtime values (and the shQuote EXPECTED value
  `` `'/it'\''s/a path'` `` is untouched). Called out here because in a redaction test a silently
  altered string literal would be a real security regression — it was verified, not trusted.

### Dead import dropped: `appendFileSync`   [MIGRATION CONSEQUENCE]

- `fleetd-audit-regressions.test.mjs` imported `appendFileSync` from `node:fs` but never called it
  (0 body references, confirmed against HEAD). `noUnusedLocals` flags it; removed from the import.
  Behavior-neutral.


---

### ⇩ merged from `ts-migration-bugs.tests.md`

<!-- STAGING for the test-conversion phase (task #11): tests/*.test.mjs -> *.test.ts under
     the full strict + strictTypeChecked + stylisticTypeChecked gate. Merge into
     ts-migration-bugs.md (newest-first) once the phase lands; kept separate only to avoid a
     concurrent-append race on the shared 140k log. Entries are migration CONSEQUENCES of
     strict typing, plus a couple of genuine "the test read/eval'd JS and the source is TS
     now" breakages that are real bugs, not authoring friction. -->

### A converted test that consumes real source objects forces the source to export its types — and the export must land in the SAME commit   [MIGRATION CONSEQUENCE + a split-commit gap that broke HEAD tsc]

- **What breaks.** `tests/termbridge-capture-race.test.ts` drives the real
  `createTermBridge` and collects the frames it emits. As `.mjs` it just pushed
  frames into an untyped `const frames = []`. Strict `.ts` wants the element type,
  so the test now imports the frame union — `import { createTermBridge, type TermFrame }
  from '../scripts/fleetd/termbridge.ts'` — to type `frames: TermFrame[]` and an
  `Extract<TermFrame, { t: 'out' }>` narrowing. But `TermFrame` was a *file-local*
  `type` in termbridge.ts, so the import failed tsc (TS2459/2305: imports a
  non-exported member). Fix: `type TermFrame =` → `export type TermFrame =`.
- **Why it's a real bug, not just friction.** The export edit is a SOURCE change,
  and it got split from the test: an earlier batch committed the converted test but
  left the one-line `export` uncommitted in the working tree. HEAD therefore had a
  test importing a non-exported type — a red tsc gate hiding in a "green tests" run
  (node --test type-strips and never sees it). Rule: when a conversion adds
  `export` to a source symbol, that source hunk belongs in the SAME commit as the
  test that needs it, or the intervening HEAD is broken. Caught here by diffing the
  stray `M scripts/fleetd/termbridge.ts` against its consumer before committing.
- **Bundle impact: none.** `export type` emits zero runtime code (esbuild strips
  it), so the committed `fleetd.bundle.mjs` is byte-identical — no rebundle despite
  the `scripts/fleetd/` touch. Verified the bundle contains no `TermFrame` token.
  (Generalizes: exporting a *type* from daemon source never dirties the bundle;
  exporting a *value/function* for a test to import would, and would need
  `npm run bundle`.)

### board-util.test drags DOM-typed `board/src/util.ts` into the DOM-less root program → 14 tsc errors   [FINAL-GATE BLOCKER — straddles board ownership, coordinate with comet before fixing]

- **What breaks.** The full-repo gate `tsc -p tsconfig.json --noEmit` reports exactly
  **14 errors, all in `board/src/util.ts`** (`Cannot find name 'Element' /
  'HTMLTextAreaElement' / 'ClipboardEvent' / 'Clipboard' / 'Permissions' /
  'PermissionDescriptor' / 'window' / 'document'`, and `Property 'clipboard' does not
  exist on type 'Navigator'`). They are NOT any test's authoring fault and NOT a
  board bug: `util.ts` is the board's clipboard/OSC-52 helper and legitimately uses
  DOM globals.
- **Why it lands in the ROOT program.** Root `tsconfig.json` is Node/Bun-only
  (`lib: ["ES2023"]`, no DOM) and `exclude`s `board`. But `tests/board-util.test.ts`
  *statically* imports symbols from `../board/src/util.ts` (lines 21–39). tsc follows an
  import into an excluded file — `exclude` only bars a file from being a *root*, not
  from being pulled in transitively — so `util.ts` gets type-checked under the DOM-less
  root lib and its DOM globals are undefined. `board/tsconfig.json` (comet-owned) DOES
  carry `lib: ["ES2023","DOM","DOM.Iterable"]`, so `util.ts` checks clean *there*; it
  just never checks the tests. The board-cluster tests fall in the gap between the two
  configs.
- **Scope is narrow.** Only `util.ts` and only `board-util.test.ts` are involved. The
  other four board-cluster tests import DOM-free board modules
  (`markdown.ts`/`qr.ts`/`staleChunks.ts`/`termDiag.ts`) and check clean under the root
  lib. So the per-test conversion gate is unaffected — anchor tsc on the specific test
  filename and these 14 `board/src/**` lines are provably not yours.
- **Recommended fix (deliberate, coordinated — NOT done here).** Add a dedicated
  DOM-aware program for the board-cluster tests: a new `tsconfig.board-tests.json`
  (extends root, overrides `lib` to add `DOM`/`DOM.Iterable`, drops `board` from
  `exclude`, `include: ["tests/board-*.test.ts"]`) and add `tests/board-*.test.ts` to the
  root `exclude`. Final gate then runs both `tsc -p tsconfig.json` and
  `tsc -p tsconfig.board-tests.json`. Caveat to verify when doing it: that program mixes
  Node test files (need `types:["node","bun"]`) with DOM board source, so watch for the
  `setTimeout`/`fetch`/`Blob` node×DOM global overlap; `skipLibCheck` covers the .d.ts
  side but user-code assignments could still collide. All three touch points
  (root tsconfig, the new config, `tests/board-*.test.ts`) are in the test/migration lane,
  but the *boundary* is board-adjacent, so flag comet before landing it.

### board-util.test: a source-slice `new Function()` eval broke because the sliced source became TypeScript   [GENUINE MIGRATION BUG — the test's oracle stopped parsing]

- **What broke.** `tests/board-util.test.ts` (was `.mjs`) pins the OSC 52 clipboard provider
  by reading `board/src/components/TermPane.tsx`, slicing out the `clipboardProvider` factory
  by source markers, and `new Function(...)`-eval'ing that slice so the assertions run against
  the SHIPPED code, not a re-implementation. Once TermPane converted to `.tsx`, the slice is
  now TypeScript — `const clipboardProvider = (term: Terminal): IClipboardProvider => ({ ... })`
  — and `new Function()` parses its body as plain JS, so it threw `SyntaxError: Unexpected
  token ':'` on the `: Terminal` / `: IClipboardProvider` annotations. The test that guarded
  the provider silently could not even construct it.

- **Fix.** Strip the annotations before the eval with Node's own type-stripper:
  `import { stripTypeScriptTypes } from 'node:module'` then
  `const body = stripTypeScriptTypes(src.slice(start, end))` before `new Function(...)`.
  `stripTypeScriptTypes` default mode ('strip') handles type ANNOTATIONS (which is all the
  slice contains — no enums/namespaces/param-properties), so it degrades the TS slice to the
  exact JS the old `.mjs` test used to slice directly. This is the same mechanism Node uses to
  run the `.ts` sources at all, applied at the granularity of a source substring.

- **Why this generalizes.** ANY test that reads product source as text and re-evaluates a
  fragment of it (rather than importing it) now has to strip types first, because the fragment
  is TS. Importing the symbol would avoid this, but here the point of the test is to eval the
  exact shipped bytes with a spy injected for its one free identifier (`copyText`), which an
  import cannot do. So `stripTypeScriptTypes` is the right seam, not a workaround.

- **eslint tension it creates.** `strictTypeChecked` flags the legitimate eval with
  `@typescript-eslint/no-implied-eval` AND `@typescript-eslint/no-unsafe-call` (the
  `new Function()` result is a `Function`-typed value). Both are correct in general and wrong
  here: the "input" is a file on disk in this repo, and the eval IS the test. Resolved with a
  single scoped `// eslint-disable-next-line @typescript-eslint/no-implied-eval,
  @typescript-eslint/no-unsafe-call` carrying a justification comment — the only disable
  directive in the file. (Contrary to an earlier guess, `no-implied-eval` DOES cover
  `new Function` under this config; it is not limited to string `setTimeout`/`setInterval`.)

### Reflow-caused vacuous assertion: a biome-reformatted anchor made a security assert pass on nothing   [MIGRATION CONSEQUENCE — the PostToolUse formatter moved the string the test grepped for]

- **What broke.** `board-util.test.ts` asserts a REFUSED copy keeps the selection by locating
  the error branch via `src.indexOf("flash('err', 'the clipboard refused")`. After TermPane
  converted and the PostToolUse biome hook reflowed it, the `flash('err', ...)` call wraps
  across lines, so that exact substring no longer exists — `indexOf` returned -1, the "no
  `clearSelection` within the next 200 chars" check ran over an empty slice, and the assertion
  passed VACUOUSLY (green while guarding nothing).
- **Fix / rule of thumb.** Anchor source-grep assertions on the STABLE payload text
  (`'the clipboard refused'`), never on call-site punctuation (`flash('err', '...`) that a
  formatter is free to rewrap. Every readFileSync-anchor in the file was re-reconciled against
  the converted source for the same reason (casts the migration added: `clip?.writeText`,
  `(active as {...})?.focus`, `(globalThis as {...}).__fdCopy`, `ok: ran && proof.fired`,
  `(term as TermModesView).modes`, `(health as TermHealth|null)?.auth`,
  `clipboardProvider = (term: Terminal): IClipboardProvider =>`).

### Awaitless `async` stubs vs `@typescript-eslint/require-await`   [MIGRATION CONSEQUENCE — recurring across converted tests]

- Test DOM/clipboard doubles were written `async () => granted` / `async () => {}` for shape
  parity with the browser APIs they stand in for. `strictTypeChecked`'s `require-await` flags
  every `async` function with no `await`. Rewrite each as an explicit
  `() => Promise.resolve(...)` (e.g. `permissions.query`, `clipboard.writeText/readText`, the
  `__copySpy`): same return type (`Promise<T>`), no lint violation. This recurs anywhere a
  converted test stubs a promise-returning API without awaiting inside the stub.

### Smaller strict-typing consequences worth the pattern (board cluster)

- **`noUncheckedIndexedAccess` on regex captures / `queue.shift()` / `arr[0]`.** `m[1]` is
  `string | undefined`; `queue.shift()` is `T | undefined`; `one[0].msg` needs `one[0]?.msg`.
  Fixes: `m[1] ?? ''`, an `if (name === undefined) continue;` guard after `shift()`, and `?.`.
  Note the tuple exception: annotating a table as `[string, string][]` (not letting it infer
  `string[][]`) makes destructured `[a, b]` come out `string`, not `string | undefined` — so
  `const PRETTY: [string | null | undefined, string][]` and
  `const expectations: [string, string][]` were the minimal fix for the loops over them.
- **Inferred-union member access.** `helpText.ts`'s `ORCH_COMMANDS` is an un-annotated array
  whose entries infer to a union of `{syntax;does;chip}` and `{syntax;does}`; `x.chip` errors
  on the chip-less members. Narrow with `'chip' in c ? c.chip : undefined` + an
  `if (chip === undefined) continue;`.
- **`no-dynamic-delete`.** A restore loop doing `delete globalThis[k]` (computed key) is
  flagged; use `Reflect.deleteProperty(globalThis, k)`. A STATIC `delete obj.__fdCopy` is fine
  provided the property is optional in its type.
- **`no-unused-vars` is `after-used`, and does NOT honour a `_` prefix in this config.** A
  `_`-prefixed param BEFORE the last used one is silently fine (that's why `(_x, y) => ...`
  passes), but a `_`-prefixed TRAILING param (`(x, _y) => x % 3`) is still flagged. tsc's
  `noUnusedParameters` exempts `_` names; eslint here does not. For a mask function that only
  needs its first arg, drop the trailing param entirely — `(x) => x % 3 === 0` stays assignable
  to `(x: number, y: number) => boolean` (fewer params is assignable). [board-qr.test]
- **`@typescript-eslint/no-empty-function` on DOM stubs.** Empty method bodies (`focus() {}`,
  `preventDefault() {}`, …) on test doubles are flagged. A shared
  `const noop = (): void => { /* stub */ };` referenced from each slot (the comment makes
  `noop` itself pass the rule) is cleaner than commenting every body. [board-util.test]
