# TypeScript migration — the progressive, file-by-file playbook

> **PARTIALLY SUPERSEDED (2026-08, single-runtime decision).** The file-by-file
> migration mechanics below still hold, but the run-path framing is stale on three
> counts: (1) there is no "modern Node" run-path — the daemon, CLI, and tests run
> under **Bun** (`bun >=1.3.14`), the Node floor was deleted (`beb18cea`); (2) the
> DB seam is `bun:sqlite`, not `node:sqlite` (retired); (3) the `test:filewise` /
> `test:bundle:filewise` scripts and the `run-tests-filewise.mjs` runner are gone —
> `bun test` (and `bun run test:bundle`) is the single lane. Read the `.mjs`/Node
> references below as the migration's mid-flight state. Full rewrite tracked under
> the §8 propagation debt.

*Companion to [foundations](./foundations.md) (F1). Part of [Fleet Deck v1.0](./README.md). Foundations says **what** F1 is (contracts-first, no rewrite) and **why**; this doc is the **how** — the mechanics of moving 34 daemon modules from `.mjs` to `.ts` one file at a time, with JS and TS running side by side the whole way, and no flag-day. Grounded in the actual tree (every count and `loc`/fan-in figure below was measured, 2026-08-07 against v0.22.4).*

> **The one-sentence version.** Both of the daemon's run-paths — the esbuild bundle *and* modern Node — already load `.ts` and `.mjs` interchangeably, so the only per-file chore is updating the explicit `./x.mjs` import specifier when you rename. Convert leaves first, giants last-and-only-when-a-pillar-touches-them, keep the 124-file suite green after every single rename, and let the two never-broken safety nets (the bundle-test lane and `tsc --noEmit`) catch drift.

---

## Ground truth — what we're actually migrating

Measured, not estimated:

- **Daemon:** 34 source modules in `scripts/fleetd/`, **18,260 lines**, all ESM `.mjs`, **explicit extensions on every relative import** (`from './db.mjs'`). Zero `.ts`, zero `tsconfig`, no `tsc` anywhere in the repo.
- **Tests:** **124 `*.test.mjs` files, 34,581 lines** in `tests/` — *the suite is larger than the daemon it covers.* Run one-process-per-file by `scripts/run-tests-filewise.mjs` (a workaround for a Node 22.x runner IPC bug; the Node 24 lane is green on the plain runner).
- **Build:** `npm run bundle` = `esbuild scripts/fleetd/fleetd.mjs --bundle --platform=node --format=esm` → the committed **`fleetd.bundle.mjs` (630 KB)**. **The npm tarball ships the *bundle*, not source** (`package.json` `files`). So production correctness rides on the bundle, and esbuild handles `.ts` by extension with **zero build-script change**.
- **Board:** `board/` is a separate Vite 8 + React 19 project, **pure `.jsx`, no TS, no tsconfig**. It hand-mirrors the `/state` contract in `board/src/useFleetState.js` — the comment-contract F1a exists to kill.
- **Topology (the pleasant surprise):** it's a **star**. `fleetd.mjs` (706 loc) imports and wires almost everything; the fan-in table below shows most modules — including the giants — are imported by exactly **one** place.

### Fan-in vs size — read this before choosing an order

| Module | loc | importers | Module | loc | importers |
|--------|----:|----------:|--------|----:|----------:|
| env-scrub | 47 | 1 | mail | 612 | 2 |
| tickets | 49 | 5 | files | 621 | 1 |
| ledger | 62 | 2 | settings | 626 | 1 |
| config | 72 | 3 | termbridge | 669 | 1 |
| plans | 107 | 1 | repos | 783 | 1 |
| run-nonce | 133 | 1 | events | 829 | 1 |
| agents-poll | 148 | 1 | derive | 957 | 1 |
| ingest | 148 | 1 | mdns | 1012 | 1 |
| transcript | 165 | 2 | spawn | 1082 | 2 |
| commands | 199 | 1 | questions | 1106 | 3 |
| paste | 199 | 1 | http | 1492 | 1 |
| repo-identity | 212 | 6 | spawns | 2369 | 2 |
| exec | 256 | 5 | **db** | **445** | **1** |
| snapshot | 300 | 1 | statements | 587 | 1 |
| helpers | 419 | **11** | retention | 549 | 1 |
| payload-capture | 502 | 5 | worktrees | 558 | 1 |

**The insight this table hands us:** the two costs are independent.

