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
