# P8.2 — bun:sqlite `strict: true` trial

**Purpose.** Record the P8.2 trial of bun:sqlite `DatabaseOptions.strict` at Bun 1.3.14
against the daemon statement corpus, and the resulting **DO-NOT-ENABLE** decision.
This is bun's named-bind convention flag (since v1.1.14), **not** SQLite
`CREATE TABLE ... STRICT`. The trial does not enable `strict` on the daemon and
does not enable `safeIntegers`.

**Provenance / how to read this.** Dual-mode fixtures in
[`tests/effect/sqlite-strict-trial.test.ts`](../../../../tests/effect/sqlite-strict-trial.test.ts)
open scratch DBs only (`:memory:` or `mkdtemp`). They never touch the daemon's
store. Every behavioural fixture asserts both `strict: false` and `strict: true`
so the delta is the evidence. Daemon source (`sqlite.ts`, `db.ts`, `statements.ts`)
is unchanged.

**Section inventory:** §0 identity · §1 verdict · §2 what `strict` is · §3 corpus
inventory · §4 strict-vs-non-strict delta · §5 integers / `safeIntegers` ·
§6 statement lifetime · §7 recommendation · §8 ENABLE-later path · §9 fixture
inventory.

---

## 0. Identity

| Field | Value |
| --- | --- |
| Date | 2026-08-23 |
| Bun | `1.3.14` (`0d9b296af33f2b851fcbf4df3e9ec89751734ba4`) |
| `@types/bun` | `1.3.14` (`DatabaseOptions.strict` / `safeIntegers` since v1.1.14) |
| Product branch | `fd/p8-strict` @ `37e07659` (`docs(effect): close the P3 performance ledger item by adaptation`) |
| Workspace | `/tmp/fd-wt-strict` |
| Daemon source | **untouched** — `src/daemon/sqlite.ts` still `wrap(new Database(file))` |
| Bundle | **not rebuilt** |

---

## 1. Verdict

**DO-NOT-ENABLE.**

Zero corpus styles would break under `{ strict: true }` today. That is not a
reason to flip the flag. `strict` only changes **named** bind behaviour, and the
corpus has **zero** named SQL and **zero** object binds. The documented
"missing bound parameters will throw" tightening does **not** apply to the
positional `?` rest-args style the daemon actually uses: a zero-arg run of a
two-placeholder `INSERT` stores NULL in both modes.

Enabling it would still introduce a bun-only constructor option on a seam that
still has a `node:sqlite` fallback (P8.1), invert the documented `{$name}`
convention for any future named SQL, and open the same options object that
gates `safeIntegers` (the bigint landmine this trial is forbidden to flip).

---

## 2. What `strict` is (and is not)

From `bun-types` `DatabaseOptions` (v1.3.14):

- `strict: false | undefined` (default): missing named parameters do **not**
  throw; JS bind keys must **exactly match** the SQL prefix (`$name`, `:name`,
  `@name`).
- `strict: true`: missing named parameters throw `Missing parameter "name"`;
  JS bind keys are the **bare** name; the SQL stays prefixed.

It is **not**:

- SQLite `CREATE TABLE ... STRICT` (affinity enforcement). TEXT stored in an
  ordinary `INTEGER` column is accepted in both bun-strict modes; a SQLite
  STRICT table rejects it with `SQLiteError` in both modes.
- `safeIntegers`. `{ strict: true }` leaves INTEGER / `lastInsertRowid` as
  `number`. `{ safeIntegers: true }` (with or without `strict`) returns `bigint`.

The daemon open is argument-less:

```ts
makeHandle = (file) => wrap(new Database(file));
```

The wrapper's `SqliteStatement` is positional-only: `run(...params: SqlValue[])`.
Object binds are a type error at the seam even when bun:sqlite would accept them.

---

## 3. Corpus inventory

Prepared SQL is positional `?` only. No `$name` / `:name` / `@name` / `?NNN`
placeholders in the three modules that prepare statements.

| Source | Prepares | With `?` | `?` placeholders | Named | Numbered `?N` | Zero-placeholder |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `src/daemon/statements.ts` `q` map | **112** keys | 85 | 216 | **0** | **0** | 27 |
| `updateSession` shape cache | dynamic `UPDATE … col = ?` | rest-args | one `?` per column + `session_id` | **0** | **0** | — |
| `src/daemon/questions.ts` | 9 | 6 | 13 | **0** | **0** | 3 |
| `src/daemon/db.ts` pragma prepares | 4 | 0 | 0 | **0** | **0** | 4 |

