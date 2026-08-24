# P9.4 DESIGN — Converting `mail.ts` pane-delivery async legs to Effects

> **Stamped into the repo 2026-08-24.** Copied verbatim from the adjudicated draft `/tmp/fd-effect/p9-4-design-draft.md`; §6 carries the orchestrator's rulings. Slices 1+2 (the pane-delivery workflow) landed as `cd65f374`; the deferred POST /mail core (Q1/Slice 3) moved to P9.5.

**Scope note.** Design/adjudication document. READ-ONLY output; no repo files were
written. Every claim carries a `file:line` anchor against the worktree
`tmp/p9-4-design` @ HEAD `03ee63f2`. After adjudication it becomes the P9.4
execution spec. It follows the P9.1 template skeleton
(`docs/v1/evidence/effect/p9-1-design.md`) verbatim: §1 lettered async-leg
inventory, §2 target shapes, §3 slice plan, §4 danger notes, §5 open questions.

**Two framing facts that shape everything below.**

1. **`mail.ts` is DOMAIN, and the pane-delivery workflow is *timer-driven*, not
   request-driven.** Unlike every P9.1 core (spawnKill/revive/adopt/enableRemote,
   discharged at the HTTP ingress by `runControlDetached`) and unlike P9.3 files
   (GET `/api/fs`, also HTTP-ingress), the pane-delivery workflow
   `tryOwnedPaneDelivery` has **no HTTP caller and no root-schedule caller**. It is
   fired by a P1 unref'd grace timer armed from `mail()` inserts (`mail.ts:307` →
   `armPaneMailTimer` `:315` → `setTimeout` → `tryOwnedPaneDelivery` `:320`) and is
   owned by mail's **own** P1 latch (`inFlight`/`own()`/`phase`, `mail.ts:230-243`),
   **not** the ingress supervisor. This is the central §4 danger and the crux of
   the whole design: P9.4 reuses the *runner* `runControlDetached` but must keep the
   *join* domain-local.

2. **Class = HTTP-CAPABILITY, per the completion map** (`p9-completion-map.md`
   mail row; `db-workflows/retention.ts:10-15`). The request path is not the root
   fiber: capabilities stay params, `R = never`, `E = never` (expected outcomes are
   DATA — here a `boolean`, not typed errors), and mail workflows **do NOT
   `yield* Store`**. SQLite (`q.*` + the `claimAllMail` `db.exec('BEGIN IMMEDIATE')`
   txn), tmux (`findScopedWindow`, `paneCurrentCommand`, `pasteText`, `sendEnter`)
   stay `ctx` closures/capability params.

**Scope boundaries (ruled by `p9-completion-map.md`, mail row + §4 table).** GET
`/mail` (`drainMail`) and GET `/api/watch` (`claimMail`/watch claim) stay **P10**
(holds). POST `/mail/ack` (`ackMail`) is **P9.5**. The lease txn stays **sync**
(`p9-completion-map.md` §2 item 5). P9.4 covers the **pane-delivery workflow
only** — `tryOwnedPaneDelivery` → `tryOwnedPaneDeliveryImpl` and its
`claimAllMail` batch-lease txn. Whether the POST `/mail` request-path core
(`postMailImpl`) and its route-label probe (`ownedPaneDeliverable`) come in P9.4 or
P9.5 is **Open Question Q1** (recommendation: P9.5).

---

## §1 — THE ASYNC-LEG INVENTORY

Every Promise-crossing path in mail.ts's delivery story, with entry point, trigger,
await points, the sync-SQLite interleave, failure dialect, and atomicity
constraints. **Awaiting legs: 1A, 1C, 1D. Sync-txn hazard: 1B. Support machinery:
1E, 1F.**

### 1A. `tryOwnedPaneDeliveryImpl(sid)` / `tryOwnedPaneDelivery(sid)` — THE pane-delivery workflow *(primary P9.4 target)*
- **Entry:** impl `mail.ts:541`; dispatcher wrapper `mail.ts:640`. Exported on the
  core surface (`mail.ts:858`; re-exported `derive.ts:1157`, `:1364`).
