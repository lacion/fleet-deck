# Effect migration checkpoint status

- **Checkpoint date:** 2026-08-24
- **Branch:** `fd/v1-effect-feasibility`
- **Published branch:** `origin/fd/v1-effect-feasibility` currently at `f2592534`
  (`test(effect): pin the mixed-load shutdown matrix (P10 slice 4)`). Implementation
  HEAD is **1 commit ahead** — the slice-5 orphan-sweep DEFER (`16656dae`, test +
  docs only) is local-only and unpushed. Slices 0–4 are pushed.
- **Current implementation HEAD:** `16656dae`
  (`test(effect): defer the orphan-sweep conversion with its floor (P10 slice 5)`;
  the P10 stack above the P9 docs-close `8d09b332` is `650fb09b`..`16656dae`)
- **P10 completion evidence:** [p10-close.md](./evidence/effect/p10-close.md)
  (package roll-up, the slice-2 DO-NOT-SHIP→SHIP arc, the held-primitive record,
  the exit-gate audit, the bundle trajectory) + the stamped design
  [p10-design.md](./evidence/effect/p10-design.md) §1–§6 (incl. the §6
  adjudications) + [p10-slice5-defer.md](./evidence/effect/p10-slice5-defer.md)
- **P9 completion evidence:** [p9-close.md](./evidence/effect/p9-close.md)
  (package roll-up, exit-gate audit, bridge inventory) + per-sub-package designs
  [p9-1-design.md](./evidence/effect/p9-1-design.md) §9 …
  [p9-6-design.md](./evidence/effect/p9-6-design.md)
- **P8 completion evidence:** [p8-strict-trial.md](./evidence/effect/p8-strict-trial.md),
  [p8-stmt-cache-trial.md](./evidence/effect/p8-stmt-cache-trial.md),
  [p8-sql-client-trial.md](./evidence/effect/p8-sql-client-trial.md),
  [p9-completion-map.md](./evidence/effect/p9-completion-map.md)
- **P6 completion evidence:** [p6-http-matrix.md](./evidence/effect/p6-http-matrix.md),
  [p6-route-wave.md](./evidence/effect/p6-route-wave.md),
  [p6-graceful-stop-verification.md](./evidence/effect/p6-graceful-stop-verification.md),
  [p6-bench-comparison.md](./evidence/effect/p6-bench-comparison.md)
- **P6.3 HttpServer owner:** `e7900bac`
  (`feat(effect): own the Bun listener as the HttpServer root service`)
- **P5 completion evidence:** [p5.md](./evidence/effect/p5.md)
- **P4 root-cutover checkpoint:** `661dfe31b66843f70a1dcebbc4f340ad9c62f76f`
- **P3 rollback anchor:** `bcf3337e48d7dd35437d2e2369d0a91fbcbfa114`
- **Runtime floor:** Bun 1.3.14, revision `0d9b296af33f2b851fcbf4df3e9ec89751734ba4`

This is the durable handoff for the executable
[Effect migration plan](./effect-migration-plan.md). **P0–P10 are complete.** P10
("questions, holds, and fail-open cleanup") closed at `16656dae`: a six-slice
ladder converted GET `/mail` (slice 1) and the last two held request paths — GET
`/api/watch` (slice 2) and the hook HOLD relay (slice 3) — onto one held-response
`Deferred` settlement primitive; slice 4 pinned the mixed-load shutdown matrix; and
the slice-5 orphan-sweep conversion is an adjudicated DEFER (risk > value, with an
explicit re-evaluation trigger). The fail-open contract is strengthened, not merely
preserved. Implementation HEAD `16656dae` is 1 commit ahead of origin `f2592534` —
the slice-5 test+docs commit is local-only and unpushed. This P10-close
documentation is uncommitted. No pull request has been opened, and nothing has been
tagged, released, or deployed. Local resume is **P11** (remaining Bun trials). P7
remains the standing §7 platform authorization checkpoint — it is BLOCKED on Luis's
draft-PR authorization and is not the local next slice.

## Executive status

| Package | State | Resume point |
| --- | --- | --- |
| P0 | Complete | Baselines, workloads, probes, comparisons, and CI policy are recorded. |
| P1 | Complete | Explicit resource owners and ordered shutdown remain the rollback seam. |
| P2 | Complete | The exact Effect RC cohort, kernel, boundaries, and Bun conformance are recorded. |
| P3 | Implementation complete | Bun process routing is live. Quiet-host performance evidence remains an explicit ledger item. |
| P4 | Implemented and checkpointed | The root cutover and shutdown evidence are complete for the pre-P5 artifact. Do not relabel that evidence as measuring the current tree. |
| P5 | Complete | Prompt failure publication, the reviewed detached-owner exception, whole-slice rollback, gzip budget, and quiet global suites are recorded at `ca62b94f`. |
| P6 | Complete at `b2d11d84` | Exit gate met with the five dispositions in [effect-migration-plan.md](./effect-migration-plan.md) P6 (cancellation-on-disconnect is deliberately none-yet). |
| P7 | Blocked (not started) | Standing §7 platform authorization checkpoint — BLOCKED on Luis's draft-PR authorization (push + draft PR so the macOS/real-tmux + Linux lifecycle jobs can run). Termbridge facade is the seam; `/ws/term` stays P7. Not the local next slice; not a reason to stall P9. |
| P8 | Complete at `cd0470bb` | Exit gate MET with the four dispositions in [effect-migration-plan.md](./effect-migration-plan.md) P8. Root-owned Store + five db-workflows slices; HTTP-bridged keep capability params. |
| P9 | Complete at `b3666ca8` | Exit gate MET ([p9-close.md](./evidence/effect/p9-close.md) §3). P9.1 spawn/revive/dismiss `03ee63f2`; P9.2 repos/worktrees `f23cfc9b`/`991c82be`/`8409be88`/`0074a154`; P9.3 files `1b07aaa0`/`0a18335c`; P9.4 mail pane-delivery `cd65f374`; P9.5 settings/ack/postMail `ae6c36fd`/`b3666ca8`; P9.6 takeover = adjudicated justified NON-conversion. |
| P10 | **Complete at `16656dae`** | Exit gate MET ([p10-close.md](./evidence/effect/p10-close.md)). Six-slice ladder: floor `650fb09b`; GET `/mail` `8c896a80`; GET `/api/watch` held `2ab8ec95`; hook HOLD relay `285122a1`; shutdown matrix pin `f2592534`; orphan-sweep DEFER `16656dae`. Held-response `Deferred` primitive; fail-open strengthened. Slices 0–4 pushed; slice 5 local-only. |
| P11–P14 | **Not started — resume at P11** | Remaining Bun trials (P11, resume here), build/distribution (P12), cleanup/docs (P13), RC rehearsal + version-manifest closure checkpoint (P14). |

P3's paired quiet-host performance evidence is unchanged and out of P8/P9/P10
scope. Do not close it from these suites.

## P10 complete at `16656dae`

