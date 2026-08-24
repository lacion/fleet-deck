# P10 CLOSE — holds and fail-open become the held Deferred primitive

> **Written 2026-08-24.** Package close record for **P10** ("questions, holds,
> fail-open cleanup") on branch `fd/v1-effect-feasibility`. HEAD `16656dae`
> (slice 5, the orphan-sweep DEFER). **Push state is split:**
> `origin/fd/v1-effect-feasibility == f2592534` (slice 4) — slices 0–4 are
> **pushed**; slice 5 (`16656dae`, test + docs only) is **1 ahead, local-only,
> not pushed**. The committed bundle is identical either way (the last *source*
> change was slice 3, `285122a1`). Sibling record: the adjudicated design
> [`p10-design.md`](./p10-design.md) (stamped verbatim, §6 orchestrator
> adjudication). This file is the as-landed roll-up + exit-gate audit.

P10 is **COMPLETE**. The two genuine held-response surfaces (hook HOLD relay and
GET /api/watch) now settle through **one parameterized `Deferred` primitive**;
GET /mail landed as a P9-class transport slice; the shutdown-quiesce guarantee
(P10.4) is delivered by the P4-frozen phase order plus the slices-2/3 settlers and
closed with one mixed-load matrix pin; the orphan sweep is a recorded, trigger-gated
**DEFER**. The whole package is the last held-response work in the Effect
migration. The next open package is **P11** (Bun capability trials).

---

## 1. Slice ledger (as landed)

| Slice | Scope | Commit(s) | Review class | Verdict | Findings applied |
|---|---|---|---|---|---|
| **0** | characterization floor — lockstep source-pin (650<660<720) + 25 s watch ceiling; in-process 1A.3 socket-disconnect / 1A.4 cap-evict / 1A.5 `failOpenAllHolds` (test-only, 2 files) | `650fb09b` | line review (pin slice) | — (pins) | 2 new test files, no source seam |
| **1** | `GET /mail` → `mailDrainWorkflow` (`Effect.sync` ack → drain(lease) → broadcast) + `MAIL_DRAIN_DEFECT` (500 `{}`); transport via `installEffectRoutes` | `8c896a80` | line review (§6-Q4 P9-class) | integrated | **stale-design correction recorded:** the draft §2A proposed `CONTROL_DEFECT`; the landed outer-catch dialect is `MAIL_DRAIN_DEFECT` (500 `{}`, http.ts:1514) — a distinct leaf. control-route sibling comment/port pass 32→39 (+8) |
| **2** | `GET /api/watch` held long-poll → the **one** parameterized `Deferred` settle primitive (`heldSettleWorkflow` / `settleEffectWatchHold`); 4-leg first-settlement-wins; discharged on `runControlDetached`; **idle** terminal fold | `70b37f90` (fixture torn-line tolerance) + `2ab8ec95` | **adversarial-mandatory** | **DO-NOT-SHIP → unblocked → SHIP** (see §2) | test-only unblock: new `tests/helpers/http-lifecycle-effect-fixture.ts` + retargeted pins + sibling alignment |
| **3** | hook **HOLD relay** → `settleEffectHookHold` (the final held conversion); flag `EFFECT_CORE_HOLD_RELAY` (http.ts:88, double-gated `effectRoutes && flag`) | `285122a1` | **adversarial-mandatory** | **SHIP-WITH-NITS** (4 nits) → all applied | F1 shared `hookFailOpenBody()`; F2 floor anti-truncation pin; F3 cap-evict + dismiss (#7) HTTP; F4 8 sibling "REJECTS" comments; suite 6→9 |
| **4** | mixed-load D2 shutdown matrix — **collapses to pins** (non-conversion; P9.6 precedent) | `f2592534` | adversarial-mandatory → **pins-only** (source untouched → line review) | pins-only | 1 test (537 L), mutation-proven; no source, no bundle regen |
| **5** | orphan sweep → **DEFER** (risk > value, §6-Q2) | `16656dae` (HEAD, **unpushed**) | per own analysis | **DEFER** | characterization floor (3 pins, 2 mutation-proven) + [`p10-slice5-defer.md`](./p10-slice5-defer.md); triggers T1/T2 |

Rollback seams: slices 1 & 2 land as the per-group **`effectRoutes === null`**
transport seam (`installEffectRoutes` unset) — **no `EFFECT_CORE_*` flag** (the
design's proposed `EFFECT_CORE_GET_MAIL` was superseded by the P9.5-s1+2 transport
convention). Slice 3 is the only P10 kill-switch: `EFFECT_CORE_HOLD_RELAY = true`
(http.ts:88) with verbatim `*Legacy` twins, double-gated so a null `effectRoutes`
**or** a `false` flag both restore the P1 path. Slices 4 & 5 are test/docs-only —
no seam.

---

## 2. Slice 2 honesty record — the DO-NOT-SHIP → SHIP arc

The design (§6) made slices 2/3/4 **adversarial-review-mandatory**. Slice 2's
first adversarial pass returned **DO-NOT-SHIP**, and it was right to:

- **Why it failed.** The conversion itself was sound, but its retargeted 1E-3 /
  1E-4 pins asserted against the **legacy park path**, not the new Effect held
  path. They would have stayed green with the Effect conversion reverted — i.e.
  they proved nothing about the seam the slice existed to add. A held-primitive
  slice whose pins don't touch the primitive is untested-by-construction.
- **The unblock (test-only, no source logic change).**
  1. New `tests/helpers/http-lifecycle-effect-fixture.ts` — installs the **real**
     `installEffectRoutes` and threads a `runHeldCalls` counter through the
     untracked `runControlDetached` runner, so a test can prove the Effect held
     path (not the legacy park) armed the response.
  2. Retargeted 1E-3 / 1E-4 to that fixture, asserting `runHeldCalls === 1`
     (Effect park proven). **Mutation-proven:** forcing the guard to `if (false)`
     made the pin fail with `actual 0 / expected 1`, then reverted.
  3. U3 — in-process null-seam **byte-equivalence** pins (Effect path vs legacy
     path emit identical wire bytes).
  4. U4 — Finding-3: aligned 9 sibling call-sites to the real `runControlDetached`;
     Finding-4: a `SLICE-3-WARNING` comment on the `http.ts` `.catch` (bundle-neutral).
- **Re-review: SHIP.** `tsc` 0 both legs; `bun run ci` clean; `p10-slice2-watch-legs`
  4 pass ×3 on the Effect fixture; `watch-rewake` 18; committed bundle unchanged by
  the comment-only fixups.

This is recorded here rather than smoothed over: the adversarial gate caught a real
test-adequacy defect, the fix was test-only, and the shipped state is clean.

---

## 3. The held-response primitive (the thing P10 built)

**`heldSettleWorkflow`** is the single first-settlement `Deferred` primitive the
design's §2C / Q3 prescribed, landed on `http.ts`:

- **Shape.** `Deferred<HeldOutcome, never>` under `Effect.scoped` + `acquireRelease`.
  The parked `res` is written **exactly once**, from the winning leg's value.
- **First-settlement-wins.** Every racing leg calls `Deferred.doneUnsafe`
  (idempotent) — "first wins, the rest are structural no-ops," which *is* the
  `holds`-map identity guard the P1 manager enforced, expressed in the type.
- **Parameterized on the terminal fold — and only the fold:**
  - **watch** (`settleEffectWatchHold`, http.ts:1399): idle-info fold —
    `{status:'idle', …}` on lapse/shutdown, `{status:'mail', …}` on wake. **Not**
    the hook contract.
  - **hook** (`settleEffectHookHold`): fail-open `{}` fold. Only the board-answer
    leg carries a non-`{}` body; every other leg folds to `hookFailOpenBody()`.
- **Discharge.** `runControlDetached` (`Effect.runPromiseWith(Context.empty())`,
  the P9.4 sanctioned unsupervised runner) — a started held body is deliberately
  **not** `supervisor.interrupt()`-cancellable, so it survives shutdown and settles
  in the correct phase (D2). No new `run*With` runner site was introduced; the
  ingress boundary pin held.
- **Q1 SETTLEMENT-ONLY doctrine (ACCEPTED-and-BINDING, §6).** The P1 hold-manager
  Maps (`holds` + `rearmById/Meta/Chains` + `completedKeys`) **stay imperative**
  behind the policy adapter. The Deferred wraps *response settlement only* — it
  does **not** own the durable row lifecycle. P10 did not deepen Effect ownership
  into the hold manager; that is explicitly left to a post-P10 package (see §6, T1).

---

## 4. The hook fail-open contract — STRENGTHENED, not merely preserved

P10 was the package most able to break `200 {}`. It closed **tighter** than it
opened:

- **One source of the canonical `{}`.** `hookFailOpenBody()` (hook-policy.ts:47)
  is now the single producer of the fail-open body, shared by **both**
  `mapHookExit`'s failure arm (hook-policy.ts:60) **and** `settleEffectHookHold`'s
  `.catch`. Contract-tie pin: `mapHookExit(Exit.die(…)).body` deep-equals
  `hookFailOpenBody()` (interrupt arm too). A future edit to the fail-open body can
  no longer drift the route path and the hold path apart.
- **No double-wrap.** Holds are deliberately **not** routed through `mapHookExit`
  (that renders `{body: …}`); the hold settler renders `.body` directly, so a hold
  never emits `{body:{body:obj}}`.
- **Anti-truncation floor pin (F2).** The `HOOK_REPLY_FLOOR_MS` (5000 ms unref'd
  idempotent floor) must **not** truncate a long park. New pin: a 5.5 s park →
  `received() === 0`, `runHeldCalls === 1`, then settles `200 {}` at close. The
  child env deletes `FLEETDECK_HOOK_REPLY_FLOOR_MS` so a sibling can't shorten it.
  Observed slice-3 wall ~7.5 s ×3 — the hold genuinely outlives the 5 s floor.
- **Fail-open is TOTAL and never a failed Effect (D4).** `Deferred<_, never>`; a
  die in any leg folds to `{}` at completion; the transport terminal is the only
  place the fold lives (P6.4 discipline, extended across time). No Cause, token,
  path, or stack can reach the client.

---

## 5. Slice 4 collapse-to-pins and slice 5 DEFER

**Slice 4 (P10.4, "root quiesce settles all holds before stop") delivered its
exit-gate guarantee without a source conversion** — the P9.6 justified-non-conversion
shape:

- The `releasing-holds → closing-clients → closing-http/store` **ordering** is
  already Effect-owned by P4's frozen `ShutdownPhaseOrder`
  (`lifecycle-coordinator.ts`); the **settlement** inside those phases is already
  Effect (slices 2 + 3); the coordinator body stays imperative by the §6-Q1 binding;
  the failure-injection exit gate ("phase failure retained, later phases still run";
  "forceStop runs AFTER releaseHolds") is already pinned.
- The **one** observational gap was a *mixed-load* shutdown — a hook hold **and** a
  watch long-poll parked in the **same** shutdown. `tests/p10-slice4-shutdown-matrix.test.ts`
  (537 L, 1 test) parks both, sends one `close`, and pins: hook settles `200 {}`
  (releasing-holds fold) **and** watch settles the idle body (closing-clients fold)
  — two distinct parameterized folds in one quiesce — with `runHeldCalls` 1→2 (no
  legacy fallback on either surface) and `ownedCounts → ZERO` (listener released
  only after both joins). **Mutation-proven:** swapping the hook's expected body to
  the watch idle body failed with `actual {}`, proving the folds are distinct;
  reverted.

**Slice 5 (orphan sweep) is a recorded DEFER** (risk > value, §6-Q2) — full
analysis in [`p10-slice5-defer.md`](./p10-slice5-defer.md). Summary:

- The sweep is a domain-owned `setInterval` (questions.ts:1432, `SWEEP_MS=5000`,
  `unref()`) created **inside** `createQuestions`, with **zero** Effect imports; it
  is **not** in the P5 supervised-schedule family, and the P5 ledger already records
  "question orphan sweep stays on the P1 handle until P10."
- Converting it would (i) **break the Q1-BINDING doctrine** — `expireOrphans` reads
  the private imperative `holds` Map every tick, so lifting the fiber deepens Effect
  ownership into the hold manager; (ii) add a **bridge with no Store seam** (raw
  prepared statements, not the Effect Store); (iii) **perturb the P4-frozen D2 phase
  order** (teardown moves from the core-lifecycle `clearInterval` to the
  Background-interruption phase). Value: uniformity only. It does not clear the bar.
- **Explicit re-evaluation triggers:** **T1 (primary)** — a post-P10 package lifts
  the hold Maps into Effect-owned state; the Q1-BINDING block is P10-scoped, so it
  dissolves and the P5-family port becomes cost-free. **T2 (secondary)** — if the
  sweep ever stops being *redundant* with the per-hold `holdMs` timers, it must move
  under supervision immediately.
- A characterization floor (`tests/p10-slice5-orphan-sweep-characterization.test.ts`,
  3 pins: live-socket skip / redundant cleanup / audit-silence; 2 mutation-proven)
  freezes the sweep's contract before any future port.
- **Note on "R7/R8":** that is orchestrator shorthand in §6-Q2 for the plan's
  evidence-backed KEEP/DEFER discipline (plan lines 58 / 1042 / 1285), the same
  shape as the P6.7 / P8.7 KEEP-adapter trials and the P9.6 non-conversion — **not**
  a literal numbered rule.

---

## 6. P10 EXIT GATE AUDIT

> **Exit gate** (plan `effect-migration-plan.md:985-986`): *"all existing and new
> hook/needs-you/board-hold suites pass from source and bundle; every
> shutdown/failure race returns control to the native terminal; no hold timer or
> Deferred is left live."*

**Clause 1 — suites pass from source AND bundle.** Discharged. At `f2592534`
(slice 4, = origin HEAD): **1812 pass / 6 skip / 0 fail** source and **1803 pass /
15 skip / 0 fail** bundle (`p9→p10` growth **+27 / +27** over the P9-close baseline
1785/1776 at `b3666ca8`). Slice 5 (`16656dae`, unpushed) adds **3** source
characterization tests (individually **3 pass ×3**, 2 mutation-proven) that are
**not** in that quiet-suite roll-up. `bun run ci` (biome) clean (447 files);
`typecheck` both legs exit 0 at every integrate. The new hook/watch/hold suites —
`p10-slice2-watch-legs` (4), `p10-slice3-hook-hold-legs` (9), `p10-slice4-shutdown-matrix`
(1), `p10-slice5-orphan-sweep-characterization` (3), `p6-hook-failopen-contract`,
`question-rearm` (9), `watch-rewake`, `board-hold-presence`, `mail-delivery-lease` —
are green on both legs.

**Clause 2 — every shutdown/failure race returns control to the native terminal.**
Discharged. Slices 2 + 3 settle every held leg through the Deferred primitive; the
D1 lockstep (650 < 660 < 720) is byte-frozen and pinned by the 64 s real-timer wall
(`question-rearm` + `watch-rewake`); the D2 shutdown ordering is delivered by the
P4-frozen phase order and closed by the slice-4 mixed-load matrix (hook `200 {}` in
releasing-holds, watch idle in closing-clients, listener released only after both
joins).

**Clause 3 — no hold timer or Deferred left live.** Discharged. Every per-hold
timer is `unref()`'d and cleared on settle; `runControlDetached` held bodies settle
in-phase and are joined before the listener releases (`runHeldCalls === ownedCounts`
→ ZERO at close). The one surviving raw `setInterval` (the orphan sweep) is `unref()`'d,
redundant, characterization-pinned, and a recorded DEFER with named triggers — not a
live untracked timer in the fail-open path.

---

## 7. Bundle at close

`src/daemon/fleetd.bundle.mjs`. Gzip measured by the project's canonical
`Bun.gzipSync(bytes, {level:9, library:'zlib'})` (`daemon-bundle-policy.test.ts:34`).
Ceiling: raw ≤ 768,000 · gzip-9 ≤ 189,440.

| Commit | Slice | raw B | gzip-9 B | sha256 (prefix) |
|---|---|---|---|---|
| `b3666ca8` | P9 close baseline | 648,714 | 171,096 | `5202bf81…` |
| `285122a1` | **P10 slice 3 / last source change** | **652,147** | **172,335** | **`84daf597…`** |

**Re-verified this close (live tree, `16656dae`):** raw **652,147**, sha256
`84daf5972d10316797bbf7a8f6b5996d8801c3612994de49eaf58e56ec20e327`, gzip-9-zlib
**172,335** — byte-identical to the committed slice-3 bundle. Slices 0, 2 (fixups),
4, and 5 are test/docs-only (or comment-only), so the bundle has not changed since
`285122a1`. **Headroom: gzip 17,105 B · raw 115,853 B.** Growth over the P9 close:
raw **+3,433**, gzip-9 **+1,239**.

**Convention flag (verify Deliverable 6).** The per-slice *worker* reports quote a
`gzip -c` (default level 6) figure — slice 1 172,632 → slice 3 173,231 — which is
**not** the policy metric and must not be read against the 189,440 ceiling. The
`daemon-bundle-policy` gate measures `Bun.gzipSync(…,{level:9,library:'zlib'})`; that
authoritative value at HEAD is **172,335** (17,105 under ceiling). Both are recorded
so the `gzip -c` trajectory the slice reports cite is not mistaken for the gate
figure. The version-manifest string `0.23.6` (×4) remains the standing bundle
landmine — a bump reprices this row.

---

## 8. Residuals carried out of P10

- **Orphan sweep** — DEFERRED past P10 behind triggers T1/T2 (§5;
  [`p10-slice5-defer.md`](./p10-slice5-defer.md)). The only surviving raw
  `setInterval`; characterization-pinned.
- **`/ws/term` terminal ownership (P7)** — still **BLOCKED** on Luis's draft-PR
  authorization at the §7 platform checkpoint (P7.0). Untouched by P10.
- **AskUserQuestion `PermissionRequest` fast-path (P6.4 freeze)** — intentionally
  frozen; the instant-`{}` guard (D7) stays before the hold relay and is never a hold.
- **In-memory ephemera (rearm / completedKeys)** — stays in-memory by design (D8 /
  §6-Q5); only teardown is scope-reached. Not a P10 gap.

No P10-scoped held-response surface is left unconverted; the one non-conversion
(orphan sweep) is adjudicated and trigger-gated.