- **Trigger (the caller answer):** the P1 unref'd grace timer only —
  `armPaneMailTimer` (`mail.ts:315-326`) does `void tryOwnedPaneDelivery(sid).catch(…)`
  (`mail.ts:320`), armed by `mail()` on any insert (`mail.ts:307`) and re-armed by
  the workflow itself when a batch leaves a tail (`rearmPaneMailTimer`, `mail.ts:569`).
  **No HTTP route, no root schedule invokes it.** Tests call it directly and await the
  boolean (`p1-mail-lifecycle.test.ts:157,229,283`; `daemon-maintenance.test.ts:678,698,705`;
  `mail-and-blocking.test.ts:554,721,748`).
- **Await points (4), in legacy order:** `findScopedWindow(pair.sp.tmux_window)`
  (`mail.ts:544`) → `tmuxAdapter.paneCurrentCommand(target)` (`:549`) →
  `tmuxAdapter.pasteText(target, text)` (`:571`) → `tmuxAdapter.sendEnter(target)` (`:595`).
- **Sync-SQLite interleaved between/around the awaits:** eligibility gate
  `ownedPaneRow(sid)` (`:542`, re-read at `:563` post-probe TOCTOU and at `:589`
  last-mile before Enter), `hasWatchWaiter(sid)` watcher-priority (`:543`, re-check `:555`),
  the **`claimAllMail(sid)` BEGIN IMMEDIATE batch-lease txn** (`:564` — leg 1B),
  `releaseClaim` on paste-fail (`:579`), `ackMail` finalize on the pane-flipped/
  enter-failed/enter-confirmed arms (`:591`, `:611`, `:625`), `tick`/`logEvent`/`onMutate`.
- **Cooperative cancellation:** `if (!isOpen()) return false` after every
  non-cancellable await (`:545`, `:550`, `:576`, `:599`). This is mail's OWN
  cancellation — the lease is the durable recovery boundary; a post-quiesce
  continuation must NOT release/ack/log against a DB the root is closing
  (`mail.ts:572-576`; pinned by `p1-mail-lifecycle.test.ts:211,266`).
- **Failure dialect:** returns `Promise<boolean>` — DATA, never a typed error.
  Every branch's boolean is contractual: watcher-priority `false` (`:543,:555`),
  UNKNOWN-window `false` (`:546`), TOCTOU-bail `false` (`:563`), empty-batch `false`
  (`:565`), paste-fail `false` (`:581`), pane-flipped-after-paste `true` (`:593`),
  enter-fail `false` (`:619`), enter-confirmed `true` (`:637`), any `!isOpen()` gate
  `false`. A genuine throw (a `claimAllMail`/tmux-adapter throw) rejects the Promise;
  the timer's `.catch` (`mail.ts:320`) swallows it (fail-open, mail stays pending).
- **Atomicity constraints:** the `claimAllMail` txn (1B) must run in one JS turn with
  no suspension between the post-probe TOCTOU re-read (`:563`) and the paste (`:571`);
  BUG-033/BUG-034 lease-finalize discipline (paste = the side effect; never re-paste).

### 1B. `claimAllMail(sid)` — the BEGIN IMMEDIATE batch-lease txn *(sync; the txn-after-await hazard)*
- **Site:** `mail.ts:511-539`. `db.exec('BEGIN IMMEDIATE')` (`:512`) → bounded batch
  build from `q.pendingMailPage` (`:515`) → `q.claimMail.run(deadline, m.id)` per row
  (`:528`) → `db.exec('COMMIT')` (`:529`); `catch` → `db.exec('ROLLBACK')` + rethrow
  (`:531-538`).