P10 is **COMPLETE**. The last held/lease request paths now settle through one
Effect held-response primitive; the questions orphan sweep is an adjudicated DEFER;
and the fail-open contract is strengthened, not merely preserved. The full package
record — the per-slice ledger, the slice-2 DO-NOT-SHIP→SHIP arc, the held-primitive
record, the exit-gate audit, and the bundle trajectory — is in
[p10-close.md](./evidence/effect/p10-close.md). The stamped design (§1–§6, including
the §6 adjudications that bind Q1 settlement-only and the slice-5 defer) is
[p10-design.md](./evidence/effect/p10-design.md); the slice-5 gap analysis is
[p10-slice5-defer.md](./evidence/effect/p10-slice5-defer.md).

Push state: slices 0–4 are pushed (origin at `f2592534`); the slice-5 orphan-sweep
DEFER (`16656dae`, test + docs only) is local-only, 1 ahead. This P10-close
documentation is uncommitted.

### Six-slice ladder (commit · disposition)

| Slice | Commit | Scope | Disposition |
| --- | --- | --- | --- |
| 0 — characterization floor | `650fb09b` | lockstep source-pin + watch 25 s ceiling + socket-disconnect / cap-evict / fail-open-all pins | test-only floor |
| 1 — GET `/mail` drain | `8c896a80` | mail-drain transport → `mailDrainWorkflow` (`MAIL_DRAIN_DEFECT`, 500 `{}`) | converted (P9-class, `effectRoutes` seam) |
| 2 — GET `/api/watch` held | `2ab8ec95` | held long-poll → the parameterized held `Deferred`; idle fold; `runControlDetached` | converted; **DO-NOT-SHIP → unblocked → SHIP** |
| — test fixup (interleaved) | `70b37f90` | tolerate a torn trailing line when reading fixture spawn JSONL | test-robustness, no seam |
| 3 — hook HOLD relay | `285122a1` | final held conversion; fail-open `{}` fold; `EFFECT_CORE_HOLD_RELAY` + `*Legacy` twins | converted (double-gated) |
| 4 — shutdown matrix | `f2592534` | mixed hook+watch in ONE shutdown → releasing-holds ordering pin | test-only (collapse to pins) |
| 5 — orphan sweep | `16656dae` | gap analysis → **DEFER** (risk > value; §6-Q2) + characterization floor | test + docs only; local-only |

### Held-response primitive

Slices 2 and 3 share one settlement primitive: `heldSettleWorkflow`, a
`Deferred<HeldOutcome, never>` under `Effect.scoped` + `acquireRelease`,
first-settlement-wins via idempotent `Deferred.doneUnsafe`, parameterized ONLY on
the terminal fold and discharged through the pre-existing `runControlDetached`
(`Context.empty()`, unsupervised, survives shutdown for D2). Two transport folds
sit on it: `settleEffectWatchHold` (idle fold, `http.ts`) and `settleEffectHookHold`
(fail-open `{}` fold). The P1 hold-manager Maps stay imperative behind the policy
adapter — the Q1 settlement-only doctrine (§6 ACCEPTED-BINDING): P10 wraps
settlement, it does not deepen ownership.

### Fail-open STRENGTHENED

The fail-open contract is not merely preserved — it is consolidated. Every
`/hook/*` answer is HTTP 200 `{}` unless a handler produced output;
`hookFailOpenBody()` (`hook-policy.ts:47`) is now the single source shared by
`mapHookExit`'s failure arm AND `settleEffectHookHold`'s `.catch`. Held answers are
NOT routed through `mapHookExit` (that would double-wrap to `{body:{body:obj}}`).
The sibling settlers REJECT-on-die into the fail-open fold rather than leaking a
hold.

### Slice 4 collapse-to-pins, slice 5 DEFER

- **Slice 4** collapsed to pins (the P9.6 non-conversion shape): shutdown ORDERING
  is already Effect-owned (P4-frozen `ShutdownPhaseOrder`), settlement is already
  Effect (slices 2+3), the coordinator is imperative by §6-Q1, and the exit gate is
  already pinned at the phase layer. The one gap — a mixed hook+watch load in a
  SINGLE shutdown — is closed by `tests/p10-slice4-shutdown-matrix.test.ts`
  (releasing-holds settles hook 200 `{}` + watch idle body before closing-clients;
  `runHeldCalls` 1→2; `ownedCounts`→0). Mutation-proven; no bundle change.
- **Slice 5** is an adjudicated DEFER (§6-Q2, risk > value): the orphan sweep is a
  domain-owned `setInterval` inside `createQuestions` (`questions.ts:1432`), with
  zero Effect imports and no P5-family Store seam; converting would break the
  Q1-BINDING doctrine and perturb D2 order for uniformity only. Re-evaluation
  trigger: **T1** (a post-P10 package lifts the hold Maps into Effect-owned state,
  dissolving the P10-scoped Q1 block) or **T2** (the sweep stops being redundant
  with the per-hold `holdMs` timers). A 3-test characterization floor freezes the
  §2D contract first.

### Quiet suites at `f2592534` (slice 4, = origin HEAD)

Bun 1.3.14, quiet WSL2 host, 2026-08-24, as reported at the slice-4 integrate:
`bun run test` **1,812 pass / 6 skip / 0 fail**; `bun run test:bundle` **1,803 pass
/ 15 skip / 0 fail**. Growth over the P9 close (`b3666ca8` 1,785 / 1,776): +27 /
+27. Slice 5 (`16656dae`, local) adds 3 characterization tests (3 pass ×3, 2
mutation-proven) not in that roll-up. New/updated files: `p10-slice2-watch-legs`
(4), `p10-slice3-hook-hold-legs` (9), `p10-slice4-shutdown-matrix` (1),
`p10-slice5-orphan-sweep-characterization` (3), plus the 64 s real-timer lockstep
oracle (`question-rearm` 9 + `watch-rewake`). `bun run ci` (biome, 447 files) clean;
typecheck both legs 0 at each integrate.

### Accepted daemon identity at HEAD `16656dae`

Slices 4 and 5 are test/docs-only, so the bundle is byte-identical to slice 3
(`285122a1`) — **re-verified live against the committed artifact this session**:

- Raw: 652,147 B (**115,853 B** under the 768,000 B ceiling).
- gzip-9 zlib: 172,335 B (**17,105 B** under the 189,440 B ceiling).
- SHA-256: `84daf5972d10316797bbf7a8f6b5996d8801c3612994de49eaf58e56ec20e327`.
- Deterministic. Versus P9 close (`b3666ca8`, 648,714 / 171,096): raw +3,433 B,
  gzip-9 +1,239 B.
- CONVENTION NOTE: the per-slice worker reports cite a `gzip -c` (level 6) figure
  (172,632→173,231) that is **not** the policy metric. The gate value is the
  gzip-9-zlib 172,335 above (`Bun.gzipSync(bytes,{level:9,library:'zlib'})`, the
  value the `daemon-bundle-policy` gate enforces).

### Exit-gate dispositions at `16656dae`

