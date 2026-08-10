<!-- STAGING for the Node engine-floor raise (22.13 -> 22.18) that the TS migration
     forced. Merge into ts-migration-bugs.md (newest-first) once the concurrent board
     subagent has finished its own append; kept separate only to avoid a concurrent-
     append race on the shared 140k log. This is a PREREQUISITE for the test-conversion
     phase (task #11): the suite cannot run on the old floor at all (see below). -->

### Engine floor raised 22.13.0 -> 22.18.0 — the TS migration's one published-contract change   [MIGRATION CONSEQUENCE, not a code bug — but it is the thing that unblocks the whole test phase]

- **What forced it.** Running the project's TypeScript sources directly requires Node's
  native, unflagged type-stripping, which first shipped in **22.18.0**. The daemon `start`
  script already points at `fleetd.ts`, and — the binding discovery — the EXISTING
  `tests/cli.test.mjs` already does `await import('../bin/fleetdeck.ts')` and
  `'../bin/tmux-version.ts'`. So the test suite in its CURRENT `.test.mjs` form ALREADY
  needs >=22.18: on the old CI floor lane (pinned `22.13.0`) `cli.test.mjs` would throw on
  the `.ts` import before asserting anything. The floor raise is not merely future-proofing
  for the `.test.ts` rename — it repairs the branch's present state.

- **Why not keep the floor at 22.13 for consumers?** Considered and rejected by the
  maintainer (informed choice). The shipped artifact is the plain-JS esbuild BUNDLE, which
  genuinely runs on 22.13 (node:sqlite loads unflagged from 22.13.0), so a "declared 22.18,
  tolerated 22.13" split was technically possible — but the codebase deliberately locks
  `doctor`/guard == `package.json engines` (a `cli.test.mjs` invariant: "doctor text and
  engines must not drift apart"). A split would fight that invariant for a range (22.13–22.17)
  that, by Aug 2026, essentially no one runs — especially Claude Code users. Raising the
  single floor is simpler and keeps CI structurally unchanged.

- **CONSEQUENCE to flag: the runtime guard now hard-blocks 22.13–22.17.**
  `bin/fleetdeck.ts` `nodeVersionSupported` gates whether the CLI boots (`major===22 ->
  minor>=18`). Because the shipped bundle is plain JS, those versions would *technically*
  still run fleetd — but they are now out of the supported/declared/enforced range, and the
  guard refuses them with a clear message rather than silently supporting an untested range.
  This is the honest reading of "drop declared support for 22.13–22.17." If a future need to
  tolerate-but-not-support that range arises, decouple the guard from `engines` (and relax
  the `cli.test.mjs` drift assertion) — but that is a deliberate reversal, not the default.

- **Files changed (all mine; no comet surface touched):**
  - `package.json` — `engines.node` `^22.13.0 || >=24.0.0` -> `^22.18.0 || >=24.0.0`.
  - `bin/fleetdeck.ts` — `MIN_NODE_RANGE` + the `minor>=13`->`minor>=18` check + the doctor
    message (dropped the misleading "for node:sqlite" clause — node:sqlite is NOT the binding
    constraint at 22.18) + the rationale comments + the source-require floor note (source-run
    floor and supported floor are now equal, so that path never needs a newer Node than the
    CLI already requires).
  - `bin/fleetdeck.mjs` — rebuilt from the edited `.ts` via esbuild (`bundle:bin` args,
    byte-for-byte the shipped artifact); floor now reads `>= 18`.
  - `tests/cli.test.mjs` — the floor test rewritten: `22.13.0`/`22.17.1` now expected FALSE,
    `22.18.0` the first TRUE; the `MIN_NODE_RANGE` equality assertion bumped. 43/43 green
    locally on v22.22.2.
  - `.github/workflows/ci.yml` — matrix `['22.13.0','24']` -> `['22.18.0','24']`; the pin
    rationale rewritten (22.18 = first unflagged type-strip); the "floor CANNOT strip .ts"
    comment block rewritten (22.18 CAN strip — the floor lane still runs the BUNDLE because
    that is what SHIPS, not because the floor can't strip source).
  - `scripts/fleetd/http.ts` — a one-line comment (`engine floor (22.13)` -> `(the engine
    floor)`), accuracy only.
  - `README.md` — badge `>=22.13`->`>=22.18`, requirements line, `doctor` comment.
  - `CONTRIBUTING.md` — the floor bullet + the "README promises Node 22.13+" line.

- **DEFERRED (coordination):** `docs/CODER.md` still says `^22.13.0 || >=24.0.0` (line ~191).
  It is a SHIPPED doc (in `package.json files`) and currently clean, but it lives under
  `docs/**`, the boundary the concurrent comet-55e1 session is actively editing. Left for the
  final docs pass (task #9) / once comet's docs work settles, to avoid a clobber. Same for
  `docs/v1/ts-migration.md` and `docs/v1/phase6-ci-publish-bun.md` (comet's untracked v1 docs
  that mention 22.13 in migration-planning context).

- **Verification:** `tsc -p tsconfig.json` clean for `bin/**`; `eslint` clean on
  `bin/fleetdeck.ts`; `bin/fleetdeck.mjs` rebuilt and floor-checked; `cli.test.mjs` 43/43 on
  node v22.22.2. This lands as its own green checkpoint BEFORE any `.test.mjs -> .test.ts`
  rename.
