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

<!-- entries appended below as modules convert -->

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
