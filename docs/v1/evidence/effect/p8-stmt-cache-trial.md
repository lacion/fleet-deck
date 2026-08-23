# P8.5 — bun:sqlite `Database.query()` statement-cache trial

**Purpose.** Inventory the daemon's repeated/static prepared SQL, measure
`Database.query()` (bun's compile-and-cache) against the current compile-once
`prepare()` map, and record whether any callsite is worth changing. This is an
evidence slice: **no daemon callsite changed**.

**Provenance / how to read this.** Behavioural fixtures in
[`tests/effect/sqlite-stmt-cache-trial.test.ts`](../../../../tests/effect/sqlite-stmt-cache-trial.test.ts)
open scratch DBs only (`:memory:` or `mkdtemp`). They never touch the daemon's
store. Benchmarks used scratch WAL files (SELECT, daemon-like) and `:memory:`
(writes / boot). Daemon source (`sqlite.ts`, `db.ts`, `statements.ts`,
`questions.ts`) is unchanged.

**Section inventory:** §0 identity · §1 verdict · §2 what `query()` is ·
§3 corpus inventory · §4 cache semantics · §5 `close(true)` interaction ·
§6 benchmark · §7 recommendation · §8 later path · §9 fixture inventory.

---

## 0. Identity

| Field | Value |
| --- | --- |
| Date | 2026-08-23 |
| Bun | `1.3.14` (`0d9b296af33f2b851fcbf4df3e9ec89751734ba4`) |
| Host | AMD Ryzen 9 9950X3D, Linux 6.6.87.2 WSL2 |
| Product branch | `fd/p8-stmtcache` @ `351b376f` (`feat(effect): own the SQLite lifetime as the Store root service`) |
| Workspace | `/tmp/fd-wt-stmtcache` |
| Daemon source | **untouched** — `SqliteHandle` still has `prepare` only; wrap `Set` still tracks `handle.prepare` |
| Bundle | **not rebuilt** |

---

## 1. Verdict

**KEEP prepare-once.** Do not add `query()` to `SqliteHandle`. Do not change any
callsite.

`Database.query()` is not a free faster `prepare()`. At Bun 1.3.14 it is a
**20-slot first-20-win cache** (not LRU; `Database.MAX_QUERY_CACHE_SIZE`
defaults to 20 and is a writable static, absent from `bun-types`). The daemon
compiles **112** distinct `q` statements at boot — 92 past the cache. A
`query()`-cached statement the wrap `Set` does **not** track is the P8.3
finalize-then-`close(true)` hole: occupancy ≤20 is silently finalized by bun on
`close(true)`; occupancy ≥21 leaves overflow `sqlite3_stmt`s live and
`close(true)` throws `database is locked`, even after `finalizeStatements()`
has run over the prepare-only Set.

On the hot path, holding a compile-once `Statement` and looking up a cached
`query()` are a **wash** (~2 µs SELECT on WAL; SQLite execution dominates).
`prepare`-per-call + `finalize` is ~3×; overflow `query()`-per-call is ~6–8×
**and leaks**. The current `q` map already is the winning strategy.

---

## 2. What `query()` is (and is not)

From `bun-types` 1.3.14 `Database`:

- `query(sql)` — "the same as `prepare` except that it caches the compiled
  query" (`sqlite3_prepare_v3`). Does not execute.
- `prepare(sql)` — "does not cache the compiled query". Always a new
  `Statement`.
- Capacity, eviction, and `close` interaction are **not documented** in
  `bun-types`. Runtime exposes `Database.MAX_QUERY_CACHE_SIZE` (default `20`,
  writable). Post-1.4 bun.sh copy describes LRU; **1.3.14 is first-20-win**,
  pinned below.

It is **not**:

- A replacement for `statements.ts`'s `q` object. The `q` map already holds one
  `Statement` per SQL for the handle's life.
- Tracked by `wrap()`. `DriverHandle` / `SqliteHandle` have `prepare` only.
  `wrap.prepare` inserts into a `Set`; `finalize()` deletes; `finalizeStatements()`
  finalizes the Set then clears. A raw `db.query()` never enters that Set.

---

## 3. Corpus inventory

Every production prepare goes through `SqliteHandle.prepare` (so the wrap Set
sees it). There is **no** `db.query(` in `src/daemon`.

| Source | Prepares | Lifetime | Distinct SQL | `query()` candidate? |
| --- | ---: | --- | ---: | --- |
| `statements.ts` `q` map | **112** keys | compile-once per handle (`createStatements` WeakMap) | **112** (no duplicates) | **No.** 112 ≫ 20. Holding `prepare()` is the same speed as a cache hit. |
| `updateSession` shape cache | 1 template, `Map<shape, Statement>` | compile-once per column-shape (M-P8; was the historical hot-path re-prepare) | one `UPDATE … col = ?` per updater path | **No.** Already cached. `query()` would key the same SQL string. |
| `questions.ts` compile-once `q` | **7** | compile-once per `createQuestions` | 7 | **No.** Same as `q`. |
| `questions.ts` `rearmPending` | 1 ephemeral | `prepare` + `run` + `finalize` per expiry | 1 static `UPDATE` | Microscopic. Rare. Wrap has no `query()`. Keep finalize so the Set does not leak a stmt per expiry. |
| `questions.ts` `purge` | 1 ephemeral | `prepare` + `run` + `finalize` per Clear | 1 static `DELETE` | Same as rearm. |
| `db.ts` boot pragmas | **4** | once per open; live until wrap `finalizeStatements` | `PRAGMA table_info(sessions/mail/spawns)`, `PRAGMA user_version` | **No.** Once per boot. Already tracked. |

`q.*` call sites (**303** `.run`/`.get`/`.all` in 15 modules):

| Module | Calls |
| --- | ---: |
| `spawns` | 69 |
| `questions` | 42 |
| `retention` | 42 |
| `derive` | 41 |
| `mail` | 22 |
| `events` | 20 |
| `settings` | 16 |
| `snapshot` | 12 |
| `worktrees` | 10 |
| `repos` | 9 |
| `commands` | 6 |
| `plans` | 5 |
| `ledger` | 4 |
| `ingest` | 3 |
| `files` | 2 |

Hottest keys: `getSession` 45, `questions` `q.get` 19, `setSetting` 17,
`setSpawnStatus` 16. All of those are already compile-once.

`updateSession(` callers: 41 across `spawns`/`derive`/`events`/`retention`/
`ingest`/`http`/`commands`. Map-cached; not a remaining re-prepare.

`settings.ts:704` `handler.prepare` is a **config handler**, not sqlite.

No remaining hot-path re-prepare.

---

## 4. Cache semantics (Bun 1.3.14, empirical)

Pinned by `tests/effect/sqlite-stmt-cache-trial.test.ts`.

| Behaviour | Finding |
| --- | --- |
| Cache key | Exact SQL string. Interned equal content hits (`'SELECT ' + '1 AS n'`). Whitespace / newline / case / comment miss. |
| `prepare` identity | Always a new object, even for identical SQL. `query() !== prepare()` for the same SQL. |
| Capacity | First **20** distinct `query()` SQLs win. Slot 0 of 40 still hits; index 20+ misses. Touching a late SQL does **not** evict slot 0 → **not LRU**. |
| Bind aliases | `query(sql)` returns the **same object**. Omit-after-bind is shared across aliases. `finalize()` on one alias kills all; the next `query(sql)` is a new object. |
| `SELECT *` schema freeze | `ALTER TABLE … ADD COLUMN` does not refresh a cached `SELECT *`. Requery hits the frozen stmt. `finalize()` then `query()` sees the new column. |
| `MAX_QUERY_CACHE_SIZE` | Runtime default 20, writable. Raising to 200 made 50/50 hits and `close(true)` succeed in a probe. **Do not raise it** — process-global, untyped, and it would paper over wrap not tracking `query()`. |

---

## 5. `close(true)` interaction (load-bearing)

P8.3/P8.4 invariant: the store owner calls `finalizeStatements()` then
`close(true)` (`sqlite3_close`). A still-live `sqlite3_stmt` makes that throw
`database is locked`.

| Occupancy | `close(true)` | After |
| --- | --- | --- |
| 1, 19, 20 live `query()` | **succeeds** | bun finalizes the cache (`Statement has finalized` on a previously-live stmt) |
| 21, 32, 112 live `query()` | **throws** `database is locked` | first 20 finalized; **index 20 stays live** |
| 21 `query()`, JS refs dropped | **still throws** | GC of the JS wrapper does not `sqlite3_finalize` overflow stmts |
| Finalize the overflow stmt, then `close(true)` | succeeds | recovery path |

Wrap-Set replica (finalize only `prepare()`, leave `query()` untracked) — the
P8.3 hole:

| Left untracked | After `finalize(prepares)` + `close(true)` |
| --- | --- |
| 1 `query()` | **OK** (bun finalizes the first-20 cache) |
| 21 `query()` | **LOCKED** |
| 1 live `prepare` + 1 `query`, no finalize | **LOCKED** (the prepare) |

`openDatabase` wrap (production path):

- 112 `prepare()`s, no `finalizeStatements()` → `close(true)` locked.
- Then `finalizeStatements()` → `close(true)` OK. Same for the four boot
  pragmas.

**If `q` (112 keys) switched to untracked `query()`:** 92 overflow stmts, wrap
Set empty, `finalizeStatements()` is a no-op, `close(true)` throws. That is
the invariant break. Tracking `query()` in the Set would make the bun cache
redundant with the `q` object we already hold.

---

## 6. Benchmark

Method: `Bun.nanoseconds()`, warmup discarded, median of repeats. SELECT on a
scratch **WAL file** (daemon-like). UPDATE / INSERT / ephemeral / boot on
`:memory:` (WAL-file write benches timed out under the 90s probe budget;
ranking is the same). Representative shapes: `getSession`-like `SELECT *`,
`last_seen` UPDATE, `events` INSERT, questions `rearmPending` UPDATE.

### 6.1 SELECT `getSession`-like — WAL file

Two independent runs (warmup 3k–5k, 25k / 40k iters, 5 repeats). Median ns/op:

| Strategy | Run A (40k) | Run B (25k) |
| --- | ---: | ---: |
| prepare-once-reuse (current `q`) | 1866 | 2186 |
| query-once-reuse (hold `query()`) | 1837 | 1860 |
| query-per-call (in first-20 cache) | 1854 | 1875 |
| prepare-per-call + finalize | 6601 | 5484 |
| query-per-call overflow (21st SQL) | 11736 | 14742 |

A third WAL SELECT (15k iters) reproduced the wash: prepare-once 1975,
query-once 2006, query-per-call cached 1879.

### 6.2 Writes and boot — `:memory:` (warmup 800, 4000 iters, 3 repeats)

| Strategy | SELECT | UPDATE | INSERT |
| --- | ---: | ---: | ---: |
| prepare-once-reuse | 1345 | 999 | 1800 |
| query-once-reuse | 1340 | 979 | 1804 |
| query-per-call cached | 1339 | 931 | 1814 |
| prepare-per-call + finalize | 5658 | 2465 | 3788 |

Overflow SELECT (400 iters, leaks `sqlite3_stmt`): **14647 ns/op**.

Ephemeral `rearmPending` shape (1500 iters):

| Strategy | ns/op |
| --- | ---: |
| prepare + run + finalize (current) | 2563 |
| query + run, no finalize (after warmup = cache hit) | 593 |

The ephemeral gap is real and expected: current path recompiles every call;
`query()` hits the 20-slot cache. It is **not** a reason to expose `query()` —
rearm/purge are rare, wrap would have to grow a tracking hole, and the first
21 distinct untracked SQLs break `close(true)`.

Boot compile 112 distinct `SELECT n` (12 repeats, ns/stmt): prepare 2869,
query 2835. A wash, and `query()` of 112 then `close(true)` throws.

### 6.3 Reading the numbers

SQLite execution dominates. Cache lookup vs holding a `Statement` is noise
(~0–10%). The strategies that recompile (prepare-per-call, overflow query)
lose by 3–8× and, for overflow, leak. 112 `q` keys will not fit in 20 slots,
so a naive `query()` swap is the overflow column, not the cache-hit column.

---

## 7. Recommendation

**KEEP prepare-once.** Specifically:

1. Do **not** add `query()` to `SqliteHandle` / `DriverHandle`.
2. Do **not** switch `wrap.prepare` to `handle.query`. Two `prepare(same SQL)`
   would alias one cached `sqlite3_stmt` in the Set; `finalize()` of one would
   kill the other. `q` relies on compile-once objects it already holds.
3. Do **not** raise `MAX_QUERY_CACHE_SIZE` to cover 112 keys. Untyped,
   process-global, first-20-win-vs-LRU docs drift, and it would hide wrap not
   tracking `query()`.
4. Leave the two questions ephemerals as `prepare` + `finalize`. They are
   already correct against the wrap Set. Caching them is a sub-microsecond
   rare-path win that requires growing the P8.3 hole.
5. Leave the four boot pragmas as `prepare` (tracked until `finalizeStatements`).

The expected outcome in the P8.5 brief is confirmed by data, not assumed.

---

## 8. Later path (not this slice)

Revisit only if **all** of these become true:

- bun documents and pins cache capacity + eviction (and it is LRU or ≥112).
- wrap tracks **every** compiled stmt, including `query()`, **or** the owner
  stops using `close(true)` (it must not).
- A new hot path re-prepares the same SQL without going through `q` /
  `updateStmts`.

Until then the seam stays `prepare()` + wrap Set + `finalizeStatements()` +
`close(true)`. P8.6/P8.7 do not depend on this cache.

---

## 9. Fixture inventory

File: `tests/effect/sqlite-stmt-cache-trial.test.ts`. Scratch DBs only. No
ns/op asserts (those live here).

| Fixture | What it pins |
| --- | --- |
| seam has no `query()`; wrap tracks only `prepare()` | `SqliteHandle` / `handle.prepare` / runtime `typeof h.query` |
| corpus prepares | 112 unique `q` keys; 113 prepared SQL strings in `statements.ts`; 9 `questions.ts`; 4 `db.ts`; 303 `q.*` calls; `rearmPending`/`purge` finalize; `updateStmts` Map; no `db.query(` |
| `MAX_QUERY_CACHE_SIZE` | default 20 |
| cache key / prepare identity | exact SQL; interned hit; ws/case/comment miss; prepare always distinct |
| first-20-win, not LRU | slots 0..19 hit after 40 fills; 20+ miss; touching last does not evict 0 |
| alias bind state | omit-after-bind shared; finalize one alias kills all; requery is new |
| `SELECT *` schema freeze | ALTER + cached requery misses new column; finalize then requery sees it |
| `close(true)` occupancy | 1/19/20 OK (cache finalized); 21/32/112 `database is locked` |
| overflow recovery | index 20 live; finalize it then `close(true)` succeeds |
| dropped JS refs | 21 query() still lock `close(true)` |
| wrap-Set replica 1 vs 21 | 1 untracked query OK; 21 lock |
| wrap 112 prepares | locked until `finalizeStatements` |
| wrap 4 boot pragmas | same |
| mix prepare + query | locked because of the prepare |