- **hook / needs-you / board-hold failure + race matrix green** — the slices 2+3
  legs (`p10-slice2-watch-legs` 4, `p10-slice3-hook-hold-legs` 9), the 64 s
  real-timer lockstep oracle (`question-rearm` 9 + `watch-rewake`), and the
  mixed-load shutdown pin (slice 4).
- **canonical 200 `{}` release-all** — `settleEffectHookHold` + `hookFailOpenBody`
  shared source; D2 releasing-holds settles 200 `{}` before closing-clients and
  native stop.
- **no new unsupervised runner** — the ingress `run*With` pin held at 2; held
  bodies discharge on the pre-existing `runControlDetached`; a started hold body
  JOINs before the listener releases (`runHeldCalls == ownedCounts → 0` at close).
- **orphan-sweep disposition recorded** — adjudicated DEFER with an explicit
  trigger, characterization floor in place.

### Rollback

Per-slice: flip `EFFECT_CORE_HOLD_RELAY`→`false` (slice 3, verbatim `*Legacy`
twins, double-gated with `effectRoutes`) and unset the `effectRoutes` transport
seam (slices 1–2). Whole-P10: revert `650fb09b`, `8c896a80`, `2ab8ec95`,
`70b37f90`, `285122a1`, `f2592534`, `16656dae` to restore the P9 docs-close
`8d09b332`. Slices 4–5 are test/docs-only (no seam).

## P9 complete at `b3666ca8`

P9 is **COMPLETE**. Every daemon HTTP-triggered / async application workflow now
has an Effect primary path with a named rollback seam; the residual Promises are
inside inventoried native adapters; the one justified non-conversion (takeover) is
adjudicated. The full package record — per-slice ledgers, the exit-gate audit
against all three gate clauses, and the compatibility-bridge inventory — is in
[p9-close.md](./evidence/effect/p9-close.md). HEAD == origin at `b3666ca8`; the
whole P9 stack is pushed. Sub-package landings:

| Sub-pkg | Scope | Commits | Disposition |
| --- | --- | --- | --- |
| P9.1 | spawn/revive/dismiss request-path | `03ee63f2` (ladder `7bdff948`..`53148019`) | converted; `EFFECT_CORE_*` + `*Legacy` twins |
| P9.2 | repos/worktrees + 3 routes | `f23cfc9b`/`991c82be`/`8409be88`/`0074a154` | converted; soft-read/preflight/remove settlers, `effectRoutes=null` |
| P9.3 | bounded files read/list/search | `1b07aaa0`/`0a18335c` | converted; `settleFilesystemOperation`, `runBounded` frozen |
| P9.4 | owned-pane mail delivery | `cd65f374` | converted; `EFFECT_CORE_PANE_DELIVERY` (q-corpus 330→334) |
| P9.5 | settings GET + /mail/ack + postMail | `ae6c36fd`/`b3666ca8` | converted; last app-service (`EFFECT_CORE_POST_MAIL`, WRAP) |
| P9.6 | takeover/election | (no source commit) | adjudicated justified NON-conversion |

**Final quiet suites at HEAD `b3666ca8`** (Bun 1.3.14, quiet WSL2, 2026-08-24):
`bun run test` **1,785 pass / 6 skip / 0 fail**; `bun run test:bundle` **1,776
pass / 15 skip / 0 fail** (1,791 tests / 217 files). **Bundle** 648,714 B raw /
171,096 B gzip-9 zlib (18,344 B under the 189,440 B ceiling) / SHA-256
`5202bf81756465cc325ca0edc41d6c5c20c710a8c6d007f2d1dd5ae84ce83c92`; re-verified
live. Trajectory + per-slice SHAs in [p9-close.md](./evidence/effect/p9-close.md) §6.
(The gzip figure is Bun's `Bun.gzipSync(..,{level:9,library:'zlib'})`, the value
the `daemon-bundle-policy` gate enforces; Node's zlib reports 171,436 for the same
bytes — an implementation difference, not an artifact mismatch.)

**Exit-gate leftover sweep** (routes deliberately not P9 targets): GET `/mail` +
GET `/api/watch` → **P10**; `/ws/term` → **P7**; AskUserQuestion
`PermissionRequest` fast-path → **P6.4 freeze**. Carry-forward landmines from the
conversions: do not yield Store on HTTP-bridged fibers; do not yield inside a
SQLite txn callback; keep the ingress `run*With` pin at 2 (`runControlDetached` is
the only unsupervised runner); keep every verify list that touches `q.*` sites
pinned to the P8.5 q-corpus tripwire (330→334 after P9.4, stable through P9.5).

### P9.1 — spawn/revive/dismiss (`03ee63f2`)

P9.1 converts the spawn/revive/dismiss **request-path** async orchestration
(`spawns.ts` + the dismiss pair in `retention.ts`) to Effect cores under the
HTTP-CAPABILITY convention (`R=never`, `E=never`, capabilities-as-params; expected
outcomes are `ControlWire` data, not typed errors; discharged through the
ctx-resident `runControlDetached`, the sole sanctioned unsupervised runner). The
root-fiber legs (`spawnLivenessTick` / `reconcileSpawns` / `reconcileClearForks`)
were already Effects under P8.6 and were NOT reopened.

### Eight-slice ladder (commit · verdict)

| Slice | Commit | Verdict |
| --- | --- | --- |
| 0 — arm-unsupervised | `7bdff948` | line review (no adversarial pass, per §6 Q4) |
| 1 — plan-claim release structural | `80615321` | SHIP-WITH-NITS |
| 2 — dismiss pair | `69d3d98b` | DO-NOT-SHIP → fixed (runner pin + tripwire bump) |
| 3 — spawnKill | `6835de2e` | SHIP (no findings) |
| 4 — enableRemote | `3c88576c` | SHIP-WITH-NITS (2 LOW) |
| 5 — revive + adoptSession | `d0083d01` | SHIP-WITH-NITS |
| 6a — /api/spawn transport | `0dfce719` | SHIP (no surviving findings) |
| 6b — spawn core | `53148019` | SHIP-WITH-NITS → both fixed |
| tripwire re-baseline (test-only) | `03ee63f2` | q-corpus `qCalls` 303 → 330 |

Per-core `EFFECT_CORE_*` kill-switch flags (default true) keep verbatim `*Legacy`
twins in-tree as the per-slice rollback; the ingress `run*With` pin held at 2
throughout. Full per-slice one-liners, findings applied, and the slice→commit
reconciliation are in [p9-1-design.md](./evidence/effect/p9-1-design.md) §9.

### Quiet global suites at HEAD `03ee63f2`

Bun 1.3.14, quiet WSL2 host, sequential, 2026-08-24. All pushed.

- `bun run test` (source) = **1,693 pass / 6 skip / 0 fail** (210 files).
- `bun run test:bundle` = **1,684 pass / 15 skip / 0 fail** (210 files).
- Was 1-fail on each before `03ee63f2` — the stale P8.5 q-corpus tripwire only.
  Its `qCalls` constant had silently drifted red across five conversions; the
  re-baseline `303 → 330` is its designed maintenance (no assertion weakened,
  slices 6a/6b added zero `q.*` call sites).