`q.*` call sites (303 in 15 modules: `commands`, `derive`, `events`, `files`,
`ingest`, `ledger`, `mail`, `plans`, `questions`, `repos`, `retention`,
`settings`, `snapshot`, `spawns`, `worktrees`):

| Bind style | Count | Notes |
| --- | ---: | --- |
| Rest positional (`q.x.run(a, b)`) | 265 | Corpus style. Matches `SqliteStatement.run(...params: SqlValue[])`. |
| Zero-arg (`q.x.get()` / `.all()` / `.run()`) | 38 | 28 distinct keys, **all 0-placeholder** SQL. Does not rely on silent-NULL missing-all. |
| Object bind (`q.x.run({…})`) | **0** | |
| Array-literal bind (`q.x.run([…])`) | **0** | bun expands a single array as the positional list; unused. |
| Production spread | 1 | `updateSession`: `stmt.run(...keys.map((k) => upd[k] ?? null), sid)` |

`SCHEMA` in `db.ts` declares **40 INTEGER** columns (ms-epoch timestamps, ids,
0/1 flags). `db.ts` header: "All timestamps are ms epoch integers."

---

## 4. Strict-vs-non-strict delta (Bun 1.3.14)

| Binding | Non-strict (`false` / default) | `strict: true` | Corpus impact |
| --- | --- | --- | --- |
| Positional rest `INSERT (?, ?)` + `run('a', 1)` | stores values | identical | **used** — no delta |
| Positional extra / partial | throw `SQLite query expected N values, received M` | identical | **used** — already throws |
| Positional numbered `?1, ?2` | positional, same extra/partial throws | identical | unused |
| Positional missing-all `run()` | **success**, stores NULL | **success**, stores NULL | unused; **strict does not throw** |
| Omit-after-bind `get()` | reuses last bound values | identical | unused |
| Array-as-list `run(['a', 1])` / `db.run(sql, ['a', 1])` | expands as rest positional | identical | unused |
| Object to positional `?` + `{name}` | **success**, stores NULL | throw `Missing parameter "1"` | unused |
| Named `$name` + `{$name}` | **binds** | throw `Missing parameter "name"` | unused — **would break** |
| Named `$name` + `{name}` | **success**, stores NULL | **binds** | unused — **would start binding** |
| Named `:name` / `@name` | same prefix-vs-bare inversion | same inversion | unused |
| Missing named + `{other}` | **success**, stores NULL | throw `Missing parameter "name"` | unused |
| Extra named keys | ignored if the mode-required key is present | ignored if the bare key is present | unused |
| `null` / `undefined` rest | store NULL | identical | `updateSession` uses `?? null`; undefined also stores NULL |
| INTEGER / timestamps as `number` | round-trip | identical | **used** |
| `lastInsertRowid` | `number` | `number` | **used** |
| `prepare()` identity | uncached (distinct objects) | identical | daemon uses `prepare` |
| `query()` identity | cached by exact SQL | identical | unused (P8.5) |
| `finalize()` then `run` | throw `Statement has finalized` | identical | P8.4 |
| `close(true)` + live stmt | throw `database is locked` | identical | P8.4 |
| `close(false)` + live stmt still runnable | yes (`sqlite3_close_v2`) | identical | P8.4 reason to require `close(true)` after finalize |
| Ordinary table TEXT-in-INTEGER | stored as text | identical | bun-strict ≠ table STRICT |
| `CREATE TABLE … STRICT` TEXT-in-INTEGER | `SQLiteError` | identical | not bun `strict` |

**Styles that would BREAK under `strict: true`:** named SQL bound with prefixed
keys (`{$name}`, `{':name'}`, `{'@name'}`), and object binds to positional `?`.
Corpus count of both: **0 SQL, 0 calls**.

**Styles the corpus uses:** positional `?` rest-args. Delta: **none**.

---

## 5. Integers and the `safeIntegers` pin

Default `safeIntegers` is `false` in both bun-strict modes. Values the daemon
actually stores (ms-epoch timestamps ~1.7e12, ids, 0/1 flags) round-trip as
`number`, including past signed-32 (`2^31`, `2^32`) up to
`Number.MAX_SAFE_INTEGER` (`2^53 - 1`).

A `bigint` bind above the 52-bit mantissa (`2n ** 53n + 1n`, `2n ** 60n`)
returns a **truncated `number`** in both modes. `{ safeIntegers: true }` — with
or without `strict` — returns `bigint` for INTEGER columns, `id`, and
`lastInsertRowid`. That would break the sqlite.ts seam comment that `.run()`
carries `{ changes, lastInsertRowid }` as plain numbers and every consumer
doing arithmetic on timestamps/ids.

P8.2 forbids enabling `safeIntegers`. Leaving the constructor argument-less
keeps that landmine closed.

