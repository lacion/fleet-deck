# P8.7 — `@effect/sql-sqlite-bun@4.0.0-rc.110` trial

**Purpose.** Independently trial `@effect/sql-sqlite-bun@4.0.0-rc.110` against Fleet Deck's
frozen SQLite seam, and record the resulting **KEEP direct bun:sqlite** decision. Plan default
was KEEP. Adopt only if the named properties MATCH Fleet Deck intent **and** measured value
justifies a third production dependency. They do not.

**Provenance / how to read this.** Installed-source pins plus scratch-DB fixtures in
[`tests/effect/sql-sqlite-bun-trial.test.ts`](../../../../tests/effect/sql-sqlite-bun-trial.test.ts)
and the microbench
[`scripts/effect-migration/p8-sql-client-bench.ts`](../../../../scripts/effect-migration/p8-sql-client-bench.ts).
Fixtures never touch the daemon store. Daemon source (`sqlite.ts`, `db.ts`, `statements.ts`,
`store-owner.ts`, the 303 `q.*` sites) is unchanged. The client is an exact **devDependency**
so `MAX_DIRECT` stays 2.

There is no separate `@effect/sql` package at v4. `SqlClient` lives in `effect/unstable/sql`
inside `effect`. Trial sources import `@effect/sql-sqlite-bun/SqliteClient` (and stable
`effect/*`) only — they do **not** import `effect/unstable/sql/*` or `@effect/sql`.

**Section inventory:** §0 identity · §1 verdict · §2 what the client is · §3 Fleet Deck frozen
contract · §4 semantics table · §5 benchmarks · §6 structural cost · §7 recommendation ·
§8 ADOPT-later path · §9 fixture inventory.

---

## 0. Identity

| Field | Value |
| --- | --- |
| Date | 2026-08-23 |
| Bun | `1.3.14` (`0d9b296af33f2b851fcbf4df3e9ec89751734ba4`) |
| Host | Linux 6.6.87.2-microsoft-standard-WSL2 x86_64 |
| Cohort | `effect@4.0.0-rc.110` / `@effect/platform-bun@4.0.0-rc.110` |
| Trial package | `@effect/sql-sqlite-bun@4.0.0-rc.110` exact **devDependency** (peer `effect@^4.0.0-rc.110`) |
| Production `dependencies` | still exactly those two — `direct.count === 2` |
| Product branch | `fd/p8-sqltrial` @ `351b376f` (`feat(effect): own the SQLite lifetime as the Store root service`) |
| Workspace | `/tmp/fd-wt-sqltrial` |
| Daemon source | **untouched** |
| Bundle | **not rebuilt** |
| Unstable register | **unchanged** — trial does not import `effect/unstable/sql/*` |
| migration-ledger P8 row | left **Not started** (same as P8.2) |

---

## 1. Verdict

**KEEP direct bun:sqlite.**

WAL, `busy_timeout = 5000`, writable `BEGIN IMMEDIATE`, and the absence of streaming **do**
match numbers Fleet Deck already sets itself. That is not a reason to adopt. The client is
per-statement Effect wrapping (P8.6 forbids wrapping each statement), it cannot express the
frozen P8.4 `prepare()` + `finalizeStatements()` + `close(true)` protocol, it has no
`.run()` `{changes, lastInsertRowid}` surface the corpus uses, it hard-codes `BEGIN IMMEDIATE`
with no config override (migrate is frozen on deferred `BEGIN`), and adopting it is a third
production dependency whose measured value on the hot path is a ~4× Effect-machinery tax on
cheap point lookups and noise on disk-bound writes.

The plan already records this outcome as a successful P8: Effect owns the store lifetime
(P8.3/P8.4) while the 303 sync `q.*` leaves stay sync TS via `Effect.try` around a coarse
operation. P8.7 does not require switching SQL clients and does not block P8 continuation.

---

## 2. What the client is (rc.110)

Installed source: `node_modules/@effect/sql-sqlite-bun/src/SqliteClient.ts`.

The module header states the named properties this trial was asked to verify: a serialized
semaphore, WAL unless `disableWAL`, a five-second blocking busy timeout, writable
`BEGIN IMMEDIATE`, no streaming queries, busy waits block the event loop because `bun:sqlite`
is synchronous.

