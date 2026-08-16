# Board frontend conversion — strict-typing findings

Conversion of `board/src/**` from `.jsx`/`.js` to strict TypeScript `.tsx`/`.ts`
under maximally-strict `tsc` + typescript-eslint `strictTypeChecked`. Prime
directive was ZERO behavior change; every finding below is either type-only
(NOISE), a real defect the type checker surfaced (BUG), or a place where the old
JS was type-unsound at runtime-safe but had to be pinned for the checker
(UNSOUND). Guards, coercions, and comments were preserved verbatim throughout.

Tag key: **NOISE** = type-forced, behavior-identical · **BUG** = latent defect ·
**UNSOUND** = types under-described the value; pinned with a type-only change.

### hooks/useWorktrees.ts

- **UNSOUND** — `removeWt` returned bare object literals `{ ok: true, json }` /
  `{ ok: false, reason }`. TS widens the `ok` field of a bare literal to
  `boolean`, so the union collapsed to `{ ok: boolean; ... }` and was NOT
  assignable to the modal's discriminated `{ ok: true } | { ok: false; reason }`
  prop (App.tsx `onRemove`). Fixed with `as const` on each discriminant
  (`ok: true as const` / `ok: false as const`). Type-only: the runtime object is
  byte-identical, and `WorktreesModal` defines its own `RemoveResult` union and
  only ever reads `if (res.ok)`, never `.json`, so nothing observes the change.
  A one-line comment documents why the `as const` is load-bearing.

### useModal.ts

- **NOISE (scope gap)** — this file was still fully unconverted (implicit-`any`
  hook) after the ~44 component files were done; it surfaced only once App.tsx
  went green. Now typed. The three DOM method-existence guards the hook relies on
  (`opener.focus &&`, `.focus?.()`, `fb.focus &&`) would be flagged
  "always present" on a concrete `HTMLElement`, so a local `interface Focusable
  { focus?: ... }` marks `focus` optional and keeps every guard honest —
  `activeElement` is only typed `Element` and a `querySelector` hit is only
  structurally an `Element`, so the guards are genuinely live at runtime.
- **NOISE** — `noUncheckedIndexedAccess` types `items[0]` / `items[items.length
  - 1]` as `HTMLElement | undefined`; added `first?.focus()` / `last?.focus()`.
  Both sites are inside `if (!items.length) return;`, so the optional call never
  no-ops in practice. Behavior-identical.
- **NOISE** — `querySelectorAll<HTMLElement>` (was untyped `Element`) unlocks
  `.offsetParent` in the visibility filter without a cast.
- **NOISE** — `(first || node)` → `(first ?? node)` (prefer-nullish, object LHS);
  identical since a query hit is either an element or `null`.

### App.tsx

- **NOISE** — required-array `|| EMPTY_*` drops. `snap.sessions`, `.questions`,
  `.conflicts`, `.ticker`, `.repos`, `.mail_pending`, `.mail_meta`,
  `.spawn_orphans` are contract-required (seeded snapshot defaults), so the
  `|| EMPTY_ARR` / `|| EMPTY_OBJ` fallbacks were dead per type
  (`no-unnecessary-condition`) and were dropped. The snapshot seed guarantees
  these keys are always present and truthy, so the fallback never fired at
  runtime — behavior-identical.
- **NOISE** — `browse_root` guard preserved via a local optional interface
  (`SettingsBrowse { browse_root?: { resolved?: string | null } | null }`) plus a
  cast, so the live `snap.settings?.browse_root?.resolved` optional chain isn't
  stripped by `no-unnecessary-condition`. The contract types `browse_root` as
  present, but older daemons omit it, so the guard is a real runtime defence, not
  dead code. (REFINEMENT pattern — guard kept, not dropped.)
- **NOISE** — `term.n` default preserved via `(term as { n?: number }).n ?? 0`.
  `TermState.n` is typed as an always-set `number`, which would make the original
  defensive `term.n ?? 0` "unnecessary"; the local optional shape keeps the
  default without changing the emitted value.
