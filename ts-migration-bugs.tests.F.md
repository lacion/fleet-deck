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