### Accepted daemon identity at HEAD `03ee63f2`

`03ee63f2` is TEST-ONLY, so the daemon bundle is byte-identical to slice 6b
(`53148019`):

- Raw: 636,984 B.
- gzip-9 zlib: 169,587 B, **19,853 B** under the 189,440 B ceiling.
- SHA-256: `d6df8703894af59862bf06769cad2d74d41b7a1301dfc361f213dc6a56706dc1`.
- Deterministic (double-build identical).

### Open residuals (detail in [p9-1-design.md](./evidence/effect/p9-1-design.md) §9)

- (a) in-process `adoptSessionEffect {deferred:true}` pin missing (production
  pinned via `adopt.test.ts`); add when the mixed-caller file is next touched.
- (b) `daemon-maintenance` / `p1-spawns-lifecycle` / `spawn-setup` memoryCores omit
  the runner (legacy-path fixtures) — align when next touched.
- (c) Slice 7 deferred (§6 Q2): AbortController→fiber + `spawnMaintenance`
  fiber-pool ownership, gated on dedicated survival/signal tests. Four native
  bridges named: `provisioningOps`, `AbortController`, `runControlDetached`,
  `spawnMaintenance.run`.
- (d) PROCESS LESSON: the P8.5 q-corpus tripwire must be in every verify list that
  touches `q.*` call sites (it drifted red for five slices before it was caught).

### Rollback

Per-slice: flip the relevant `EFFECT_CORE_*` flag to `false` (legacy adapters
remain in-tree). Whole-P9.1: revert `7bdff948`, `80615321`, `69d3d98b`,
`6835de2e`, `3c88576c`, `d0083d01`, `0dfce719`, `53148019`, `03ee63f2` to restore
`79bd7fb9` (the stamped P9.1 design). No cross-module flag day.

### P9.2 — repos + worktrees (`f23cfc9b`/`991c82be`/`8409be88`/`0074a154`)

Converted `GET /api/worktrees`, `POST /api/repos/preflight`, and destructive
`POST /api/worktrees/remove` per the HYBRID ruling (§6 OQ-1): each route slice did
the P6.4 transport wiring AND a degenerate/hybrid core conversion behind a
fail-soft / preflight / remove settler, with the `effectRoutes=null` per-group
rollback. Slice 0 (`f23cfc9b`) pinned the legacy 500-dialects first. Verdicts:
slice 1 line-review; slices 2 & 3 SHIP-WITH-NITS (all nits applied, incl. N1–N5 on
remove). Design [p9-2-design.md](./evidence/effect/p9-2-design.md).

### P9.3 — bounded files (`1b07aaa0`/`0a18335c`)

Converted the `files.ts` read / list / search cores behind
`settleFilesystemOperation`, keeping `runBounded` as the frozen bounded-exec
adapter. Slices 1+2 (`1b07aaa0`, integrated from worktree `tmp/p9-3-design`) + slice
3 search core (`0a18335c`), all SHIP-WITH-NITS with fixups applied — including a
test-only TS2345 typefix on the `execFileP` spy (amended into `1b07aaa0`). Design
[p9-3-design.md](./evidence/effect/p9-3-design.md).

### P9.4 — owned-pane mail delivery (`cd65f374`)

Converted the owned-pane delivery leg to an Effect core behind
`EFFECT_CORE_PANE_DELIVERY`, discharged via `runControlDetached` (DUPLICATE
strategy → q-corpus re-baselined 330→334). SHIP-WITH-NITS, spy-fix applied
(per-harness counting spy + `assertDispatcherLiveness` on the `p1-mail-lifecycle`
delivery cases). Its design-deferred POST /mail core moved to P9.5. Design
[p9-4-design.md](./evidence/effect/p9-4-design.md).

### P9.5 — remaining HTTP-triggered application services (`ae6c36fd`/`b3666ca8`)

The last application-service conversions: `GET /api/settings` (snapshot settler) +
`POST /mail/ack` (mutating settler + `CONTROL_DEFECT`) at `ae6c36fd` (no
`EFFECT_CORE_*` flag — rollback is the `effectRoutes=null` seam), and the
`POST /mail` core at `b3666ca8` as a **WRAP** (`Effect.promise(() =>
postMailImplLegacy(args))` behind `EFFECT_CORE_POST_MAIL`, body byte-identical,
q-corpus stays 334, `p1-mail-lifecycle` 18→25). Design
[p9-5-design.md](./evidence/effect/p9-5-design.md).

### P9.6 — takeover / election (justified NON-conversion)

Adjudicated (§6) as a justified NON-conversion — no source change. The one async
leg (`terminateDaemon`) already reaches Effect through the native
`acquireDaemonResourcesOwned` adapter (`live-layer.ts:217`), and an in-file
`Effect.run*` is forbidden by three green gates (`import-boundaries.test.ts:980`
hook Effect-marker scan; `:772` fail-open floor `FLOOR_SEAM_ALLOW`;
`cli-serve-paths.test.ts:235` bin self-containment). Design
[p9-6-design.md](./evidence/effect/p9-6-design.md).

## Beyond P10

P10 closed GET `/mail` and GET `/api/watch` (they folded in as slices 1–2).
Continue in [effect-migration-plan.md](./effect-migration-plan.md) /
[p9-completion-map.md](./evidence/effect/p9-completion-map.md) order: **P11**
remaining Bun trials (resume here), P12 build/distribution, P13 cleanup/docs, P14
RC rehearsal + version-manifest closure checkpoint. `/ws/term` stays P7;
static/favicon stay P13; the questions orphan sweep is a P10 DEFER whose triggers
(T1/T2) are the earliest a later package would revisit it.

## P8 complete at `cd0470bb`

HEAD `cd0470bb` on Bun 1.3.14, quiet WSL2 host, 2026-08-23. P8.1–P8.7 boxes are
checked. The five root-context conversions complete the checkbox for
root-owned workflows; HTTP-bridged workflows keep capability parameters BY
CONVENTION (P6.4 constraint), holds/questions are P10, termbridge P7, async
shells + leftover HTTP are P9 — per
[p9-completion-map.md](./evidence/effect/p9-completion-map.md). This is the
plan's own boundary, not incompleteness.

### Six item verdicts (P8.3+P8.4 share one landing)

1. **P8.1 static `bun:sqlite` — Complete** `05b40bd5`. Characterization-first;
   the `node:sqlite` fallback was already dead via the 0.23.0 serve() exit-78
   preflight. Null-to-undefined miss normalization and public row types
   preserved.
2. **P8.2 `strict: true` — DO-NOT-ENABLE** `346ee85a` (pin retarget
   `4ff3e393`). Evidence:
   [p8-strict-trial.md](./evidence/effect/p8-strict-trial.md). 100% positional
   corpus; named-bind inversion. `safeIntegers` not enabled.
