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
