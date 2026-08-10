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
