# P10 Slice 5 — orphan sweep: DEFER record

**Verdict: DEFER (do not convert in P10).** The orphan sweep stays the P1
`setInterval` behind `createQuestions`; this slice ships the characterization
floor (`tests/p10-slice5-orphan-sweep-characterization.test.ts`) and this
evidence-backed defer with an explicit re-evaluation trigger. No source change.

Slice 5 (design [p10-design-draft §2D / §3 / §6-Q2]) was adjudicated
**CONDITIONAL / LAST**: "orphan sweep is conditional/LAST; if the slice-5 analysis
finds risk>value, record a DEFER with an explicit trigger (the R7/R8 convention)
rather than forcing it." The gap analysis below finds risk>value, so this record
is the DEFER. ("R7/R8 convention" is the orchestrator's shorthand for the plan's
evidence-backed KEEP/DEFER discipline — a capability may finish as KEEP/DEFER when
its named gate fails, provided the record carries measured evidence and a named
gate: effect-migration-plan.md lines 58, 1042 exit gate, 1285. This mirrors the
P6.7 / P8.7 KEEP-adapter trials and the P9.6 justified non-conversion.)

Evidence anchors are at branch `fd/v1-effect-feasibility` HEAD `f2592534`.

## §1 — What the orphan sweep IS today (gap analysis, file:line)

- **Cadence.** `const sweep = setInterval(…, orphanSweepMs)` — `questions.ts:1432`.
  `orphanSweepMs = SWEEP_MS = 5000` by default (`questions.ts:175`, `1430-1431`),
  injectable via the `sweepMs` option (test seam). `sweep.unref()` (`1440`) — it
  never holds the event loop open.
- **What it does.** Each tick, when `active()`, calls `expireOrphans()`
  (`questions.ts:1308-1324`). `expireOrphans` walks `q.pending.all()` and:
  - **skips** any non-hold-kind row and any hold-kind row that still owns a live
    socket — `if (!HOLD_KINDS.has(r.kind) || holds.has(r.id)) continue;`
    (`questions.ts:1312`). This is the *redundant-with-the-per-hold-timer* skip:
    a live hold is reaped by its own `holdMs` timer, never by the sweep.
  - **recycles** an aged re-armed row (`recycleRearm`, `1313-1316`) — already
    pinned by `question-rearm.test.ts` ("recycled by the 5 s orphan sweep").
  - **expires** a genuine orphan (pending hold-kind row, no live socket):
    `if (q.markExpired.run(r.id).changes) { changed = true; onRetired(q.get.get(r.id)); }`
    (`1317-1320`), then `if (changed) onChange();` (`1322`). `markExpired` commits
    the durable row BEFORE `onRetired` fires.
- **Failure behavior.** The tick body is
  `try { expireOrphans(); } catch { /* hygiene only */ }` (`questions.ts:1434-1438`)
  — **audit-silent, fail-open by design**. A throw inside a tick (e.g. a wired
  `onRetired`/`onChange` seam that throws) is swallowed; `markExpired` has already
  committed, and the interval survives to the next tick.
- **Ownership / lifecycle.** Created *inside* `createQuestions`
  (`derive.ts:485` → the imperative `createCore` in `program.ts`), torn down in the
  same factory's `close()` via `clearInterval(sweep)` (`questions.ts:1461`), on the
  core-lifecycle path — not the Background-interruption phase.
- **Family membership.** It is **NOT** in the P5 supervised-schedule family.
  `makeDaemonBackgroundProgram` (`app/background-program.ts`) composes exactly
  `agents-poll | lan-refresh | retention` + boot; the retention template
  (`app/retention-schedule.ts`) is Clock + `Effect.repeat` + `fixedGridNoCatchUp`
  under the shared Background owner/root Scope. The orphan sweep is none of these.
  The P5 ledger row records the intent verbatim: "question orphan sweep stays on
  the P1 handle until P10" (migration-ledger.md, P5 row). The completion map:
  "P1 timer, unref'd, keep on explicit P1 handle until P10."

## §2 — Conversion cost vs the P5 family (why risk > value)

A P5-family port would replace the `setInterval` with a scoped
`Effect.repeat(expireOrphans, Schedule.fixed(SWEEP_MS))` + `Clock` fiber forked
under the Background owner. The cost:

1. **Q1-BINDING break (the decisive one).** `questions.ts` imports **zero** Effect
   (`grep -c "from 'effect" src/daemon/questions.ts` → `0`). `expireOrphans` reads
   the private `holds` Map (`1312`) and drives the rearm machinery (`recycleRearm`,
   `1313-1316`) on every tick. Pushing an Effect fiber across that boundary deepens
   Effect ownership INTO the imperative hold manager — exactly what §6-Q1
   (ACCEPTED-and-BINDING) forbids: "hold-manager Maps stay imperative behind the
   policy adapter; do not deepen ownership in P10." The sweep is the hold manager's
   own reaper; it cannot be lifted without lifting the Maps it reaps.
2. **No Store seam.** The sweep uses raw prepared statements on the imperative db
   handle (`q.pending.all()`, `q.markExpired.run`, `q.get.get`), not the Effect
   Store. There is no converted work surface to schedule — a port would have to
   invent an adapter around the imperative closure, adding a bridge P10 is meant to
   remove, not add.