Load-bearing implementation (line numbers of the installed file):

| Site | Behaviour |
| --- | --- |
| `new Database(options.filename, { readonly, readwrite, create })` | Opens bun:sqlite. No `strict` / `safeIntegers` constructor flags. |
| `Effect.addFinalizer(() => Effect.sync(() => db.close()))` | Close with **no** `throwOnError`. bun:sqlite default is `sqlite3_close_v2`. |
| `options.busyTimeout ?? Duration.seconds(5)` then `PRAGMA busy_timeout = ${busyTimeout}` | Default 5000 ms. `Duration.zero` → 0. Waiting blocks the event loop. |
| `if (options.disableWAL !== true && !readonly) db.run("PRAGMA journal_mode = WAL;")` | WAL unless disabled or readonly. |
| `const statement = db.query(sql)` then `statement.all(...params)` | Cached statements. Never `prepare()`. Never `finalize()`. Execute always `.all()`, including INSERT. |
| `executeStream` → `Stream.die("executeStream not implemented")` | `.stream` dies; it does not fail in the error channel. |
| `Semaphore.make(1)` | One permit. |
| Ordinary acquirer | `semaphore.withPermits(1)(Effect.succeed(connection))` — permit wraps **acquire of the connection object**, not execute. |
| Transaction acquirer | `semaphore.take(1)` + `Scope.addFinalizer(scope, semaphore.release(1))` — permit held until the transaction scope closes. |
| `beginTransaction: "BEGIN IMMEDIATE"` | Hard-coded in `Client.make`. `SqliteClientConfig` has **no** `beginTransaction` override. |
| `SafeIntegers` | Read from fiber context per execute; default false (INTEGER stays `number`). |

`SqliteClientConfig` is `filename` plus optional `readonly` / `create` / `readwrite` /
`disableWAL` / `busyTimeout` / `spanAttributes` / `transform*`. No close-mode, no
prepare-vs-query, no begin-SQL, no `strict`.

A tagged-template statement **is** an `Effect`. Queries compile to positional `?` via
`makeCompilerSqlite` (`SELECT * FROM t WHERE id = ${1}` → `['SELECT * FROM t WHERE id = ?', [1]]`).
That bind style MATCHES the corpus. The return type does not: execute returns the `.all()`
array, so `INSERT` yields `[]`.

---

## 3. Fleet Deck frozen contract (unchanged)

| Invariant | Where | Frozen shape |
| --- | --- | --- |
| Driver | `src/daemon/sqlite.ts` | `wrap(new Database(file))` — no constructor options |
| Statement API | `SqliteStatement` | positional `run(...params)` / `get` / `all`; `.run()` returns `{ changes, lastInsertRowid }` |
| Lifetime | `SqliteHandle` + `store-owner.ts` | `prepare()` tracks live statements; owner `finalizeStatements()` then `close(true)` (P8.4) |
| PRAGMAs | `db.ts` `PRAGMAS` | `busy_timeout = 5000`, `journal_mode = WAL`, at open, **outside** transactions |
| Migrate | `db.ts:497` | `db.exec('BEGIN')` — deferred, then `PRAGMA user_version`, COMMIT/ROLLBACK |
| App transactions | mail, derive, worktrees, events, settings | `db.exec('BEGIN IMMEDIATE')` — five sites |
| Call sites | 15 modules | **303** `q.<stmt>.(run\|get\|all)` — sync, positional `?` only |
| `lastInsertRowid` | `events.ts:945`, `questions.ts:446` | `Number(info.lastInsertRowid)` after `.run()` |
| `.changes` | retention, mail, spawns, questions, derive, worktrees | used as the "did this write land" signal |
| P8.5 | plan | inventory `db.query()` **per callsite**; change only proven sites |
| P8.6 | plan | `Effect.try` around a **coarse** sync DB operation; do **not** wrap each statement; never yield inside a direct SQLite transaction callback |

P8.2 already recorded **DO-NOT-ENABLE** for bun `strict` / `safeIntegers`. The sql client
does not flip those; INTEGER stays `number`. That MATCHES P8.2, it is not a reason to adopt.

---

## 4. Semantics table

Each named property was verified empirically (scratch DB) **and** against installed source.
Verdicts are MATCHES-INTENT or DIVERGES relative to Fleet Deck's frozen behaviour, not
relative to the client's own docs.