- **Rename cost ∝ fan-in** — how many `./x.mjs` specifiers you rewrite. It is *tiny* here: even `http` (1,492 loc) and `spawns` (2,369 loc) have fan-in 1–2, so renaming them touches ~one importer plus their test files.
- **Typing cost ∝ loc** — how much annotation the file itself needs. This is where the giants are expensive.

So we **do not** order by "what's easy to rename" (almost everything is). We order by **typing cost and boundary value**: small, boundary-relevant leaves first to build the recipe; the big stateful cores last, and only when a pillar is already rewriting them (the F1b rule). The high-fan-in *small* utils (`helpers` 419 loc/11, `repo-identity` 212/6, `exec` 256/5) are the only ones where the specifier codemod sweeps many files — cheap to type, moderate churn — so they slot in after the warm-ups.

---

## Why coexistence is genuinely free here

The fear with a slow migration is a broken intermediate state. It doesn't arise, because **both** ways we run the daemon load mixed `.ts`/`.mjs` transparently:

1. **esbuild bundle (production + `test:bundle`).** esbuild picks its loader by extension. A `.ts` importing an unconverted `./exec.mjs`, or an unconverted `helpers.mjs` importing a converted `./db.ts` — both bundle without a config change. The shipped artifact is always plain JS.
2. **Node from source (`npm start`, `npm test`).** Node ≥22.18 / ≥24 **strips types per-file at load** (no build, no dependency). Stripping is applied to whichever `.ts` file is loaded, regardless of the importer's extension — so a plain `helpers.mjs` importing `./db.ts` just works, and vice-versa.

**The only thing that is *not* free is the explicit extension.** Node ESM and our own convention require `./x.mjs` in the specifier, so renaming `x.mjs → x.ts` means rewriting `'./x.mjs'` → `'./x.ts'` at every import site. That's a one-line mechanical codemod per file (below), and the fan-in table shows it's small.

Everything else — the store, the mail queue, the state machine — never sees a flag-day. You can stop the migration at any file boundary and ship; a 60%-converted daemon is a normal daemon.

---

## The mechanism (recommended): rename + native strip-types + esbuild

No `ts-node`, no `tsx`, no loader — nothing to `npm install`, which keeps the doctrine (`node:sqlite`, no native deps, launched under Claude's own Node) intact.

- **tsc only checks; esbuild/Node only run.** We never emit JS from `tsc`. Set `noEmit: true` and `allowImportingTsExtensions: true` so tsc accepts the real `./x.ts` specifiers we import. esbuild produces the bundle; Node strips types for source runs.
- **Dev/CI Node floor for *source* runs: 24 (or ≥22.18).** The runtime `engines` floor stays as-is (`^22.13 || >=24`) because **production ships the bundle**, which is plain JS and runs on the whole range. Only running *from source* needs a stripping-capable Node — and the Node 24 lane already exists and is green.
- **Avoid the two TS features native stripping won't transform:** `enum`, `namespace`, and constructor parameter-properties need `--experimental-transform-types` (esbuild handles them, but Node-from-source wouldn't). Use `const` objects + union types and plain field assignment instead. One rule, zero surprises.
- **`verbatimModuleSyntax: true` + `import type`** for type-only imports, so stripping never leaves a dangling runtime import.

*Fallback if we ever don't want to bump the dev Node:* run source tests through esbuild (`--import` a tiny esbuild-transform hook) instead of native stripping. Not recommended — it adds a dev dependency for something Node now does for free — but it's the escape hatch if the strip-types corners bite.

### tsconfig strategy

**Root `tsconfig.json`** (daemon + contracts):

```jsonc
{
  "compilerOptions": {
    "target": "ES2023",
    "module": "nodenext",
    "moduleResolution": "nodenext",
    "allowJs": true,          // tsc resolves .mjs siblings…
    "checkJs": false,         // …but doesn't error on them yet (the coexistence knob)
    "strict": true,           // full strictness for .ts from day one
    "noEmit": true,           // esbuild/Node run; tsc only checks
    "allowImportingTsExtensions": true,
    "verbatimModuleSyntax": true,
    "skipLibCheck": true,
    "types": ["node"]
  },
  "include": ["contracts", "scripts/fleetd", "bin", "tests"]
}
```

The `allowJs:true / checkJs:false` pair is the whole coexistence story: tsc *sees* the unconverted `.mjs` for module resolution but only *type-checks* `.ts`. When appetite grows, flip individual `.mjs` to checked with a top-of-file `// @ts-check` (catches real bugs in JS with zero rename), and eventually flip `checkJs:true` globally.

