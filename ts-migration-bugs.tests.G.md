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