- **The hazard the completion map flags** (`p9-completion-map.md` §2 item 5:
  "`claimAllMail` BEGIN IMMEDIATE … Lease batch **after** async tmux probes … P9.4.
  No yield in txn"). At this HEAD the map's `:490-498` probe anchor points at 1C's
  probes; the *actual* caller is 1A, whose two awaited tmux probes (`:544`, `:549`)
  precede the `claimAllMail` call at `:564`. **The txn is sync and has NO awaits
  inside** — it is safe *only* while it stays inside 1A's single native continuation.
- **Failure dialect:** ROLLBACK + rethrow (`:531-538`) → propagates to 1A's caller as
  a rejection.

### 1C. `ownedPaneDeliverable(sid, {probe})` — read-only route-label tmux probe
- **Site:** `mail.ts:486-498`. Awaits `findScopedWindow` (`:493`) + `paneCurrentCommand`
  (`:496`). Cheap mode (`probe:false`) short-circuits with no await (`:492`).
- **Caller:** `postMailImpl` route-reporting ONLY — `await ownedPaneDeliverable(sid)`
  inside `Promise.all` (`mail.ts:763`). It mutates nothing (no claim, no paste). NOT
  reached by snapshot (snapshot uses the SYNC `ownedPaneRow`, `snapshot.ts:117`).
- **Failure dialect:** returns `Promise<boolean>`; a throw rejects the enclosing
  `Promise.all` in 1D.
- **Scope:** part of the POST `/mail` request path (1D), not the timer workflow — see Q1.

### 1D. `postMailImpl(args)` / `postMail(args)` — POST `/mail` request path
- **Entry:** impl `mail.ts:689`; wrapper `mail.ts:813` (`return own(postMailImpl(args))`,
  `:815`). Dispatched at the HTTP transport as an already-P6.4-bridged Effect route
  (`http.ts:1420` `effectRoutes.mail({ postMail: recorder.invoke })`; `http.ts:2324`).
- **Await points (1):** `await Promise.all(targets.map(async sid => … ownedPaneDeliverable …))`
  (`mail.ts:760-766`) — the route-label probes (leg 1C). After the probes,
  `if (!isOpen()) return quiescingPostMailResult()` (`:769`), then the SYNC
  `mail()` inserts (`:774`, which arm the delivery timer at `:307`).
- **Trigger:** HTTP POST `/mail` (external board/orchestrator). This is the *ingress
  that indirectly triggers 1A* (via `mail()` → `armPaneMailTimer`) — but it does not
  call 1A itself.
- **Failure dialect:** returns a `{status, body}` control wire (200 receipt / 422 / 409 /
  429 / 503-quiescing); no throws for expected outcomes.
- **Class note:** the completion map marks POST `/mail` "HTTP-CAPABILITY DONE" — that
  is the *transport* bridge (P6.4). The *core* `postMailImpl` is still a native
  Promise. Its conversion is a request-path concern; recommend **P9.5** (Q1).

### 1E. Mail P1 ownership — `own()` / `inFlight` / `phase` / `quiesce` / `close`
- **State/ops:** `inFlight: Set<Promise>` (`mail.ts:230`), `phase` (`:231`),
  `own<T>(promise)` (`mail.ts:236-243`), `isOpen()` (`:234`), `quiesce()`
  (`:818-824`, clears every grace timer), `closeImpl` (`:826-834`, while-loop
  `Promise.allSettled([...inFlight])`), `close` (`:836-844`, memoized).
- **Role:** the admission latch + join for BOTH `postMail` and `tryOwnedPaneDelivery`.
  This is mail's analog of `spawnMaintenance` — but domain-local and NOT the ingress
  supervisor. **The pane-delivery discharge must register its Promise here** (D3).
  Pinned by `p1-mail-lifecycle.test.ts` (close joins in-flight paste `:211`, joins
  in-flight Enter and suppresses late ack `:266`, lease survives shutdown as recovery
  boundary `:234-257`).

### 1F. The grace-timer machinery — `armPaneMailTimer` / `rearmPaneMailTimer` / `paneMailTimers`
- **Site:** `paneMailTimers: Map<string, Timeout>` (`mail.ts:314`); `armPaneMailTimer`
  (`:315-326`, one coalesced unref'd timer per session, BUG-128); `rearmPaneMailTimer`
  (`:329-333`, drops the running handle and schedules the next round).
- **Role:** the sole TRIGGER for 1A. Stays a **native timer adapter** (not
  orchestration) — the P9.4 analog of P9.1's unref'd harvest timers, which the P9.1
  design left as timer adapters, not Effects.

---

## §2 — THE TARGET SHAPE

**Uniform target for the pane-delivery workflow (1A):** an
`Effect<boolean, never, never>`, discharged by the injected context-free runner and
joined by mail's own latch. Expected outcomes stay a `boolean` (DATA); a genuine
throw becomes a **die** → the discharged Promise rejects → the timer's `.catch`
swallows it (fail-open), byte-identical to today.

**Family pattern that applies:** the **P9.1 landed ControlStep pattern**, spelled
file-local. `mail.ts` is DOMAIN and MUST NOT relative-import the app zone, so — exactly
as `spawns.ts` spells its own `SpawnsWire`/`ControlStep<A>`/`dischargeStep`
(`spawns.ts:93,125,133`) and `retention.ts` spells its own `DismissWire`/`DismissStep`
(`retention.ts:83,102`) — mail spells a file-local trio. Since the wire here is a bare
`boolean`, the wire type collapses:

```
// file-local, mirrors spawns.ts:125-134 / retention.ts:102-134
type PaneDeliveryStep =
  | { readonly done: true;  readonly wire: boolean }
  | { readonly done: false; readonly run: () => Promise<boolean> };

const dischargeStep = (step: PaneDeliveryStep): Effect.Effect<boolean, never, never> =>
  step.done ? Effect.succeed(step.wire) : Effect.promise(step.run);
```

**Boundary placement (the mechanical target).** `tryOwnedPaneDeliveryImpl` splits into
a synchronous prefix that builds a `PaneDeliveryStep` and one coarse `run` thunk that
carries EVERY remaining await *and* the `claimAllMail` txn:

```
function paneDeliveryStep(sid): PaneDeliveryStep {
  const pair = ownedPaneRow(sid);                 // sync gate — mail.ts:542
  if (!pair || hasWatchWaiter(sid)) return { done: true, wire: false };  // :543
  const runDeliver = async (): Promise<boolean> => {
    // EVERYTHING from findScopedWindow (:544) onward, byte-for-byte:
    //   probes (:544,:549) → isOpen()/win/pane gates (:545..:551)
    //   → watcher re-check (:555) → TOCTOU re-read ownedPaneRow (:563)
    //   → claimAllMail BEGIN IMMEDIATE txn (:564)   ← leg 1B, stays INSIDE run
    //   → rearmPaneMailTimer on tail (:569) → pasteText (:571)
    //   → isOpen()/releaseClaim (:576..:582) → last-mile ownedPaneRow (:589)
    //   → sendEnter (:595) → isOpen()/ackMail finalize/tick/logEvent (:599..:637)
  };
  return { done: false, run: runDeliver };
}

function tryOwnedPaneDeliveryEffect(sid): Effect.Effect<boolean, never, never> {
  return Effect.sync(() => paneDeliveryStep(sid)).pipe(Effect.flatMap(dischargeStep));
}
```

The dispatcher keeps the P1 latch and the rollback seam, mirroring
`spawns.ts:2394-2397` / `retention.ts:1087-1090`:

```
function tryOwnedPaneDelivery(sid): Promise<boolean> {
  if (!isOpen()) return Promise.resolve(false);                     // mail.ts:641
  return EFFECT_CORE_PANE_DELIVERY && runControlDetached
    ? own(runControlDetached(tryOwnedPaneDeliveryEffect(sid)))      // Effect core
    : own(tryOwnedPaneDeliveryImpl(sid));                           // legacy rollback
}
```

- **`EFFECT_CORE_PANE_DELIVERY = true`** — the rollback seam constant, mirroring
  `EFFECT_CORE_SPAWN_KILL` etc. (`spawns.ts:139`). `false` OR no injected runner →
  legacy `tryOwnedPaneDeliveryImpl` answers directly.
- **`runControlDetached` is REUSED, not rebuilt.** It is context-free
  (`Effect.runPromiseWith(Context.empty())`, `ingress-supervisor-live.ts:52`) and
  explicitly "NOT tracked by the supervisor registry" (`:44-53`) — so it is safe to
  call from a timer. Thread it onto `MailCtx` as
  `runControlDetached?: RunControlDetached` (optional, `import { type RunControlDetached }
  from './retention.ts'` as spawns does at `spawns.ts:33`); it is already on the shared
  ctx object (sourced `program.ts:794` `ingress.runControlDetached`, destructured
  `derive.ts:312`, placed on ctx `derive.ts:1124`), so extending `MailCtx` (`mail.ts:164-184`)
  and destructuring in `createMail` (`mail.ts:203-222`) is a one-line-each addition.
  Standalone mail-factory tests omit it → legacy fallback (as spawns/retention do).

**The discharge join stays `own()`, NOT the ingress supervisor** (D3). The wrapper is
`own(runControlDetached(effect))` — do NOT route through `ingress.runPromise(...)`,
which would (a) double-track the fiber under the supervisor registry and (b) be
*refused* once ingress quiesces (`ingress-supervisor-live.ts:229-237`), whereas mail
must remain deliverable on its own lifecycle. `runControlDetached` executes
`Effect.sync → flatMap → Effect.promise` synchronously up to `run`'s first await
(the P9.1 timing guarantee, `spawns.ts:104-108`), so `own(runControlDetached(...))`
registers the Promise before any await yields — preserving mail.ts:642-645's
"impl before `own()`" invariant (D4).

**Sync stays sync inside the Effect.** The `claimAllMail` BEGIN IMMEDIATE txn
(`mail.ts:512-529`) runs inside `runDeliver` between the `paneCurrentCommand` await and
the `pasteText` await — one JS turn, no Effect `yield*` can split it. It is NEVER
lifted into its own Effect step (D1). The sync prefix (`ownedPaneRow` + `hasWatchWaiter`
gate) is the only thing in `Effect.sync`; the `isOpen()` gates stay native `if` inside
`run` (D2).

**Where typed errors go:** nowhere new. `E` stays `never`; a `run` rejection is a die
(`dischargeStep`'s `Effect.promise`, `spawns.ts:130-134`). No error channel widens.

**What stays a named native adapter (per the P9 exit text):**
- **The grace-timer machinery (1F)** — `armPaneMailTimer`/`paneMailTimers` stay timer
  adapters; not orchestration.
- **The tmux leaves** — `findScopedWindow`, `paneCurrentCommand`, `pasteText`,
  `sendEnter` stay `ctx` capability params, awaited inside the coarse `run`.
- **The `claimAllMail` sync txn** — one coarse `Effect`-external sync function inside `run`.
- **`runControlDetached` at the discharge** — the one inventoried runner bridge.
- **`own()` / `inFlight` P1 ownership (1E)** — the domain-local join bridge.

---

## §3 — SUB-SLICE PLAN

Ordered, commit-sized, characterization-first. **Rule** (from P9.1): no path is
converted until the CHARACTERIZATION GAPS below it are closed by a *new* test that
pins the exact bytes/timing of the legacy path FIRST. Each slice lands gate-green,
rollback = the legacy body + the `EFFECT_CORE_PANE_DELIVERY` flag. mail is a smaller
module than spawns: **two in-scope slices + one deferred.**

### Slice 1 — Characterization backfill (pure tests, no path moved)
- **Why first:** today's delivery suites (`mail-delivery-lease.test.ts` [5 pass],
  `p1-mail-lifecycle.test.ts` [4 pass], `mail-and-blocking.test.ts`,
  `daemon-maintenance.test.ts`) all call `tryOwnedPaneDelivery` with **no injected
  runner** → they exercise the LEGACY body. The Effect core would ship unexercised.
- **Add (gaps):**
  1. **Runner-injected byte-parity harness** — a mail delivery test that passes
     `runControlDetached` into the factory (mirrors the P9.1 Slice-5 follow-up where
     "fleet-bugs' memoryCore injects the runner so the pin exercises the Effect core",
     `p9-1-design.md` §8). Re-run the existing delivery assertions through the Effect
     path: success `true`, watcher-priority `false`, TOCTOU-bail `false`, paste-fail
     releaseClaim + `false`, enter-fail ack-finalize + no-requeue + `false`
     (`daemon-maintenance.test.ts:662,711` are the legacy pins to mirror).
  2. **`claimAllMail` ROLLBACK pin** — force `q.claimMail.run` to throw mid-batch and
     assert `db.exec('ROLLBACK')` runs and the rejection propagates (legacy: rejects;
     Effect: die → rejected Promise → timer `.catch`). No test hits `:531-538` today.
  3. **`isOpen()` mid-`run` gate pin under the Effect path** — that a quiesce landing
     between the probe and the paste leaves the lease intact and fires no mutation
     (the Effect analog of `p1-mail-lifecycle.test.ts:211,266`, which today run legacy).
- **Blast radius:** tests only. **Rollback:** revert tests.
- **Adversarial review:** no (characterization only).

### Slice 2 — `tryOwnedPaneDelivery` to Effect core *(the conversion)*
- **Convert:** introduce the file-local `PaneDeliveryStep` + `dischargeStep` +
  `EFFECT_CORE_PANE_DELIVERY`; extract `paneDeliveryStep(sid)` (sync prefix) +
  `runDeliver` (coarse tail carrying all 4 awaits AND the `claimAllMail` txn);
  add `tryOwnedPaneDeliveryEffect`; rewrite the `tryOwnedPaneDelivery` dispatcher to
  `own(runControlDetached(tryOwnedPaneDeliveryEffect(sid)))` with the legacy
  `tryOwnedPaneDeliveryImpl` retained UNCHANGED as the rollback seam. Add
  `runControlDetached?: RunControlDetached` to `MailCtx` and destructure it; wire it in
  `derive.ts` `createMail(ctx)` (ctx already carries it).
- **Danger notes binding:** D1 (txn stays inside `run`), D2 (`isOpen()` stays native),
  D3 (join = `own()`, not ingress), D4 (sync prefix runs on the arming turn),
  D5 (boolean parity), D6 (fail-open), D7 (rearm inside `run`).
- **Characterization:** Slice 1's runner-injected harness + the full existing delivery
  suites (`mail-delivery-lease`, `p1-mail-lifecycle`, `mail-and-blocking`,
  `daemon-maintenance`, `pane-paste-sanitize`, `tmux-adapter`).
- **Blast radius:** the pane-delivery workflow only; `mail.ts`. **Rollback:**
  `EFFECT_CORE_PANE_DELIVERY = false` → legacy `tryOwnedPaneDeliveryImpl`.
- **Adversarial review:** **YES** (paste + claim txn + TOCTOU + lease finalize).

### Slice 3 (optional / recommend defer to P9.5) — `postMailImpl` (1D) + `ownedPaneDeliverable` (1C)
- **Convert:** `postMailImpl` becomes a control core returning a `{status, body}` wire
  (its own file-local `MailWire`/step), with the `Promise.all` route-label probes as
  the coarse `run`; `ownedPaneDeliverable`'s two tmux probes become the shared probe
  leaf.
- **Why defer:** this is the POST `/mail` *request path*, not the pane-delivery
  workflow the task scopes P9.4 to. It belongs with P9.5's leftover request-path
  cores (structurally a `{status,body}` control core, discharged at the transport by
  `runControlDetached` like every other request path). See Q1.
- **Adversarial review:** **YES** if pulled into P9.4.

---

## §4 — DANGER NOTES (byte-frozen requirements)

**D1 — The `claimAllMail` BEGIN IMMEDIATE txn stays inside the single coarse `run`
thunk; NO Effect suspension mid-txn.** `mail.ts:512-529` runs synchronously between the
`paneCurrentCommand` await (`:549`) and the `pasteText` await (`:571`), *inside* the TOCTOU
re-read (`:563`) that must be on the same JS turn as the claim. The whole point of the
ControlStep pattern is that `run` is ONE `Effect.promise` — a tempting `Effect.gen`
multi-`yield*` decomposition (one yield per await) would either split the txn from its
guarding TOCTOU re-read or insert a suspension point that re-opens the exact race the
completion map item 5 and the BUG-8 comments (`mail.ts:556-563`) forbid. **Byte-frozen:
`claimAllMail` is called from native code inside `run`, never wrapped in its own
`Effect.sync`/`yield*`.** (Analog of P9.1 D2.)

**D2 — The `isOpen()` post-await gates stay native `if` inside `run` — do NOT convert to
Effect interruption.** `mail.ts:545,550,576,599`. They are mail's cooperative
cancellation, and the lease is the durable recovery boundary: a post-quiesce
continuation must not release/ack/log against a closing DB (`mail.ts:572-576`). The
discharge is UNTRACKED (`runControlDetached`, no supervisor fiber to interrupt), so
there is no `Fiber.interrupt` path to convert them to, and `p1-mail-lifecycle.test.ts:211,266`
pins the exact non-cancellable paste/Enter + lease-survives semantics. (Analog of P9.1
D3/D4 — interruption conversion is deferred out of P9.4; see Q3.)

**D3 — The discharge join is mail's `own()`, NOT the ingress supervisor.** The returned
Promise that `inFlight` tracks (`mail.ts:230,236-243`) must be the object `own()`
registers, or `close()` (`mail.ts:826-834`) stops joining the in-flight paste/Enter —
a shutdown-correctness regression pinned by `p1-mail-lifecycle.test.ts:211,266`. Wrap as
`own(runControlDetached(effect))`; do NOT call `ingress.runPromise(...)` (it double-tracks
under the supervisor registry and is *refused* after ingress quiesce,
`ingress-supervisor-live.ts:229-237`). (Inverted analog of P9.1 D7: the tracked
identity is domain-local.)

**D4 — The synchronous prefix must still run on the arming turn.** `runControlDetached`
= `runPromiseWith` executes `Effect.sync → flatMap → Effect.promise` synchronously up to
`run`'s first await (`spawns.ts:104-108`), so `own(runControlDetached(...))` registers the
Promise before any await yields, preserving `mail.ts:642-645`'s "call the implementation
before `own()`" invariant. **Byte-frozen:** the eligibility prefix (`ownedPaneRow` +
`hasWatchWaiter` gate, `mail.ts:542-543`) stays in the `Effect.sync` region, not inside
`run`.

**D5 — The delivery `boolean` is contractual, per-branch.** Tests await the boolean
directly (`daemon-maintenance.test.ts:678,698,705`; `mail-and-blocking.test.ts:554`;
`p1-mail-lifecycle.test.ts:157`). Every branch's value (§1A) must be byte-identical.
A `claimAllMail`/adapter throw rejects the Promise both ways (legacy throw; Effect die
→ `runControlDetached` rethrows `causeSquash`) — same observable for a direct test caller;
same swallow at the timer.

**D6 — Fail-open on delivery is preserved.** `armPaneMailTimer`'s
`void tryOwnedPaneDelivery(sid).catch(() => {})` (`mail.ts:320`) swallows all rejections;
an Effect die surfaces there as a rejected Promise, unchanged. Mail stays pending. No new
error surface, no new log line.

**D7 — `rearmPaneMailTimer` fires inside `run` when a batch leaves a tail.**
`mail.ts:569` (guarded by `remaining` from `claimAllMail`). It must stay a sync side
effect inside the coarse `run` (between the claim and the paste), not hoisted, to
preserve BUG-128 one-probe-per-batch coalescing (`mail-and-blocking.test.ts:586,715`).

**D8 — `postMail` (1D) and `ownedPaneDeliverable` (1C) are OUT of the P9.4 conversion
under the recommended scope.** If Slice 3 is deferred (Q1), leave them byte-untouched;
do not "opportunistically" wrap them — that would drag the POST `/mail` request path into
a pane-delivery slice and cross the P9.4/P9.5 boundary.

---

## §5 — OPEN QUESTIONS (need the orchestrator's ruling)

**Q1 — Is the POST `/mail` request-path core (`postMailImpl`, 1D) + its route-label
probe (`ownedPaneDeliverable`, 1C) in P9.4, or P9.5?**
*Recommendation: **P9.5.*** The task scopes P9.4 to "the pane-delivery workflow only,"
and the completion map's P9.4 cell names only "pane delivery (`claimAllMail` after
probes)." `postMailImpl` is the HTTP request path — its async is route-labeling and it
returns a `{status,body}` control wire, structurally identical to the P9.5 leftover
request-path cores, dischargeable at the transport by `runControlDetached` like every
other request path. Keep P9.4 to Slices 1-2 (the timer workflow); convert 1C/1D with the
P9.5 family, sharing the `findScopedWindow`/`paneCurrentCommand` probe leaves.