**Board `board/tsconfig.json`:** `jsx: "react-jsx"`, DOM libs, `allowJs:true`, `checkJs:false`, `noEmit:true`, `strict:true`; add `typescript` + `@types/react` + `@types/react-dom` as board devDeps. Vite already transpiles `.tsx` via its own esbuild — no Vite config change.

### CI lanes — and the safety net that resolves the Node-floor tension

- **New required gate:** `tsc --noEmit` (root) and `tsc --noEmit -p board`. Green from Phase 0 (nothing to check yet), and it's the gate that makes types non-decorative — the review's hard requirement (esbuild strips without checking).
- **Source suite** (`npm run test:filewise`) on **Node 24** — strips types, exercises the real `.ts` source.
- **Bundle suite** (`npm run test:bundle:filewise`) on the **full `engines` matrix incl. 22.x** — the bundle is plain JS, so it proves the *shipped artifact* on every supported Node regardless of source-TS support. **This is the linchpin:** production correctness is proven by the bundle across the whole range; TS ergonomics only ever need the newer dev Node.

---

## The per-file recipe (the heart of "file by file")

Every conversion is the same six steps, one file, one commit, fully reversible:

1. **Pick a file** by the order below (leaf-first, or pillar-driven once F1b is in force).
2. **Rename:** `git mv scripts/fleetd/x.mjs scripts/fleetd/x.ts`.
3. **Codemod the specifiers** across source *and* tests (explicit-extension chore):
   ```bash
   grep -rl "'\./x\.mjs'" scripts/fleetd tests \
     | xargs sed -i "s#'\./x\.mjs'#'./x.ts'#g"
   # plus any deeper relative path to it, e.g. '../scripts/fleetd/x.mjs' in tests/
   ```
4. **Add types, don't rewrite logic.** Annotate exported function signatures and the module's public shapes; import shared types from `contracts`. Start permissive (`unknown`/`any` at the messy edges) and tighten in follow-ups. The goal of the first pass is *green*, not *perfect*.
5. **Prove it, three ways:** `npx tsc --noEmit` clean → the file's own test green (`node tests/…/x.test.mjs`) → `npm run bundle && npm run test:bundle:filewise` green (the shipped artifact still works).
6. **Regenerate + commit the bundle with the source.** `fleetd.bundle.mjs` is committed and shipped — a conversion that forgets to re-bundle silently ships stale JS. One commit = the renamed `.ts` + rewritten specifiers + regenerated bundle. (Better: have CI rebuild the bundle so it can never drift; until then, do it by hand every time.)

Revert is symmetric: `git mv` back + `sed` back. No file conversion can wedge the tree.

---

## The order (data-driven, progressive)

### Phase 0 — toolchain, **zero conversions** (~½ day)
Add `typescript` + `@types/node` devDeps; add the two tsconfigs above; add the `tsc --noEmit` CI lanes (green immediately); document the dev Node requirement (24 / ≥22.18) and the bundle-test safety net. **Nothing is renamed; JS runs exactly as today.** This phase is pure insurance and can land now, independent of every pillar.