3. **D2 shutdown-ordering risk.** Teardown today is `clearInterval(sweep)` inside
   `questions.close()` (`1461`), sequenced with `releaseAll()` on the core
   lifecycle path. Moving the fiber under the Background owner moves its
   cancellation into the Background-interruption phase — a *different* shutdown
   phase — perturbing the P4-frozen `ShutdownPhaseOrder` for zero behavioral gain.
   The sweep is pure restart hygiene; it never settles a held socket, so it earns
   nothing from Background supervision.
4. **Redundancy/fail-open contract at risk.** Design §2D permits the port ONLY if
   it preserves (i) the deliberate redundant cleanup and (ii) the audit-silent
   `catch {}`. `Effect.repeat` + a typed error channel changes the failure surface;
   reproducing byte-for-byte audit-silence and the live-hold skip through a
   supervised fiber is net-negative work over a 5-line unref'd interval.

**Value of converting:** uniformity only (one fewer raw `setInterval`). Against a
Q1-BINDING doctrine break + a bridge addition + a D2 ordering perturbation, the
value does not clear the bar. **risk > value → DEFER.**

## §3 — DEFER decision and the explicit re-evaluation trigger

The sweep stays the P1 `setInterval`. Convert it when — and only when — **either**
trigger fires:

- **T1 (primary — the block dissolves).** A post-P10 package lifts the hold
  manager's durable ephemera (the `holds` Map + `rearmById`/`rearmMeta`/`rearmChains`)
  into Effect-owned state (a Store/`Ref`-backed hold service). The Q1-BINDING
  doctrine is explicitly P10-scoped ("do not deepen ownership **in P10**"). Once
  the Maps the sweep reaps are themselves Effect-owned, `expireOrphans` no longer
  crosses an imperative↔Effect boundary and the P5-family port becomes cost-free
  and consistency-positive. This is the natural home for the conversion.
- **T2 (secondary — necessity changes).** If the sweep ever stops being *redundant*
  with the per-hold `holdMs` timers — i.e. if it becomes load-bearing for timely
  expiry rather than restart hygiene (the design §2D redundancy proof no longer
  holds) — it must move under supervision immediately, independent of T1, because
  an unsupervised load-bearing reaper violates the exit-gate ("no hold timer left
  unaccounted").

Until a trigger fires, the P5 ledger note ("stays on the P1 handle until P10")
graduates to "stays on the P1 handle, DEFERRED past P10 — see this record."

## §4 — Characterization floor (the pins this slice ships)

Slice-0 discipline applied to the DEFER: freeze the sweep's observable contract as
an oracle BEFORE any future port. `tests/p10-slice5-orphan-sweep-characterization.test.ts`
(3 tests) pins the three properties design §2D names as the ones a conversion MUST
preserve — each otherwise UNPINNED as a direct assertion at HEAD:

| §2D property | Pin | Source anchor | Mutation-proof |
|---|---|---|---|
| live-socket skip | a parked LIVE hold is never expired / never `{}`-written by the sweep; it settles `200 {}` only at `close()`/releaseAll | `questions.ts:1312` | `holds.has(r.id)` → `false` ⇒ pin fails (`expired` vs `pending`); reverted |
| redundant cleanup | an orphaned hold-kind row (no live socket) IS expired, `onRetired`(row) + `onChange` fire | `questions.ts:1317-1322` | direct (obvious); not mutated |
| audit-silence | a throwing tick is swallowed; the durable expiry still commits and the interval survives to expire a LATER orphan | `questions.ts:1434-1438` | remove `catch {}` ⇒ uncaught `setInterval` throw crashes the sweep; pin fails; reverted |

The eventual port's acceptance criteria (design §3 Slice 5 review) are exactly
these pins plus "Clock-driven cadence == SWEEP_MS" and "scope teardown clears the
fiber" — this file is their byte/behavior floor.

The `close()`-cancels-the-sweep teardown and the recycle-aged-rearm path are
already pinned elsewhere (`p1-question-retention-lifecycle.test.ts:80`;
`question-rearm.test.ts`), so this file does not duplicate them.

## §5 — Rollback / seam

No source change. Blast radius: tests + docs only. Rollback: n/a. The
`sweepMs` option (`questions.ts:162`, `1430-1431`) remains the only injection
seam; the future port's rollback (P1 `setInterval` behind a flag) is named by
design §3 Slice 5 and is not created here.

## §6 — Verification (this slice)

`bun run typecheck` both legs = 0; `bun run ci` (biome, 447 files) clean.
Deliverable 3×3 green. Regression sweep (1× each, 0 fail): board-hold-presence,
p1-question-retention-lifecycle, question-rearm (9), p10-slice0-hold-manager (3),
p10-slice0-lockstep-source (2), p10-slice2-watch-legs (4), p10-slice3-hook-hold-legs
(9), p10-slice4-shutdown-matrix (1), effect/daemon-app (4), effect/ingress-supervisor
(13), effect/sqlite-stmt-cache-trial (15), import-boundaries (11). No bundle regen
(source unchanged).