3. **P8.3+P8.4 Store owner — Complete** `351b376f`. Store `Context.Service`
   owns the single SQLite handle under the root Scope (P6.3 owner pattern).
   Coordinator remains the authoritative close driver via `setStore`.
   Finalize-then-`close(true)` (`sqlite3_close`, not `sqlite3_close_v2`
   deferral). Completes-only root-Scope fallback honoring `storeSafe`.
   Two-finalizer LIFO: store fallback registered before HttpServer so LIFO
   retires the listener first. Adversarial SHIP-WITH-NITS, all items applied.
   Schema version not bumped. Post-close access on a tracked statement throws
   `Statement has finalized`.
4. **P8.5 `db.query()` cache — KEEP prepare-once** `50412bd8`. Evidence:
   [p8-stmt-cache-trial.md](./evidence/effect/p8-stmt-cache-trial.md).
   `Database.query()` is a 20-slot first-20-win cache, not LRU; 92/112
   overflow would leak stmts and break the close invariant. No callsite
   changed.
5. **P8.6 root-context Store yield — Complete** `cd0470bb`. Five slices, one
   family convention, one whole-gen `provideService(Store)`
   (`program.ts:1312`). Per-slice `STORE_BACKED_*` seams remain. See the
   family table below.
6. **P8.7 `@effect/sql-sqlite-bun` — KEEP direct bun:sqlite** `917c4dc8`.
   Evidence: [p8-sql-client-trial.md](./evidence/effect/p8-sql-client-trial.md).
   Trial archival `fd/p8-sqltrial` @ `a673431e`. No finalize-then-`close(true)`
   expressivity, no `{changes, lastInsertRowid}` run-result, ~4.3× slower hot
   read, async-coloring 303 sites.

### Family's five slices

| Slice | Commit | Flag (`program.ts`) | Notes |
| --- | --- | --- | --- |
| retention prune + sweep | `145e9fbd` | `STORE_BACKED_RETENTION` `:1191` | Pilot; SHIP-WITH-NITS (negative undischarged-die pin) |
| boot clear-fork + `reconcileSpawns` | `e0ab862a` | `STORE_BACKED_BOOT` `:1173` | Same `operationalError` both sides |
| agents ingest | `570d8dae` | `STORE_BACKED_AGENTS_INGEST` `:1204` | Injectable 3rd param; default legacy keeps P5 Store-free |
| LAN feed `core.tick` | `570d8dae` | `STORE_BACKED_LAN_TICK` `:1217` | Tick swallowed via `catchTag('LanTickError')` |
| spawn-liveness tick | `cd0470bb` | `STORE_BACKED_LIVENESS` `:1230` | Final slice. `ownedLivenessTick` extracted verbatim — do **not** swap `ownedLegacyPromise`. Adversarial SHIP, zero findings |

Type landmine: annotate `retention: RetentionSchedule<ProcessRunner | Store>`
(`program.ts:1241`) — do **not** add an explicit type arg on
`makeDaemonBackgroundProgram`.

### Quiet global suites at HEAD `cd0470bb`

Run on Bun 1.3.14, quiet WSL2 host, otherwise idle, 2026-08-23. All pushed.

- `bun run test` (source) = **1,658 pass / 6 skip / 0 fail**.
- `bun run test:bundle` = **1,649 pass / 15 skip / 0 fail**.
- The 6/15 skips are the fail-open contract's declared fault-injection skips
  plus platform skips (unchanged from P6).

### Accepted daemon identity at HEAD `cd0470bb`

- Raw: 607,404 B.
- gzip-9 zlib: 165,586 B, **23,854 B** under the 189,440 B ceiling.
- SHA-256: `1f601ddb497f1129da62bbc601ef8af83a1c6b3f3ba339a3e48b979e9ebcde66`.
- Deterministic.

### Exit-gate dispositions at `cd0470bb`

- **migration/restart/durability suites green throughout.**
- **query benchmarks recorded** (P8.5 / P8.7 evidence).
- **DB acquired once and closed after all users** via the Store owner with the
  coordinator authoritative (P8.3 evidence + ordering pins); post-close access
  impossible for tracked statements (`finalize` throws `Statement has
  finalized`).
- **SQL candidate decision recorded (KEEP).**

### Rollback

Per-slice: flip `STORE_BACKED_BOOT` / `STORE_BACKED_RETENTION` /
`STORE_BACKED_AGENTS_INGEST` / `STORE_BACKED_LAN_TICK` /
`STORE_BACKED_LIVENESS` to `false` (legacy adapters remain in-tree).
Whole-slice: revert the P8 commits `05b40bd5`, `346ee85a`, `4ff3e393`,
`351b376f`, `145e9fbd`, `50412bd8`, `917c4dc8`, `e0ab862a`, `570d8dae`,
`cd0470bb` to restore the store seam to P8.1's parent `37e07659`. Do **not**
range-revert `05b40bd5^..cd0470bb`: that also drops interleaved P6.8
`742168a4` (POST `/command` harness). `4ff3e393`'s parent is `346ee85a` (the
P8.2 trial itself), not the pre-P8 restore point.

Detached-work allowlist is unchanged: P8 added no new exceptions. The two
reviewed exceptions remain the P5 `Effect.forkDetach` in `background-owner.ts`
and the P6 unref'd `HOOK_REPLY_FLOOR_MS` timer.

## P5 completion at `ca62b94f`

HEAD `ca62b94f` on Bun 1.3.14, quiet WSL2 host, 2026-08-22. Evidence:
[p5.md](./evidence/effect/p5.md). Ledger:
[migration-ledger.md](./evidence/effect/migration-ledger.md).

### Outcome of the three open P5 gates

1. **Prompt defect publication — closed** by `02d25b62`. Each of the four
   top-level background children is wrapped in `Effect.onExit` publishing every
   unexpected non-success exit to the ready/failure latches (first-wins
   `Deferred.doneUnsafe`) before the aggregate sibling join. Requested-shutdown
   interrupts are suppressed via a shared `shutdownRequested` box that
   `interrupt()` flips synchronously before `fiber.interruptUnsafe()`.
   Regression `tests/effect/background-prompt-failure.test.ts`: (a) a defecting
   child fails the root promptly while a sibling is stuck in a never-settling
   `ownedLegacyPromise`, within a bounded close; (b) requested shutdown does
   not trip the failure latch. Mutation check: neutralizing the observe wrapper
   makes test (a) fail. Adversarial review (independent model, grok-4.6):
   verdict SHIP, zero findings across eight attack angles, verified against
   `effect@4.0.0-rc.110` internals (`OnExit` is an uninterruptible stack
   continuation completing before `FiberImpl` publishes the exit to observers;
   `forEachConcurrent` reacts only via `addObserver`; `Cause.hasInterruptsOnly`
   cannot suppress mixed defect+interrupt causes). One noted benign delta:
   `owner.close()` after a prompt defect can resolve as requested-interrupt
   instead of rejecting `BackgroundDefectExitError`; production shutdown keys
   off `Background.awaitFailure` (`shutdownTriggerFromExit`), not `close()`'s
   Promise.