- **NOISE** — `onSpawnedForPlan` was `async` with no `await` (`require-await`);
  dropped `async` and returned `Promise.resolve(...)` for both branches.
  Behavior-identical (no throw in body, still returns a resolved promise).
- **NOISE** — nullable `||` → `??` normalizations (`prefer-nullish-coalescing`),
  all behavior-preserving. String/number-LHS `||` sites were correctly NOT
  flagged and kept as `||` (`c.file || c.rel_path`, `p.plan_md || ''`,
  `sess.callsign || sess.session_id`). Two patterns worth noting as verified
  identical: `(x ?? '') || fallback` for `string | null` LHS (null and `''` both
  fall through, exactly as the old `x || fallback`), and `(spawnForm.planId ?? 0)
  || null` which preserves the old `planId || null` mapping of `0 → null`.
- **NOISE** — async handlers passed to `() => void` props void-wrapped for
  `no-misused-promises` (`onClear`, `onRevive`, `onEnableRemote`, `onConfirm` on
  Kill/ArmMove/Rename, `onReset`, `loadWorktrees()` → `void loadWorktrees()`).
  Fire-and-forget semantics are unchanged — React ignored the returned promise
  before, and still does.
- **NOISE** — theme effect: `dataset.theme` → `dataset['theme']`
  (`noPropertyAccessFromIndexSignature` / TS4111) and `delete
  document.documentElement.dataset.theme` → `removeAttribute('data-theme')`
  (`no-dynamic-delete`). Both write/remove the same `data-theme` attribute —
  behavior-identical.
- **NOISE** — unused `setTerm` destructure removed (`noUnusedLocals` / TS6133).
  It was never called; `tsc` green confirms no other reference.

### components/SpawnForm.tsx

- **NOISE** — `onSpawned` prop widened to `((json: SpawnJson) => Promise<
  SpawnResult | null>) | undefined` so App's conditional assignment
  (`onSpawned={spawnForm.planId ? onSpawnedForPlan : undefined}`) type-checks
  under `exactOptionalPropertyTypes`. No runtime effect.
- **Verified, not a bug** — `plan_id` is consistently typed `number` end to end
  (body `plan_id?: number`, prop `planId: number | null`, `planId ? { plan_id:
  planId } : null`). The earlier-suspected `string → number` mistype does NOT
  exist in the converted code.

### components/TermWindow.tsx, components/TermGrid.tsx (left untouched)

- **NOTE (follow-up, not a defect)** — both still carry `(useModal as (ref, opts)
  => void)(dialogRef, {...})` casts with the comment "drop the cast when
  useModal.ts is converted." Now that `useModal.ts` IS converted, those comments
  are stale, but the casts remain lint-safe: my `useModal` param is typed
  `RefObject<HTMLElement | null>` whereas the callers hold `RefObject<
  HTMLDivElement | null>`, so `no-unnecessary-type-assertion` does not flag them.
  Left verbatim to honour "preserve every comment" and the minimal-change rule.
  A future cleanup can delete both casts and their comments with no behavior
  change.

## Verify

- `tsc --noEmit -p board/tsconfig.json` → **0** `error TS` lines.
- `eslint "board/src/**/*.{ts,tsx}"` → exit **0** (clean).
- `bun run build:board` → **success** (vite: 64 modules transformed, built in
  ~193ms; exit 0).
- Control bytes (`LC_ALL=C grep -aPc '[\x00-\x08\x0b-\x1f\x7f]'`) across all of
  `board/src/**` → **0** files affected.
- Residual `.js` / `.jsx` under `board/src` → **none**.
- **Files converted: 45** (20 `.ts` + 25 `.tsx`). `board/index.html` (entry ref
  now `/src/main.tsx`) and `board/vite.config.js` intentionally kept as-is.
