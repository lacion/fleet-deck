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