**Q2 — Discharge runner: reuse the ingress `runControlDetached`, or mint a mail-local
`Context.empty()` runner?**
*Recommendation: **reuse the injected `runControlDetached`**, wrapped in `own()`.* It is
context-free and NOT supervisor-tracked (`ingress-supervisor-live.ts:44-53`), so it is
safe from a timer; a second `Context.empty()` runner would need a fresh `Effect.run*With`
call, which import-boundaries forbid outside the platform file (`ingress-supervisor-live.ts:48-51`;
bare `Effect.runPromise` is deny-listed). Do NOT route through `ingress.runPromise`
(D3). This is the same runner P9.3 files uses for its HTTP cores — P9.4 differs only in
the *join* (mail's `own()`, not the transport recorder).

**Q3 — Do the `isOpen()` cooperative-cancellation gates become Effect interruption in
P9.4?**
*Recommendation: **NO** (defer, mirrors P9.1's AbortController deferral).* The delivery
Effect is discharged untracked (no supervisor fiber to interrupt), and
`p1-mail-lifecycle.test.ts` pins the exact non-cancellable paste/Enter + lease-recovery
semantics. Keep `isOpen()` native inside `run` (D2). Interruption is a later
work-package if ever taken — like P9.1 Slice 7.

**Q4 — Adversarial-review scope.**
*Recommendation:* Slice 2 (paste + claim txn + TOCTOU + lease finalize) is
review-mandatory; Slice 1 (characterization-only) may be orchestrator line-review;
Slice 3, if pulled in, is review-mandatory.

**Q5 — Flag granularity + the standalone-factory-keeps-legacy contract.**
*Recommendation:* a single `EFFECT_CORE_PANE_DELIVERY` flag (mirrors `EFFECT_CORE_*`,
`spawns.ts:139-164`). Standalone mail-factory tests that omit `runControlDetached` keep
the legacy `tryOwnedPaneDeliveryImpl` fallback exactly as spawns/retention do — and P9.4
MUST land Slice 1's runner-injected test so the Effect core is actually exercised in CI
(without it the flag is `true` in production but every mail suite still runs the legacy
body).

---

**Inventory of the compatibility bridges P9.4 leaves standing** (for the exit-gate
ledger): (1) `runControlDetached` at the pane-delivery discharge (`ingress-supervisor-live.ts:52`,
threaded via `MailCtx`); (2) mail's `own()`/`inFlight` P1 ownership join (`mail.ts:230-243`);
(3) the unref'd grace-timer machinery `armPaneMailTimer`/`paneMailTimers` (`mail.ts:314-333`);
(4) the native tmux leaf adapters (`findScopedWindow`, `paneCurrentCommand`, `pasteText`,
`sendEnter`) + the `claimAllMail` sync BEGIN IMMEDIATE txn (`mail.ts:511-539`). All four
are named; none is a `yield* Store` violation because the pane-delivery workflow stays
`R = never`.

---

## §6 — ORCHESTRATOR ADJUDICATION (Fable, 2026-08-24)

**Q1 ACCEPTED:** P9.4 = the delivery workflow only; postMailImpl's probe fan-out + ownedPaneDeliverable go to P9.5.

**Q2 ACCEPTED:** reuse the ctx-resident ingress `runControlDetached` (context-free, untracked); the tracked join stays mail's `own()` — `own(runControlDetached(effect))` — NEVER `ingress.runPromise` (D3 is binding: double-tracking + post-quiesce refusal would break mail's independent lifecycle).

**Q3 ACCEPTED:** `isOpen()` gates stay imperative; no interruption conversion (untracked discharge has no fiber to interrupt; holds are P10).

**Q4 ACCEPTED:** Slice 2 adversarial-review-mandatory; Slice 1 (characterization backfill) = orchestrator line review.

**Q5 ACCEPTED and BINDING:** single `EFFECT_CORE_PANE_DELIVERY` flag; Slice 1's runner-injected byte-parity harness is a PRECONDITION of Slice 2 — the Effect core must not ship CI-unexercised (the P9.1 §8-note-4 residual is not to be repeated here).

**D1 endorsed as byte-frozen:** the `claimAllMail` BEGIN IMMEDIATE txn and its post-probe TOCTOU re-read stay inside ONE coarse run thunk — no Effect.gen decomposition, no yield between probe re-read and lease.

**Sequencing:** slices 1+2 may be implemented in the tmp/p9-4-design worktree in parallel with P9.2/P9.3 integration (mail.ts overlaps neither); integration after P9.2's first slices land.