### Phase 1 — F1a contracts module (1–2 weeks; the seam, purely additive)
Create `contracts/*.ts`: the `/state` snapshot, the **canonical hook events** (this *is* the [architecture](./architecture.md#layer-2--canonical-event-vocabulary) vocabulary — F1a and the P1 refactor are one task), the `/api/spawn` body, the mail/command/questions wire shapes, and a **`schema_version`** on every shape. Ship **hand-written runtime validators** beside the types for the two hostile boundaries only (hook body, spawn body) — types guard our code, validators guard the wire. Daemon and board both import it. **No existing file is converted yet** — this is the first `.ts`, and it earns its keep before any rename.

*Location:* a top-level `contracts/` dir imported by both `scripts/fleetd/**` and `board/src/**`. It needs no packaging change — esbuild inlines it into the bundle and Vite inlines it into `board-dist`, so nothing new is published. (If the shared surface later sprawls, promote it to an npm-workspace `packages/contracts` post-1.0; not worth the workspace tooling for 1.0.)

### Phase 2 — warm-up leaves (build the muscle, ~1 file/day)
Small, boundary-relevant, low typing cost: **`env-scrub` (47)**, `tickets` (49), `config` (72), `ledger` (62), `run-nonce` (133), `ingest` (148), `transcript` (165), `agents-poll` (148). `env-scrub` first — 47 lines, a security boundary, the perfect proof-of-recipe.

### Phase 3 — the data-layer seam: `db.ts` + `statements.ts`
Both fan-in 1 (trivial rename), and typing them puts real row/param types on the whole storage surface that every later module inherits. High value, low churn — do it right after the warm-ups. This is also where the P2 base-ref column and the numbered/transactional migrations (see [validation-and-gates](./validation-and-gates.md)) get a typed home.

### Phase 4+ — F1b takes over: pillar-driven, giants last
No standalone "convert the big files" phase. Each core converts **when a pillar is already rewriting it**, so typing cost rides on work that's happening anyway:
- **P1** → `events` (829), `derive` (957), `ingest` — plus the new `providers/` strategy objects (born in TS).
- **P2** → `worktrees` (558).
- **P5** → `mail`/`questions` as completions land; **P4** → `settings`/`transcript` as usage/config-home land.
- `http` (1,492), `spawns` (2,369), `spawn` (1,082) convert opportunistically as their routes are touched — never in one sitting.
- **`mdns` (1,012) last** — it's also the F2 Bun risk; leave it JS until F2 forces the issue.
The high-fan-in utils (`helpers` 419/11, `repo-identity` 212/6, `exec` 256/5, `payload-capture` 502/5) fold in whenever convenient after Phase 3 — cheap to type, and their codemod sweep is the only moderately wide one.

**Explicitly fine at the 1.0 cut:** the giants may still be `.mjs`. F1's definition of done is "new code is TS, touched code converts, `tsc` is green, tests never went red" — **not** "100% converted." A daemon that is 40% TS with typed contracts and a typed data layer has already banked the review's motivation (kill the comment-contract, stop runtime-bugs-from-missing-types); the rest is interest paid down over time.

---

## Restructuring — fold the move *into* the conversion

The 34 flat modules can gain a light directory structure as they grow — but **moving a file churns its specifier exactly like the extension change does**, so never do a big "reorganize everything" commit (it collides with every in-flight pillar and every other session's edits). Instead, when you convert `x.mjs → x.ts`, drop it into its home dir in the *same* commit — you pay the specifier codemod once. The star topology makes this safe: for most modules the only importer to update is `fleetd.mjs`.

A target layout that mirrors architecture.md's provider-free / provider split:

```
scripts/fleetd/
  contracts/     → shapes + validators + schema_version           (Phase 1)
  db/            → db, statements, migrations                      (Phase 3)
  hooks/         → events, derive, ingest, payload-capture         (P1: intake normalization lives here)
  providers/     → claude + codex strategy objects                (P1: new TS, architecture Layer 4)
  orchestration/ → mail, questions, completions, plans, ledger     (P5)
  spawn/         → spawn, spawns, worktrees, takeover, tickets, repos, repo-identity
  http/          → http, termbridge, snapshot, files, commands, paste
  usage/         → (P4, new)
  net/           → mdns                                            (last; F2 risk)
  util/          → helpers, exec, env-scrub, config, run-nonce, transcript, settings, agents-poll
```

This is a *destination*, not a phase. Some dirs (`providers/`, `usage/`, `orchestration/`'s completions) are born in TS with the pillars; the rest arrive one converted file at a time. If a move feels risky mid-pillar, skip it — a flat `.ts` file is still a win; the folder can come later.

---

## Guardrails

- **Never quarantine a test to make a conversion pass** ([[test-suite-is-trust]]). The 124-file suite is the migration's whole safety margin; green after every rename is the contract.
- **Keep the committed bundle in sync.** Every conversion re-runs `npm run bundle`; ideally CI rebuilds and diffs it so a stale bundle can't ship.
- **Sequence conversions *with* pillars, not against them.** Converting `spawns.mjs` (2,369 loc) while another session is mid-pillar in it is a merge fight — let the pillar owner convert it as part of their work (F1b).
- **Coordinate on shared-file edits.** Fleetdeck warns when another session touches a file you're editing; a rename is a whole-file touch — heed the warning before renaming a hot module.
- **No `enum`/`namespace`/param-properties** (native strip-types won't transform them); `import type` for type-only imports.

## Definition of done (F1's, restated as exit criteria)

- Phase 0 landed: two tsconfigs, `tsc --noEmit` a required green lane, dev Node requirement + bundle-test safety net documented.
- Phase 1 landed: `contracts/` consumed by daemon **and** board; the `useFleetState.js` comment-contract replaced by imported types; runtime validators on hook + spawn bodies; `schema_version` on every wire shape.
- F1b in force: new v1.0 code is TS; every pillar converts the modules it touches; `tsc` green and the 124-file suite never went red through the transition.
- The bundle-test lane is green on the full `engines` matrix at all times (the shipped artifact never regressed, whatever the source is written in).