| Property | Result | Evidence |
| --- | --- | --- |
| WAL default | **MATCHES-INTENT** on disk | On-disk `PRAGMA journal_mode` → `wal`. `:memory:` reports `memory` after the same pragma (SQLite no-op; Fleet Deck's own `PRAGMAS` string does the same). `disableWAL: true` → `delete`. |
| `busy_timeout` 5s | **MATCHES-INTENT** | Default `PRAGMA busy_timeout` → `5000`. `busyTimeout: Duration.zero` → `0`. Source default `Duration.seconds(5)`. Fixtures pin the pragma; they do not sit in a 5s lock wait. Busy waits block the event loop on both sides (`bun:sqlite` is sync). |
| Writable `BEGIN IMMEDIATE` | **MATCHES-INTENT** for the five app txn sites; **DIVERGES** from migrate | SQL capture of `withTransaction`: `BEGIN IMMEDIATE`, body, `COMMIT`. A rival writer with `busy_timeout = 0` during a read-only `withTransaction` throws `database is locked`. Raw `BEGIN` (deferred) plus a SELECT does **not** block that writer; raw `BEGIN IMMEDIATE` does. `SqliteClientConfig` cannot select deferred `BEGIN`. Migrate is frozen on `db.exec('BEGIN')`. |
| Serialized semaphore | **MATCHES-INTENT** for `withTransaction` across an async gap; ordinary queries do **not** hold the permit during execute | Latch-ordered events: `tx-in`, `tx-out`, `q-done` (query issued after the txn started, completed after it ended). Source: txn acquirer `take(1)` until scope close; ordinary acquirer is `withPermits(1)(Effect.succeed(connection))`. JS single-thread + sync `bun:sqlite` is still the real serialization for single statements — same as Fleet Deck today. The client's ability to `yield*` inside `withTransaction` while holding `BEGIN IMMEDIATE` is the pattern P8.6 **forbids**. |
| No streaming | **MATCHES-INTENT** | Fleet Deck never streams rows. `sql\`SELECT 1\`.stream` dies (`Exit.hasDies`, squash `"executeStream not implemented"`); it does not fail in `SqlError`. |
| Close / statement lifetime | **DIVERGES** | Client: `db.query()` + `db.close()` with omitted arg. Fleet Deck: `prepare()` + `finalizeStatements()` + `close(true)`. Empirically `query()` + `close(true)` succeeds; a live `prepare()` + `close(true)` throws `database is locked` until `finalize()`. Observationally the client's cache does not trip `close(true)`, but the client cannot **express** the frozen protocol and still uses `sqlite3_close_v2`, which is the deferral P8.4 closed. |
| `db.query()` vs `prepare()` | **DIVERGES** from P8.4 / P8.5 | Blanket `query()` cache. P8.5 is per-callsite and not yet done. Ephemeral prepares (questions.ts) currently `finalize()` at the callsite. |
| Result shape | **DIVERGES** | `INSERT` → `[]`. No `.run()` `{changes, lastInsertRowid}`. Workaround is a second statement (`SELECT last_insert_rowid()` / `SELECT changes()`). INTEGER cells stay `number` (MATCHES P8.2). |
| Tagged-template binds | **MATCHES-INTENT** | Compiles to positional `?`. Corpus is positional `?` only (P8.2). |
| Async coloring | **DIVERGES** from P8.6 | Every statement is an Effect. P8.6: wrap the coarse sync op with `Effect.try`, not each statement. |
| Third production dependency | **not justified** | Trial-only exact devDep. Production ceiling stays 2. Plan: add this package only if the independent SQL gate chooses it. |

---

## 5. Benchmarks

Script: `scripts/effect-migration/p8-sql-client-bench.ts`. Scratch schema shaped like the
hot `statements.ts` keys (`getSession`, `visibleSessions`, `allSessions`, `insertEvent`,
`insertMail`). 64 sessions, 16 archived. Warmup 1000 discarded. Reads 10_000 iters, writes
2_000. Writes are auto-commit WAL on disk — ~4 ms/op on this WSL2 host — so the driver delta
is not the story there.

Three drivers share the same SQL text (bun `prepare()` once vs sql tagged templates on one
live `SqliteClient.layer`):

1. **bun:sqlite prepare** — Fleet Deck today (`get` / `all` / `run`).
2. **sql-sqlite-bun amortized** — one `runPromise`, `yield*` each statement (the style P8.6
   forbids).
3. **sql-sqlite-bun runSync/op** — same live client, `Effect.runSync` per statement.

Single run, bun 1.3.14, warmup discarded:

| shape | bun:sqlite prepare ns/op | sql amortized ns/op | sql runSync/op ns/op |
| --- | ---: | ---: | ---: |
| getSession | 1_682 | 7_187 | 7_400 |
| visibleSessions | 25_639 | 24_435 | 22_659 |
| allSessions | 27_239 | 30_497 | 27_402 |
| insertEvent | 4_085_690 | 3_958_413 | 6_698_667 |
| insertMail | 3_940_717 | 4_010_673 | 4_942_859 |

Load-bearing number: **getSession is ~4.3×** under the sql client (1.7 µs → 7.2 µs amortized,
7.4 µs runSync/op). That is Effect machinery on a point lookup that is already a prepared
statement. Multi-row scans sit in the same 22–30 µs band (noise / `query()` cache vs
`prepare()`). Auto-commit inserts sit in the same ~4 ms band; the extra Effect cost is
invisible next to WAL.

A 4× tax on the cheapest read is not "free", and it is not a reason to adopt. It is also not
the headline cost. The headline cost is coloring 303 sync sites.

Reproduce:

```sh
export PATH="$HOME/.bun/bin:$PATH"
timeout 120 bun scripts/effect-migration/p8-sql-client-bench.ts --out /tmp/fd-p8-sql-client-bench.json
```

---

## 6. Structural cost

Adopting the client is not a Layer swap behind `SqliteHandle`. It is a rewrite of the
statement API and of every callsite:

1. **303 sync `q.*` sites in 15 modules** become `yield*` (or `runSync`) Effects. Tripwire in
   the trial test. Modules: commands, derive, events, files, ingest, ledger, mail, plans,
   questions, repos, retention, settings, snapshot, spawns, worktrees.
2. **P8.6 contradiction.** The plan's conversion rule is `Effect.try` around a coarse
   synchronous DB operation. The sql client **is** per-statement Effect wrapping. Using it
   and following P8.6 cannot both be true.
3. **Yielding inside transactions.** `withTransaction` holds `BEGIN IMMEDIATE` + the
   semaphore across `Effect.sleep`. P8.6 forbids suspension inside a direct SQLite
   transaction callback so unrelated fibers cannot interleave on the same connection. The
   client's selling point for the semaphore is the pattern the plan rejects.
4. **`.changes` / `lastInsertRowid`.** `.run()` result is a first-class corpus API
   (`SqlRunResult`). The client returns `[]` from execute. Every `if (q.x.run(...).changes)`
   and both `lastInsertRowid` sites would need a follow-up `SELECT changes()` /
   `SELECT last_insert_rowid()` — extra statements, extra Effect coloring, extra failure
   modes.
5. **P8.4 close protocol cannot be expressed.** Store owner is
   `finalizeStatements(); close(true)`. The client finalizer is `db.close()`. Switching
   would reopen the `sqlite3_close_v2` deferral P8.4 just closed, unless Fleet Deck forked
   the client.
6. **P8.5 is per-callsite `query()`.** Adopting the client blanket-caches every statement,
   including ephemeral prepares that currently `finalize()` at the callsite.
7. **Migrate `BEGIN` vs hard-coded `BEGIN IMMEDIATE`.** Not configurable. Either migrate
   becomes IMMEDIATE (a lock-behaviour change on the user_version ladder) or migrate bypasses
   `withTransaction` (two transaction APIs).
8. **Third production dependency.** Plan budget: add `@effect/sql-sqlite-bun@4.0.0-rc.110`
   only if this gate chooses it. Ceiling is four direct Effect/platform packages only if
   **both** the SQL driver **and** one extracted native-platform package independently win.
   This gate does not win. Keeping the package as a devDependency preserves lock
   reproducibility for the fixtures without moving `direct.count`.

---

## 7. Recommendation: **KEEP direct bun:sqlite**

Load-bearing facts:

1. WAL + 5000 ms busy **already** match. The client sets the same pragmas Fleet Deck sets.
2. `BEGIN IMMEDIATE` matches five app sites and **diverges** from migrate `BEGIN`. Not
   configurable.
3. Semaphore serializes `withTransaction` across async gaps; ordinary execute is **not**
   under that permit. JS-thread sync sqlite remains the real single-statement mutex.
4. Streaming absence matches because Fleet Deck never streams; `.stream` dies.
5. **getSession 1682 → 7187 ns/op (~4.3×)** on the only shape where Effect overhead is
   visible. Writes are ~4 ms auto-commit WAL on this host — no measured win.
6. **303** `q.*` sites in **15** modules; `.changes` and `lastInsertRowid` are live APIs;
   P8.4 `close(true)` is live; P8.6 forbids per-statement wrapping.
7. Production dependency count stays **2**. The trial package is a pin, not an adoption.

Benefit for current code: **none** that Fleet Deck does not already have. Cost: a third
runtime package, an async-colored statement API, a different close/lifetime model, and a
forced IMMEDIATE migrate or a second transaction path.

---

## 8. ADOPT-later path (not this slice)

Re-open only if all of the following become true, then re-run this trial:

- P8.5 has a measured `query()` inventory and the remaining `prepare()` sites are explicit.
- P8.4's finalize-then-`close(true)` protocol is either expressible in the client (a
  `throwOnError` / finalize hook) or has been deliberately replaced with evidence that
  `query()` + `sqlite3_close_v2` cannot defer past the root finalizer under Fleet Deck's
  statement set.
