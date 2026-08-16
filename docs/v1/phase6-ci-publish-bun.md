# Phase 6 — CI / publish on bun (npm OIDC publish preserved)

> **SUPERSEDED (2026-08, single-runtime decision).** This doc describes the
> earlier DUAL-runtime CI (an authoritative node:sqlite matrix lane + an advisory
> bun lane). Fleet Deck is now Bun-primary single-runtime: `ci.yml` has ONE
> authoritative bun `test` job (no node matrix), and `publish.yml`'s release gate
> runs the suite under bun:sqlite after a standalone adapter-contract step. The
> `run-tests-filewise.mjs` runner and the `test:filewise`/`test:bundle:filewise`
> scripts are deleted. The toolchain gate is now `bun run ci` = `biome ci` (format
> check + lint) — ESLint/Prettier/typescript-eslint were replaced by Biome 2.5.8
> (`e1b25f5d` + `826a2f7d`). `.github/workflows/{ci,publish}.yml` are the current
> truth; the full rewrite of this doc is tracked under the §8 propagation debt.

> Status: **workflows edited, locally proven green, nothing committed.** Both
> `.github/workflows/ci.yml` and `.github/workflows/publish.yml` now install with
> bun and gate BOTH runtime channels; the `npm publish` trusted-publishing step is
> untouched. Every command placed in CI was run locally under bun 1.3.14 /
> node v22.22.2 first — this records what passed.

## What changed

`oven-sh/setup-bun` is pinned by SHA to the exact bun the migration was validated
against, matching the repo's SHA-pinning discipline (checkout, setup-node):

```
oven-sh/setup-bun@0c5077e51419868618aeaa5fe8019c62421857d6 # v2.2.0  (bun-version: 1.3.14)
```

(The v2.2.0 tag is lightweight → the SHA is the release commit itself, `2026-03-14`.)

### ci.yml — two channels, one authoritative

| Job | Runtime | Gate | Blocking? |
|---|---|---|---|
| **toolchain** (NEW) | bun | `bun run typecheck` + `bun run lint` (root **and** board installed — `tsc -p board` and type-aware eslint over `board/src` both need board's types) | yes |
| **test** (matrix 22.13.0 · 24) | Node | `bun run test:filewise` (22) / `bun run test` (24) — **node:sqlite channel, authoritative** | yes |
| **bun-runtime** (NEW) | bun | ① `bun -e` **bun:sqlite adapter contract** (open/exec/prepare/run/get/all + missed-`.get()`→`undefined`); ② full suite `bun test` per file, **advisory** | ① yes ② no |
| **bundle** | Node+bun | `bun run bundle` → staleness `git diff`; `bun run test:bundle:filewise` | yes |
| **board** | bun | `cd board && bun run build` → board-dist staleness | yes |
| **test-macos** | Node+bun | `bun run test:filewise` (advisory, `continue-on-error`) | no |
| **version**, **hook-integrity** | ambient Node | script-only (`node -p` / `execFileSync git`) — no install, untouched | yes |

- **npm as a package manager is gone from CI.** Every `npm ci --ignore-scripts`
  → `bun install --frozen-lockfile --ignore-scripts` (bun's `--ignore-scripts`
  preserves the "install executes no code" posture; bun also does not run
  dependency lifecycle scripts by default). `cache: npm` and
  `cache-dependency-path: board/package-lock.json` are removed — with the npm
  lockfiles deleted, setup-node's npm cache would hard-error ("lock file not
  found").
- **The Node lane stays the gate.** `bun run test*` execs `node --test …`; the
  filewise runner spawns `process.execPath` (the matrix Node), so the pinned
  22.13.0 floor and Node 24 are exercised end to end. bun only installs and runs
  the bun-specific gates.
- **The bun lane is honest, not curated.** The advisory `bun test` step iterates
  **every** one of the 124 test files (`find tests -name '*.test.mjs'` — the same
  set the Node runner discovers, verified 124==124), time-boxes each at 120 s so
  one wedged file can't eat the budget, and prints `pass/total` + the list of
  files bun's node:test compat does not yet run. Nothing is skipped or quarantined
  to force a pass ([[test-suite-is-trust]]); the Node lane is authoritative and
  the bun count is recorded as a compat signal.

### publish.yml — install swapped, publish preserved

- **NOT renamed** — OIDC trusted publishing is keyed to the `publish.yml`
  filename; renaming fails a release with ENEEDAUTH.
- `npm ci --ignore-scripts` → `bun install --frozen-lockfile --ignore-scripts`;
  `npm test` → `bun run test`; artifact-staleness rebuild → `bun run bundle` +
  `(cd board && bun install --frozen-lockfile --ignore-scripts && bun run build)`.
- **KEPT VERBATIM:** `npm install -g npm@latest` (trusted-publishing floor) and
  `npm publish --access public` (bun cannot do npm trusted publishing) and
  `id-token: write`. A comment at the setup-bun step states why npm is retained,
  so a future editor doesn't "helpfully" swap it to `bun publish`.

## Locally proven before committing to CI (bun 1.3.14 / node v22.22.2)

| Command placed in CI | Local result |
|---|---|
| `bun install --frozen-lockfile --ignore-scripts` (root) | `Checked 103 installs … no changes`, exit 0 |
| `bun install --frozen-lockfile --ignore-scripts` (board) | `Checked 28 installs … no changes`, exit 0 |
| `bun run typecheck` (`tsc -p . && tsc -p board`) | exit 0, clean |
| `bun run lint` (eslint strict, type-aware) | exit 0, no findings |
| `bun run bundle` | **idempotent run-to-run** (identical sha256 twice) → staleness gate passes once committed |
| `cd board && bun run build` | board-dist **byte-stable** (git diff clean, no untracked) |
| `bun -e` bun:sqlite adapter contract | `BUN_SQLITE_SMOKE: PASS` (got `heron`, miss→`undefined`, 1 row, numeric rowid) |

## Notes / decisions

- CI does not run until this branch opens a PR; **nothing here is committed.** The
  edits take effect together with the rest of the migration (bun.lock committed,
  npm lockfiles deleted, bun package.json).
- `version` + `hook-integrity` keep using the runner's ambient Node — they call
  `node` directly (not a package manager) and need no `node_modules`, so no
  setup step was added (avoids scope creep in unrelated jobs).
- The bundle staleness gate could only be proven as **run-to-run idempotency**
  here (no commit yet to diff against); once the reproducible bundle is committed,
  `bun run bundle && git diff --exit-code` is green by construction.
