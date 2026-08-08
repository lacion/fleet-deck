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