2. **Detached-owner exception — accepted.** The single `Effect.forkDetach` in
   `background-owner.ts` is the one reviewed deadline-bounded detached owner: it
   exists so a stuck legacy Promise cannot make root Scope closure exceed the
   P4 hard deadline; the owner is manually registered, interrupted, and
   bounded-joined. Making every legacy bridge cancellation-bounded now was
   rejected. The ledger's former “detached work: None permitted” statement is
   amended to name exactly this exception.
3. **Rollback — whole-slice only.** Reverting the P5 slice commits `972621d5`
   through `ca62b94f` (listed in [p5.md](./evidence/effect/p5.md)) returns to
   the P4 root-cutover anchor `661dfe31`. The previous implication of a
   selectable per-scheduler rollback is dropped because `createCore` no longer
   owns the retention scheduler. No per-scheduler claim remains.

### Outcome of the open bundle-size gate

Closed by `a857bce3` without raising a ceiling and without whitespace
minification. Esbuild flags are unchanged.

- Five secret-redaction regexes (GitLab family, `AIza`, `sk-`, `hf_`,
  `dop_v1_`) had been appended to `SECRET_VALUE_RES` a second time with a
  redundant comment block; the duplicates were deleted (originals remain, each
  pattern now runs once).
- The bundle script gained a post-step deleting esbuild's `/* @__PURE__ */`
  annotations from the generated daemon (bundler hints, meaningless in a
  terminal artifact).

Accepted daemon identity at HEAD `ca62b94f`:

- Raw: 651,685 B, below the 768,000 B ceiling (116,315 B headroom).
- gzip-9 zlib: 188,452 B, 988 B under the 189,440 B ceiling.
- SHA-256: `89b3ef7723961102cb55f986f1710524ee96d55a093db09e7b963c324ee801d9`.
- Double-build byte-identical.
- `tests/effect/daemon-bundle-policy.test.ts`: 2 pass, 0 fail.
- Bundle 19,206 lines; diagnostic name strings present.

The pre-fix P5 checkpoint identity (666,615 B raw / 189,773 B gzip-9, 333 B
over ceiling, SHA-256
`529a1612068705c2e3deccce8574f340bfc6c565307cbe2aece60bc27da6223a`) is
historical and is not the accepted P5 artifact.

### Fixture closures on the same slice

`d53e57a3` and `ca62b94f` (no assertion weakened, 2000 ms budget not raised):

- Packed-install smoke now substitutes the tracked bundle path across the
  recipe tail, so the PURE-strip post-step cannot mutate the committed artifact
  while comparing an unstripped scratch build.
- P1 clone shim arms its TERM/INT trap before writing readiness (trap-window
  race was 6/300 at 44-way oversubscription, 0/300 trap-first).
- Acquisition fixture scrubs `FLEETDECK_TEST_DAEMON_SCRIPT` from the child env
  so bundle-mode runs do not emit the boot seam banner.
- Clone shim block is `sleep 1 & wait` so dash services a pending group-SIGTERM
  immediately. Differential bisection: `tests/process-driver-reference.test.ts`
  immediately before `p1-spawns-lifecycle` reproduced 3/3 pre-fix, 5/5 green
  post-fix; four other driver-heavy prefixes never failed.

### Quiet global suites at HEAD `ca62b94f`

Run on Bun 1.3.14, quiet WSL2 host, otherwise idle, 2026-08-22:

- `bun run test` = 1,493 pass, 0 fail, 186 files, 538.60 s, exit 0.
- `bun run test:bundle` = 1,484 pass, 9 skip (platform skips), 0 fail, exit 0.

Logs: `/tmp/fd-effect/quiet-test-3.log` and
`/tmp/fd-effect/quiet-test-bundle-3.log`.

## P5 production wiring (from `972621d`)

The production integration at `972621d`, still true at HEAD:

- `prepareBackgroundOwner` separates cold preparation from one idempotent fiber
  start.
- One aggregate background program owns boot reconciliation, retention, agents
  polling, and LAN refresh.
- `AppConfigLive` replaces the transitional root configuration service.
- `acquireDaemonResources` receives the prepared Background service/controller
  and returns a cold background program.
- The live Layer starts and registers the single Background owner before
  lifecycle ownership is sealed, then publishes both `Background` and
  `DaemonLifecycle`.
- The root waits on `Background.awaitFailure` instead of an unconditional
  `Effect.never`.
- The legacy boot Promise chain, legacy agents-poll start, and legacy
  network-watch start are no longer production entrypoints.
- `createCore` no longer owns the boot retention sweep or ten-minute retention
  interval. It exposes narrow `retentionSweep` and `pruneEvents` capabilities
  for the Effect schedule.
- Legacy agents and network cadence timers are no longer unref'ed; explicit
  owner shutdown now governs natural exit.
- A real BunRuntime fixture proves the three async scheduler finalizers finish
  before store close, callbacks remain stable afterward, and the process exits
  naturally without `process.exit`.
- The sole `Effect.cached` startup wrapper was replaced with a cold,
  single-assignment `Deferred` gate.

The question orphan sweep remains the explicit P1 handle until P10 (the allowed
P5.5 choice). P5.1–P5.7 plan boxes are checked.

The same integration checkpoint also stabilized two previously failing test
fixtures (BUG-145 tmux cleanup; BUG-153 spawn-reconciliation durable-row
assertion). Those remain in the tree; they are not re-litigated here.

## P4 evidence remains historical

Accepted P4 evidence in [p4.md](./evidence/effect/p4.md) and
[p4-shutdown.json](./evidence/effect/p4-shutdown.json) measures the pre-P5 tree:

- Source closure prefix: `15c62de5…`.
- Daemon bundle: 688,199 B raw and 186,978 B gzip-9.
- Bundle SHA-256: `da3c674a088d9ff2ab624f422c85c2095cec389f6ac2e57343a95bbc6e8d6e88`.
- 100 measured shutdowns plus ten warmups passed their exit, response,
  WebSocket, discovery, pidfile, listener, socket, child, timer, and
  root-keepalive residue gates.

That evidence is still valid for the P4 checkpoint, but it must not be rewritten
as if it describes the current P5 artifact.

## P6 complete at `b2d11d84`

P6 is complete. Sub-slices (historical, all landed):

- **P6.1** freeze (`46e13c50` evidence, `9332d576` plan box, `307fae0a` freeze
  tests). Matrix: [p6-http-matrix.md](./evidence/effect/p6-http-matrix.md). WS
  send probe: [p6-ws-send-probe.md](./evidence/effect/p6-ws-send-probe.md).