- P8.6 is amended to allow per-statement Effects, **or** the client gains a sync
  `run`/`get`/`all` surface that returns `{changes, lastInsertRowid}`.
- `beginTransaction` is configurable, or migrate is separately proven under IMMEDIATE.
- The third production dependency independently wins the P2 ceiling / provenance gates
  (`REQUIRED_COHORT`, `MAX_DIRECT`, packed-install).
- Re-run `tests/effect/sql-sqlite-bun-trial.test.ts` and
  `scripts/effect-migration/p8-sql-client-bench.ts`; the 303-site tripwire must move in the
  same change as the first converted module.

Until then production stays `wrap(new Database(file))` and the 303 sites stay sync `q.*`.

---

## 9. Fixture inventory

File: `tests/effect/sql-sqlite-bun-trial.test.ts`. Scratch DBs only (`:memory:` or
`mkdtemp`). Semaphore / IMMEDIATE races use Promise latches, not `scaleMs` sleeps.

| Fixture | What it pins |
| --- | --- |
| production still has two Effect deps; the client is trial-only | `dependencies` = effect + platform-bun; sql-sqlite-bun exact **devDep**; daemon tree does not import it |
| installed rc.110 source pins | `Semaphore.make(1)`, ordinary acquirer, txn `take(1)`, `BEGIN IMMEDIATE`, busy 5s, WAL, `db.query`, `db.close()`, `Stream.die("executeStream not implemented")`, no `beginTransaction?` on config, no `.run(...params)` |
| Fleet Deck frozen close protocol | `prepare<R = SqlRow>(sql: string)`, `finalizeStatements()`, `close(true)`, PRAGMAs, migrate `BEGIN`, five `BEGIN IMMEDIATE` sites |
| on-disk WAL + busy 5000; `:memory:` journal_mode is memory | default layer pragmas |
| `disableWAL` / `Duration.zero` | `delete` / timeout 0 |
| `withTransaction` SQL capture | `BEGIN IMMEDIATE`, body, `COMMIT` |
| tagged templates compile to `?` | corpus bind style |
| read-only `withTransaction` takes the write lock | rival writer `database is locked`; deferred `BEGIN` does not |
| semaphore order | `tx-in`, `tx-out`, `q-done` |
| `executeStream` dies | Fleet Deck never streams |
| `close()` argument omitted | `sqlite3_close_v2` |
| `query()` vs `prepare()` + `close(true)` | P8.4 hazard: query cache does not trip; live prepare does |
| INSERT returns `[]` | no `changes` / `lastInsertRowid`; INTEGER stays `number` |
| 303 / 15 tripwire | adoption surface |

Bench: `scripts/effect-migration/p8-sql-client-bench.ts` (not a gate; records ns/op).
