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
