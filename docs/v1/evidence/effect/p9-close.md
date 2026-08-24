# P9 CLOSE — async application shells are Effects

> **Written 2026-08-24.** Package close record for **P9** ("daemon asynchronous
> application workflows become Effects") on branch `fd/v1-effect-feasibility`.
> HEAD `b3666ca8` **== `origin/fd/v1-effect-feasibility`** (0 ahead / 0 behind —
> the entire P9 stack is **pushed**). Sibling records: the per-sub-package
> designs `p9-1-design.md` … `p9-6-design.md` (each carries its own §6/§9
> adjudication and slice ladder); this file is the roll-up + exit-gate audit.

P9 is **COMPLETE**. Every daemon HTTP-triggered / async application workflow now
has an Effect primary path with a named rollback seam; the residual Promises are
inside inventoried adapters; the one justified non-conversion (takeover) is
adjudicated and bridge-inventoried below. The next open package is **P10**
(questions, holds, fail-open cleanup).

---

## 1. Sub-package roll-up

| Sub-pkg | Scope | Disposition | Landed | Primary seam |
|---|---|---|---|---|
| **P9.1** | spawn / revive / dismiss orchestration + supervised launch | Complete (closed earlier) | `03ee63f2` code tip; docs `84a4221d` | per-core `EFFECT_CORE_*` + `*Legacy` twins; ingress `run*With` pin = 2 |
| **P9.2** | repo / worktree / git async workflows | Complete — 4 slices | `f23cfc9b`, `991c82be`, `8409be88`, `0074a154` | soft-read / preflight / remove settlers; `effectRoutes=null` |
| **P9.3** | bounded files / list / search cores | Complete — 3 slices | `1b07aaa0`, `0a18335c` | `settleFilesystemOperation`; `runBounded` (frozen adapter) |
| **P9.4** | owned-pane mail delivery core | Complete — 2 slices | `cd65f374` | `EFFECT_CORE_PANE_DELIVERY`; `runControlDetached` |
| **P9.5** | remaining HTTP-triggered app services | Complete — 3 slices | `ae6c36fd`, `b3666ca8` | snapshot / mutating / async-mutating settlers; `EFFECT_CORE_POST_MAIL` |
| **P9.6** | takeover / election lifecycle | **Adjudicated justified NON-conversion** | (no source commit) | `acquireDaemonResourcesOwned` native adapter (inventoried) |

All P9.2–P9.5 conversions follow the HTTP-CAPABILITY convention (`R = never`,
`E = never`, capabilities-as-params; expected `{status,body}` outcomes are DATA,
only a genuine throw → die → transport defect arm). Per-slice rollback is either
a `EFFECT_CORE_*` kill-switch with a verbatim `*Legacy` twin, or the per-group
`effectRoutes === null` HTTP seam — noted per row below.

---

## 2. Slice ledgers

### P9.2 — repo / worktree / git (design: `p9-2-design.md`)

| Slice | What | Commit | Verdict | Findings applied |
|---|---|---|---|---|
| 0 | characterization — pin the legacy 500-dialects of the 4 target routes (test-only) | `f23cfc9b` | (pin slice — no adversarial pass) | 4 legacy 500-dialect pins added; no source touched |
| 1 | `GET /api/worktrees` → degenerate zero-gate core + fail-soft **never-500** settler (`settleEffectSoftReadRoute`) | `991c82be` | line-review (§6-OQ-4 slice-1) | integrated as-reviewed |
| 2 | `POST /api/repos/preflight` → verbatim degenerate core + `settleEffectPreflightRoute` / `PREFLIGHT_DEFECT` (82/0 additive) | `8409be88` | **SHIP-WITH-NITS** | coverage/comment nits applied (fixups) before integrate |
| 3 | `POST /api/worktrees/remove` → **HYBRID** core (§6-OQ-1) + `settleEffectRemoveRoute` / `REMOVE_DEFECT` (destructive route) | `0074a154` | **SHIP-WITH-NITS** | N1–N5 all applied in `584e21c0` (txn-handle spy, 2 Effect-path race pins, fold-body comment, dispatcher-tail reject pin, slice-0 anchor refresh), then cherry-picked clean |

`q`-corpus unchanged across P9.2 (stays 334). `import-boundaries` keeps
`runPromiseWith` in the platform file only.

### P9.3 — bounded files / list / search (design: `p9-3-design.md`)

| Slice | What | Commit | Verdict | Findings applied |
|---|---|---|---|---|
| 1+2 | files **read** + **list** cores → Effect via `settleFilesystemOperation`; `runBounded` kept as the frozen bounded-exec adapter | `1b07aaa0` | **SHIP-WITH-NITS** | suite nits + a test-only **TS2345** typefix: `spyOn(exec,'execFileP').mockImplementation((): Promise<never> => …)` to satisfy both `execFileP` overloads (first integration `ed48ab46` → amended to `1b07aaa0`; `src/` untouched) |
| 3 | files **search** core → Effect (twin per §6 binding) | `0a18335c` | **SHIP-WITH-NITS** | 2 suite nits (new D3 pin + stale comments) applied |

`runBounded` stays a native bounded-exec bridge (inventoried below) — the P9.3
cores call it; they do not inline `Effect.run*`.

### P9.4 — owned-pane mail delivery (design: `p9-4-design.md`)

| Slice | What | Commit | Verdict | Findings applied |
|---|---|---|---|---|
| 1 (characterization) + 2 (conversion) | owned-pane delivery leg → Effect core behind one `EFFECT_CORE_PANE_DELIVERY` flag, run via `runControlDetached`; **DUPLICATE** strategy (a second `q.*` tail) | `cd65f374` | **SHIP-WITH-NITS** (Slice 2 adversarial-review-mandatory per §6) | spy-fix (Finding 1): per-harness counting spy + `assertDispatcherLiveness(effect=1, legacy=0)` on all 8 delivery-driving cases; `p1-mail-lifecycle` grew to 18; integration re-baselined **q-corpus 330 → 334** + bundle rebuild |

§3's Slice 3 (POST /mail core) was **deferred to P9.5** by design (Q1), not
landed here — see P9.5 slice 3.

### P9.5 — remaining HTTP-triggered application services (design: `p9-5-design.md`)

| Slice | What | Commit | Verdict | Findings applied |
|---|---|---|---|---|
| 1 | `GET /api/settings` snapshot READ → `settingsSnapshotWorkflow` on `settleEffectSnapshotRoute`; capability `resolve = () => core.resolveSettings()` (frozen masked-gateway leaf) | `ae6c36fd` | line review | rollback = `effectRoutes===null` → `legacySettingsReadResponse` (renamed to avoid the TS2393 collision with the POST handler); **no `EFFECT_CORE_*` flag** (task spec superseded the design's proposed flags) |
| 2 | `POST /mail/ack` sync MUTATE → `mailAckWorkflow` on `settleEffectMutatingRoute` + `CONTROL_DEFECT`; capability `ack = () => core.ackMail([ev.mail_id])` (frozen; BUG-034 lease = P10) | `ae6c36fd` | line review | quiesce → `503 {ok:false,reason:'shutting-down'}` (never a legacy replay); defect arm unreachable-by-construction (wired for byte-fidelity); rollback = `effectRoutes===null` verbatim |
| 3 | `POST /mail` core → Effect via **WRAP** (`postMailEffect = Effect.promise(() => postMailImplLegacy(args))`), run via `runControlDetached`, folded by the pre-existing `settleEffectAsyncMutatingRoute` + `MAIL_DEFECT` | `b3666ca8` | **SHIP** (no findings; adversarial-mandatory) | flag `EFFECT_CORE_POST_MAIL` + verbatim `postMailImplLegacy` twin; body byte-identical (mid-body quiesce gate preserved, `own()` latches pre-yield); **q-corpus stays 334** (WRAP, one `q.*` tail); `p1-mail-lifecycle` 18 → 25 (mutation-verified load-bearing) |

P9.5 slice 3 is the **last application-service conversion** in P9.

### P9.6 — takeover / election (design: `p9-6-design.md`)

Adjudicated (§6) as a **justified NON-conversion** — no source change. The one
async leg (`terminateDaemon`, `takeover.ts:298`) already reaches Effect through
the native `acquireDaemonResourcesOwned` adapter, and an in-file `Effect.run*` is
**forbidden by three green gates** (see bridge inventory T-row). The P9.6
"documentation slice" is folded into this close record.

---

## 3. P9 EXIT GATE AUDIT

> **Exit gate** (plan `effect-migration-plan.md:962`, completion-map
> `p9-completion-map.md:415`): *"all daemon asynchronous application workflows
> are Effects; remaining Promises are native callback/Response values inside
> named adapters; every compatibility bridge is inventoried."*

**Clause 1 — "all daemon asynchronous application workflows are Effects."**
Discharged. Every HTTP-triggered application service now has an Effect primary
path with a rollback seam: settings GET/POST, worktrees list/remove, files
read/list/search (via `settleFilesystemOperation`), `POST /mail/ack`,
`POST /mail`, settings-command-mail-cleanup, repos preflight, spawn / kill /
revive / adopt / name / dismiss / dismiss-retry / rc, questions, plans,
paste-image, arm-unsupervised, command — plus the mail owned-pane delivery core
(P9.4). Verified by the P9.5-slice-3 `routeRequest` sweep (`http.ts:2418–3333`):
no direct legacy-only core call remained that P9 should have converted.

**Leftover sweep — routes deliberately NOT converted in P9, each on a named hold list:**

| Route / leg | `http.ts` anchor | Held for |
|---|---|---|
| `GET /mail` (`core.ackMail` + `core.drainMail` lease) | `2524–2539` | **P10** |
| `GET /api/watch` (`watchHook`) | `2541` | **P10** |
| `/ws/term` terminal ownership | `3335+` | **P7** (standing §7 platform authorization; `/ws/term` stays P7 by plan `931`) |
| AskUserQuestion `PermissionRequest` `applyEvent` fast-path | `2674–2680` | **P6.4 freeze** |

**Clause 2 — "remaining Promises are native callback/Response values inside named adapters."**
Discharged. The residual Promises live inside the named adapters inventoried in
§4; no bare unsupervised `Effect.run*` was introduced (`import-boundaries` keeps
`runPromiseWith` in `ingress-supervisor-live.ts` only; ingress `run*With` pin
held at 2 — no new runner site across P9.2–P9.5).

**Clause 3 — "every compatibility bridge is inventoried."** Discharged by §4.

---

## 4. BRIDGE INVENTORY

| Bridge / adapter | Where | Nature | Enforcement / status |
|---|---|---|---|
| `runControlDetached` | `ingress-supervisor-live.ts:52` (`Effect.runPromiseWith(Context.empty())`) | the sole sanctioned context-free **unsupervised** runner; NOT supervisor-tracked | injected at `app/program.ts`; JOIN is via `own()`/`inFlight` + transport `startOnce`, by design (a started body cannot be `supervisor.interrupt()`-cancelled) |
| `provisioningOps` / `AbortController` | P9.1 spawn orchestration | native cancellation bridge behind the spawn cores | inventoried in `p9-1-design.md` §9 |
| `spawnMaintenance.run` | P9.1 | native maintenance-loop bridge | inventoried in `p9-1-design.md` §9 |
| `runBounded` | files cores (P9.3) | frozen bounded-exec adapter the read/list/search cores call | not inlined into the Effect cores; stays native |
| `acquireDaemonResourcesOwned` | `live-layer.ts:217` (`Effect.callback`) | native takeover adapter wrapping `terminateDaemon` | **forbidden to become an in-file `Effect.run*`** by 3 green gates: `import-boundaries.test.ts:980` (hook Effect-marker scan), `import-boundaries.test.ts:772` (fail-open floor, `FLOOR_SEAM_ALLOW`), `cli-serve-paths.test.ts:235` (bin self-containment) |
| pane / tmux delivery leaves | `mail.ts` (`ownedPaneDeliverable`, tmux probes) | frozen native leaves under the P9.4 Effect core | frozen; `probe:true` default preserved |
| `ackMail` / `resolveSettings` leaves | `mail.ts` / settings | frozen native capability leaves under P9.5 settlers | frozen (BUG-034 lease = P10; masked-gateway invariant `'token' in gateway === false`) |

---

## 5. Open residuals (carried out of P9)

- **P10**: `GET /mail` (ackMail + drainMail lease, BUG-034 protocol) and
  `GET /api/watch` (`watchHook`); plus the questions/holds/fail-open timers.
- **P7**: `/ws/term` terminal ownership — still **BLOCKED** on Luis's draft-PR
  authorization at the §7 platform checkpoint (P7.0).
- **P6.4 freeze**: AskUserQuestion `PermissionRequest` fast-path (intentionally
  frozen, not a P9 target).

No P9-scoped application workflow is left unconverted.

---

## 6. Suites and bundle at close

**Quiet full suites (WSL2), HEAD `b3666ca8`** (`p9-5-s3-quiet-suites.log`):

- `bun run test`: **1785 pass / 6 skip / 0 fail** — 1791 tests across 217 files (551.26 s)
- `bun run test:bundle`: **1776 pass / 15 skip / 0 fail** — 1791 tests across 217 files (533.76 s)

Growth from the P9.1 close baseline (`03ee63f2`: 1693/6/0 + 1684/15/0, plan
`941`): **+92** source, **+92** bundle across P9.2–P9.5 (new parity/route/pin
suites; `p1-mail-lifecycle` alone +7 in slice 3). `bun run ci` (biome) 0;
typecheck both legs exit 0 at each integrate.

**Bundle trajectory** — `src/daemon/fleetd.bundle.mjs`, gzip measured by the
project's canonical `Bun.gzipSync(bytes, {level:9, library:'zlib'})`
(`daemon-bundle-policy.test.ts:34`). Ceiling: raw ≤ 768,000 · gzip-9 ≤ 189,440.

| Commit | Sub-pkg | raw B | gzip-9 B | sha256 (prefix) |
|---|---|---|---|---|
| `03ee63f2` | P9.1 close baseline | 636,984 | 169,587 | `d6df8703…` |
| `1b07aaa0` | P9.3 s1+2 | 639,363 | 170,343 | `0a08a5f7…` |
| `cd65f374` | P9.4 s1+2 | 641,255 | 170,618 | `f5e1f777…` |
| `991c82be` | P9.2 s1 | 642,703 | 170,129 | `2d5c3f58…` |
| `0a18335c` | P9.3 s3 | 644,191 | 170,737 | `ffcda479…` |
| `8409be88` | P9.2 s2 | 646,006 | 171,036 | `da961756…` |
| `0074a154` | P9.2 s3 | 647,631 | 171,329 | `06cc0dfa…` |
| `ae6c36fd` | P9.5 s1+2 | 648,639 | 172,413 | `3778ba63…` |
| **`b3666ca8`** | **P9.5 s3 / HEAD** | **648,714** | **171,096** | **`5202bf81…`** |

`f23cfc9b` (P9.2 s0) is test-only — bundle unchanged from `cd65f374`.
`84a4221d` (P9.1 docs close) is docs-only — bundle unchanged from `03ee63f2`.
gzip-9 is **non-monotonic vs raw** (comment-strip + entropy): it dips at
`991c82be` and again at HEAD even as raw grows — the slice-3 WRAP (one-line thunk
replacing a duplicated body) compresses better than the slice-1+2 peak. HEAD
headroom: **18,344 B** gzip · **119,286 B** raw. The version-manifest string
`0.23.6` (×4) remains the standing bundle landmine — a bump reprices this row.

**HEAD `b3666ca8` bundle re-verified this close (live tree):** raw **648,714**
and sha256
`5202bf81756465cc325ca0edc41d6c5c20c710a8c6d007f2d1dd5ae84ce83c92` match the
slice-3 report byte-for-byte.
