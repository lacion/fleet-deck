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