- **P6.2** pure policy extract `d425cc96`; tripwire retarget `3735759e`.
- **P6.3** `e7900bac` (`feat(effect): own the Bun listener as the HttpServer
  root service`). `HttpServer` Context.Service is published beside Background;
  `makeHttpServerOwner` wraps the byte-unchanged `createHttp` transport; the
  direct `http.bind` production entrypoint is retired for `httpServer.bind`.
  Root-Scope retirement fallback is registered during acquire (LIFO after the
  coordinator's release). Sole bridge = `IngressSupervisor.runPromiseExit`
  (readiness on first bind; `HttpServer.runRequest` with
  `ApplicationQuiescingError` refusal). Adversarial review: SHIP-WITH-NITS;
  both SHOULD-FIXes applied. P6.3-commit identity (historical, not HEAD):
  654,290 B raw / 189,229 B gzip-9 / SHA-256
  `0b89888b4336531fa91500833b0c16f1be4dfb01a34c47e7deea7caffa5c1d5f`.
- **P6.7** **KEEP CUSTOM ADAPTER**. Evidence:
  [p6-transport-trial.md](./evidence/effect/p6-transport-trial.md). Four
  blocking rc.110 gaps (no `getBufferedAmount`/`terminate`/`ping`/`drain`;
  `upgrade()` overwrites custom `ws.data`; shutdown cannot express
  `stop(false)`-once-race-deadline-`stop(true)` and ends in `process.exit(130)`;
  `server.timeout(request, N)` is TS-private) plus a gzip delta that alone
  exceeds the ceiling.
- **P6.4** complete — see below.
- **P6.5** preserved as implemented (probe + matrix §3; WS slice extracted the
  decisions as pure leaves byte-identically; `ws-hardening` / `terminal-ws`
  stayed green).
- **P6.6** complete. All seven clauses implemented and pinned. Evidence:
  [p6-graceful-stop-verification.md](./evidence/effect/p6-graceful-stop-verification.md).
  Nuance: `stop(true)` is awaited on every path except hard absolute-deadline
  exhaustion (deliberate, pinned).
- **P6.8** within budget. 12/12 `/health`+`/state` p95 cells PASS the +10%
  line. Evidence: [p6-bench-comparison.md](./evidence/effect/p6-bench-comparison.md);
  [p6-postconv.json](./evidence/effect/p6-postconv.json) vs
  [p6-baseline.json](./evidence/effect/p6-baseline.json). The `/command`
  harness gap is closed: `command` is a tenth workload (`POST {text: note}`
  → 200 `core.command` relay) in `p6-http-bench.ts` and `--workload=all`.
  Quiet-host numbers wait on the next idle-machine slot; smoke-only until then.

### P6.4 wave and closing slices

Wave evidence: [p6-route-wave.md](./evidence/effect/p6-route-wave.md). Matrix
overlay: [p6-http-matrix.md](./evidence/effect/p6-http-matrix.md).

| Commit | Slice | Review |
| --- | --- | --- |
| `56a15e8a` | health/state (G1, G2) — P6.4 pilot; three template SHOULD-FIXes applied | SHIP-WITH-NITS |
| `cffb9dea` | paste-image (P8); static recorded legacy-until-P13 | SHIP-WITH-NITS |
| `c7eeb641` | settings/command/mail/cleanup (P6, P7, P3, P4) | initially **DO-NOT-SHIP** |
| `62ef3c0f` | control, 11 routes (P12–P22) | initially **DO-NOT-SHIP** |
| `e2518a63` | join-on-interrupt + bundle line-comment strip | **CLOSED-SHIP-WITH-NITS** |
| `a1ea6020` | `/ws` snapshot converted-by-ownership + pure leaves; `/ws/term` until P7 | — |
| `3eeb2641` | hook fail-open contract | — |
| `b2d11d84` | hook route group (LAST; fail-open settler) | SHIP-WITH-NITS; both nits applied |

**Defect family (headline — record honestly).** The bridge's `runPromiseExit`
was `runPromise(Effect.exit(...))` and **rejected** under external interruption
(settler defect 500 instead of any Exit); and async mutating settlers answered
503 while a started native Promise still owned SQLite/tmux, unsticking
`res.done` so `closeClients` no longer joined in-flight mutations before
closing-store (risks: `SQLITE_MISUSE`, partial cleanup Clear, silent
success-after-refusal). All 190+ green tests missed it; two independent
adversarial reviews caught it on both slices. Fix: `Effect.runPromiseExitWith`
at the bridge (true Exits; P4 contracts re-verified) + `startOnce` witness with
join-on-interrupt (503 only when the operation provably never ran); live-bridge
tests now pin join-before-resolve. Residual accepted nit: a shutdown-only
duplicate error-log line on controlAsync interrupt × rejection races (bytes
single and correct).

**18 HTTP routes + hooks** through the Effect bridge. Excluded-by-design: GET
`/mail` + GET `/api/watch` (held/lease semantics, under P1 owners until P10),
static assets (legacy until P13). `/ws/term` stays behind the termbridge
facade until P7. Leftover application handlers (P2 ack, P5 worktrees/remove,
P9 arm, P10 preflight, P11 spawn, G3 GET settings, G4–G6 fs/worktrees) remain
legacy for later packages, not P6 incompleteness.

**Exit-gate dispositions at `b2d11d84`:**

- **no scattered runtime runners** — the sole bridge is
  `IngressSupervisor.runPromiseExit` via `HttpServer.runRequest`
  (import-boundary + owner tests).
- **exact HTTP/WS parity** — freeze tests + fail-open contract + byte-identity
  fixtures across every slice.
- **cancellation on disconnect where safe** — DELIBERATELY NONE YET: admitted
  requests JOIN their operations (the frozen `res.done` invariant restored by
  `e2518a63`); disconnect-triggered cancellation was judged not-yet-safe for
  converted routes and is deferred to the packages that make the underlying
  operations abort-aware. This is the reviewed disposition, not a silent pass.
- **all transport resources root-owned** — P6.3 owner + fallback ordering test.
- **performance within budget** — P6.8 evidence.

**Quiet global suites at HEAD `b2d11d84`** (Bun 1.3.14, quiet WSL2, 2026-08-23):

- `bun run test` (source) = **1,578 pass / 6 skip / 0 fail**.
- `bun run test:bundle` one run had a single fail in
  `tests/process-driver-reference.test.ts` ("Node reference close is
  idempotent...", 1030 ms). **5/5 green** in bundle-mode isolation; the full
  rerun was **1,569 pass / 15 skip / 0 fail**. Recorded as a suite-context
  flake occurrence in a historically timing-sensitive file (second documented
  incident involving this file; a recurrence earns bisection).
- The 6/15 skips are the fail-open contract's declared fault-injection skips
  plus platform skips.

**Accepted daemon identity at HEAD `b2d11d84`:**

- Raw: 603,458 B.
- gzip-9 zlib: 164,890 B, **24,550 B** under the 189,440 B ceiling.
- SHA-256: `cab4fbc2f66565d72e52f62c52e00edd73c359798fc5f590f18fe43cebb7c874`.
- Deterministic.

Standing landmine: version manifests still `0.23.6` while daemon + bundle
changed. `hook-integrity` needs a version bump ×4 before a PR to main /
publish. Not a reason to revert conversions.

### Open notes (do not close from P6)

- P3's paired quiet-host performance evidence remains an explicit ledger item.
- Quiet-host recapture of the tenth (`command`) harness workload — smoke-only
  until the next idle-machine slot; do not mix with the nine-workload
  `comparison.key`.
- Suite-context flake in `tests/process-driver-reference.test.ts` as above.
- Version-manifest landmine (`0.23.6` ×4) still stands for any merge-to-main.

## Exact resume order

P10 is closed. Slices 0–4 are pushed (origin at `f2592534`); the slice-5
orphan-sweep DEFER (`16656dae`, test + docs only) is local-only, 1 ahead. The next
session resumes at **P11 — remaining Bun trials**, in the order of
[effect-migration-plan.md](./effect-migration-plan.md) P11 and
[p9-completion-map.md](./evidence/effect/p9-completion-map.md). GET `/mail` and GET
`/api/watch` are now converted (P10 slices 1–2). P7 remains the standing §7 platform
authorization checkpoint — BLOCKED on Luis's draft-PR authorization, not the local
next slice; `/ws/term` stays behind the termbridge facade until P7.

1. Confirm the checkpoint and runtime:

   ```sh
   git switch fd/v1-effect-feasibility
   git status --short
   git log --oneline -12
   bun --version
   ```

   Expected HEAD is `16656dae` and origin is `f2592534` (the slice-5 test+docs
   commit is local-only, **1 ahead / 0 behind**). Push `16656dae` before opening
   any PR. This P10-close documentation
   (`docs/v1/effect-migration-plan.md`, `docs/v1/effect-migration-status.md`,
   `docs/v1/evidence/effect/migration-ledger.md`,
   `docs/v1/evidence/effect/p10-close.md`, and the stamped
   `docs/v1/evidence/effect/p10-design.md`) may still be uncommitted; do not switch
   branches. Leave untracked `.claude/agents/` and `/tmp/fd-wt-*` alone.

2. Start **P11** from [effect-migration-plan.md](./effect-migration-plan.md) P11
   (the remaining Bun-native trials). Carry-forward landmines from P9/P10: do not
   yield Store on HTTP-bridged fibers; do not yield inside a SQLite txn callback;
   keep the ingress `run*With` pin at 2 (`runControlDetached` is the only
   unsupervised runner); keep every verify list that touches `q.*` sites pinned to
   the P8.5 q-corpus tripwire (334); do not route held answers through `mapHookExit`
   (the `{body:{body:obj}}` double-wrap); the questions orphan sweep stays a DEFER
   until its T1/T2 trigger fires.

3. P7 stays paused until Luis authorizes the draft PR (see standing notes). It is
   not sequenced before P11.

## Standing open notes (do not close from P8/P9/P10)

- **P7 is BLOCKED on Luis's draft-PR authorization.** It awaits the §7
  platform authorization checkpoint (push + draft PR so the blocking
  macOS/real-tmux and Linux lifecycle jobs can run). The implementation through
  `f2592534` is already on origin; the slice-5 DEFER commit `16656dae` and this
  documentation are not — push `16656dae` as part of that checkpoint. At the
  checkpoint `hook-integrity` may be intentionally red because version closure has
  not happened; record that expected failure, but P7 cannot close until its named
  platform jobs are actually green. Authorization has not been given, so the
  checkpoint stays paused. P10 is now complete, so P7 is the only migration work
  gated on that authorization — sequence P11 ahead of it locally.
- **Questions orphan-sweep DEFER (P10 slice 5).** The sweep stays a domain-owned
  `setInterval` (adjudicated §6-Q2, risk > value). It is NOT incompleteness: the
  §2D redundancy proof and a 3-test characterization floor back it. Revisit only on
  trigger **T1** (a later package lifts the hold Maps into Effect-owned state) or
  **T2** (the sweep stops being redundant with the per-hold timers). Detail:
  [p10-slice5-defer.md](./evidence/effect/p10-slice5-defer.md).
- **P3's** paired quiet-host performance evidence remains an explicit ledger
  item. Do not mark it closed.
- Quiet-host recapture of the tenth (`/command`) harness workload —
  smoke-only until the next idle-machine slot; do not mix with the
  nine-workload `comparison.key`.
- Suite-context flake watch in `tests/process-driver-reference.test.ts`
  ("Node reference close is idempotent..."). One documented incident at P6
  completion; a recurrence earns bisection.
- Version-manifest landmine (`0.23.6` ×4) still stands for any merge-to-main.
  `hook-integrity` needs a version bump ×4 before a PR to main / publish. This is
  the **P14** RC-rehearsal / version-manifest closure checkpoint — do it there, not
  piecemeal. Not a reason to revert conversions.

## P7 preflight constraints (still in force; not the local next slice)

From the plan's P7 section and the constraints still in force from P6/P8:

- The termbridge facade is the seam (`openViewer()` Promise/handle
  compatibility) until the terminal WS route is fully Effect-native (P7.6);
  then remove the facade and its runtime bridge.
- Do not use unbounded `Stream.runCollect`, `ChildProcessSpawner.string`, or
  an unbounded Queue. Buffer limits and drop/backpressure policy are external
  behavior. Every Queue has an explicit `Queue.shutdown` finalizer; root
  PubSubs also have explicit shutdown finalizers.
- A command timeout tears down the compromised shared client as today. Viewer
  child scopes close only that viewer; ref-count or root closure owns the
  shared client lifetime.
- Keep hook holds/watch waiters under their P1 owners until P10.
- Request-bridged Effects must NOT `yield* HttpServer` / `DaemonLifecycle` /
  `Background` / `Store` — the ingress runtime captured the pre-daemon
  Context (`R` must stay within
  `AppConfig | ProcessRunner | ProcessRuntimeControl`). HTTP-bridged
  workflows keep capability parameters (P6.4 / P8.6 convention).
- Consumers key off `HttpServer.state()`, never a retained address value.
- Preserve the established held-response barriers and `stop(false)` /
  `stop(true)` ordering on Bun 1.3.14. Keep the custom HTTP adapter.
- Disconnect-triggered cancellation of converted HTTP routes is **not** in
  P7's (or P9's) gift: admitted requests JOIN (P6 exit-gate disposition). Do
  not enable request interruption until the underlying operations are
  abort-aware.
- One integrator should own shared edits to `http.ts`, `live-layer.ts`, and
  `program.ts`.
- Full P6 revert must include live-layer wiring, not only `program.ts`.
  Per-group HTTP rollback remains `effectRoutes=null`. Per-slice P8 rollback
  remains the `STORE_BACKED_*` flags.

## Repository handoff expectation

This P10-close documentation is currently uncommitted. Implementation HEAD is
`16656dae`, **1 commit ahead** of `origin/fd/v1-effect-feasibility` at `f2592534`
(slices 0–4 pushed; the slice-5 orphan-sweep DEFER `16656dae`, test + docs only, is
local-only). No pull request has been opened. After the documentation is committed
and `16656dae` is pushed, the next session resumes at **P11** from
`fd/v1-effect-feasibility`, using [effect-migration-plan.md](./effect-migration-plan.md)
P11 and [p9-completion-map.md](./evidence/effect/p9-completion-map.md); the P10
package record is [p10-close.md](./evidence/effect/p10-close.md) and the P9 record is
[p9-close.md](./evidence/effect/p9-close.md). Leave untracked `.claude/agents/` and
`/tmp/fd-wt-*` alone.