---

## 6. Statement lifetime (identical in both modes)

Relevant to P8.4 (`finalize` owned statements, then `db.close(true)`):

- `db.prepare(sql)` is uncached: two prepares are distinct objects.
- `db.query(sql)` is cached by exact SQL: two queries are the same object.
- `finalize()` then `run`/`get` throws `Statement has finalized`.
- `finalize()` twice is a no-op.
- `query()` after `finalize()` returns a **new** statement (cache miss).
- `close(true)` with a live statement throws `database is locked`.
- `finalize()` then `close(true)` succeeds.
- `close(false)` (default, `sqlite3_close_v2`) leaves a live statement
  runnable — the deferred-close hazard P8.4 exists to close.

The daemon wrapper calls `handle.close()` with no boolean (deferred) and does
not expose `finalize`. Lifetime is not a reason to enable bun `strict`.

---

## 7. Recommendation: **DO-NOT-ENABLE**

Load-bearing numbers:

1. **112** `q` statements + **9** questions prepares + dynamic `updateSession`:
   **0** named placeholders, **0** numbered `?N`.
2. **303** `q.*` calls in **15** modules: **0** object binds, **0** array-literal
   binds, **265** rest positional, **38** zero-arg (all on 0-placeholder SQL).
3. **0** corpus styles break under `strict: true`. **0** corpus styles gain
   missing-param throws: positional missing-all still stores NULL.
4. **40** INTEGER columns; timestamps `> 2^31` round-trip as `number` in both
   modes. `{ strict: true }` does **not** flip `safeIntegers`.
5. Dual-driver still present: `node:sqlite` has no `strict`. P8.1 removes that
   fallback; this slice does not.
6. Named-bind convention **inverts**. The types-doc example
   `run("… $name", { $name: "foo" })` is correct only when `strict` is off.
   Enabling now would make any later named SQL a prefix/bare footgun.

Benefit for current code: **none**. Cost: a bun-only constructor option sitting
next to `safeIntegers`, on a seam whose public type is already positional
`SqlValue[]`.

---

## 8. ENABLE-later path (not this slice)

If P8.1 has removed the Node fallback **and** a later slice introduces named
SQL, `{ strict: true }` can be flipped with a migration of bind keys:

- Keep positional `?` rest-args (or array-as-list on `Database.run`). Extra and
  partial positional already throw without `strict`.
- Named SQL must bind **bare** `{ name }` under `strict: true`, never `{$name}`.
- Do **not** set `safeIntegers`.
- P8.4: `finalize()` cached statements, then `db.close(true)`. Lifetime does
  not depend on `strict`.
- Re-run `tests/effect/sqlite-strict-trial.test.ts` as the regression net; the
  corpus tripwire fails if named placeholders appear in prepared SQL.

Until then the open stays `new Database(file)`.

---

## 9. Fixture inventory

File: `tests/effect/sqlite-strict-trial.test.ts`. Scratch DBs only.

| Fixture | What it pins |
| --- | --- |
| daemon seam still opens with no constructor options | `new Database(file)`; no `strict` / `safeIntegers`; `SqlValue[]` rest-args |
| argument-less `new Database()` matches `{ strict: false }` | prefixed named keys bind; bare keys store NULL |
| `{ strict: true }` does not enable `safeIntegers` | `lastInsertRowid` / INTEGER stay `number` |
| positional rest-args bind identically | corpus style |
| positional extra / partial throws identically | `expected N values, received M` |
| positional missing-all stores NULL in both | strict does **not** tighten corpus positional |
| omit-after-bind reuses last values | both modes |
| numbered `?1/?2` is positional | unused style, no delta |
| array-as-list expands in both | unused style, no delta |
| object bind to positional `?` | non-strict NULL; strict `Missing parameter "1"` |
| named `$name` prefix vs bare inverts | the breaking unused style |
| named `:name` and `@name` invert the same way | unused |
| missing named | non-strict NULL; strict throw |
| extra named keys ignored when the mode-required key is present | unused |
| `null` / `undefined` store NULL | both modes |
| INTEGER timestamps past `2^31` round-trip as `number` | both modes |
| bigint above `2^53` returns truncated `number` | `safeIntegers` off |
| `safeIntegers: true` contrast | independent bigint landmine; not enabled on the daemon |
| prepare / query / finalize | uncached vs cached; `Statement has finalized` |
| `close(true)` + live statement | `database is locked`; finalize then close succeeds |
| bun `strict` ≠ table STRICT | TEXT-in-INTEGER ordinary vs `SQLiteError` |
| corpus tripwire | 112 `q` keys; 0 named placeholders; `updateSession` rest spread |
