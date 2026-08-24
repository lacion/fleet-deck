# P9.5 Design — Remaining HTTP-triggered application services (the leftover routes)

> **Stamped into the repo 2026-08-24.** Copied verbatim from the adjudicated draft `/tmp/fd-effect/p9-5-design-draft.md`; §6 carries the orchestrator's rulings. Slices 1+2 (GET /api/settings, POST /mail/ack) landed as `ae6c36fd`; Slice 3 (POST /mail core) as `b3666ca8`.

> DESIGN/INVENTORY draft. No source changed. Worktree `/tmp/fd-wt-p9-3-design`
> @ HEAD `0a18335c` (branch `tmp/p9-3-design`, = P9.3 slice 3 on top of the
> landed P9.1/P9.2/P9.4 stack). All file:line refs are against that tree.
> Sibling reading: `p9-1-design.md` (template + sync-route arm), `p9-2-design.md`
> (settler conventions, two 500 dialects, §6 OQ-1 degenerate core / OQ-3
> parameterized settler), `p9-4-design-draft.md` (§1D/§5-Q1 postMail deferral +
> PaneDeliveryStep template), `p9-completion-map.md` (leftover-HTTP matrix).

---

## §1 — Authoritative residual inventory (the last legacy-only application services)

P9.1/P9.2/P9.3/P9.4 have already claimed almost every row of the completion
map's leftover-HTTP table. After an **exhaustive re-sweep of every GET/POST/WS
branch in `routeRequest` (src/daemon/http.ts:2226–3080)** — see the "verified
nothing else remains" audit at the end of this section — the true P9.5 residual
is **exactly three application-service items**, plus two documented boundary
items that are explicitly NOT P9.5.

### The three P9.5-owned residuals

**A) POST `/mail/ack` — sync legacy transport over a sync core leaf.**
- Route: `src/daemon/http.ts:2513–2517`
  ```
  if (url.pathname === '/mail/ack') {
    const out = core.ackMail([(ev as { mail_id?: unknown }).mail_id]);
    json(res, 200, { ok: true, ...out });
    return;
  }
  ```
- Core: `ackMail` `src/daemon/mail.ts:400–410` — **synchronous**, shared leaf
  (guarded `Number.isSafeInteger` loop → `q.ackMail.run(now, id)` guarded UPDATE,
  the lease-finalization half of BUG-034).
- Wire dialects:
  - success → `json(res, 200, { ok:true, ...out })` where `out = { acked:N }`,
    i.e. `200 {ok:true, acked:N}` (N derived from `.changes`; `mail_id` absent /
    non-integer / non-array all fold to `acked:0`).
  - defect → sits INSIDE the inner POST try (`http.ts:2453 → catch 3062`), so a
    throw renders **`500 {err:'internal'}` + log `'fleetd handler error:'`** =
    `CONTROL_DEFECT` (http.ts:1222). (Structurally near-unreachable: `ackMail` is
    sync and type-guards its inputs — see §5 OQ-1.)
- Callers of the leaf: this route (P9.5) **and** GET `/mail` at
  `http.ts:2336` (`core.ackMail(ackIds.split(',').map(Number))`) which is a
  **P10 hold**. → the *route/transport* is P9.5; the *ackMail leaf + lease
  protocol* is frozen P10-shared (see §4).

**B) GET `/api/settings` — sync legacy always-200 read over a sync core leaf.**
- Route: `src/daemon/http.ts:2284–2287`
  ```
  if (url.pathname === '/api/settings') {
    json(res, 200, { ok: true, settings: core.resolveSettings() });
    return;
  }
  ```
- Core: `resolveSettings` `src/daemon/settings.ts:788–801` — **synchronous**,
  returns a plain object (`repos_dir, repo_transport, repo_default_org,
  browse_root, fav_dirs, repo_setup, hold_ms, gateway`); `gateway` is MASKED by
  construction (settings.ts:797–799). Shared leaf.
- Wire dialects:
  - success → always `200 {ok:true, settings:{…}}`.
  - defect → sits in the GET block under the OUTER try only (`http.ts:2227 →
    catch 3077`): `json(res, (url startsWith '/hook/') ? 200 : 500, {})` →
    **`500 {}` + log `'fleetd request error:'`**. This is **byte-identical to
    GET `/state`'s dialect**, i.e. `settleEffectSnapshotRoute` (http.ts:947).
- Callers of the leaf: this route (P9.5); POST `/api/settings` success body
  (`settings.ts:713/739`, transport ALREADY Effect via `dispatchSettings` →
  `settleEffectMutatingRoute`, http.ts:1475–1487); and the `/state` broadcast
  (`resolveHoldMsSetting → resolveSettings`, settings.ts:393 comment). →
  convert the *read route*; freeze the `resolveSettings` leaf (see §4).

**C) POST `/mail` — the async *core* (`postMailImpl`) + its probe fan-out.
Transport is ALREADY Effect; only the core is legacy. (P9.4-deferred: §1D +
§5-Q1 of `p9-4-design-draft.md`, orchestrator-ACCEPTED.)**
- Transport: `dispatchMail` `src/daemon/http.ts:1503–1529` — DONE. Rides
  `settleEffectAsyncMutatingRoute` (http.ts:1259) with a `startOnce` recorder
  around `mailCapabilities(ev).postMail` and `MAIL_DEFECT`
  (`{log:'fleetd mail error:', body:{ok:false, err:'internal'}}`, http.ts:1203).
  `onFulfilled` normalizes: `json(target, rec.status ?? 200, rec.body ?? out)`.
- Bridge: `mailCapabilities` `http.ts:929–931` = `{ postMail: () =>
  core.postMail(ev) }`; `postMail` `mail.ts:977–980` = outer `isOpen()` pre-gate
  then `own(postMailImpl(args))`.
- Core (legacy async): `postMailImpl` `mail.ts:853–975`. Structure that P9.5
  must preserve:
  - **sync validation prefix** (mail.ts:861–918): 422 (empty sender / reserved /
    unsafe / reserved-frame text), 409 (name resolves only to shell panes). No
    await.
  - **the ONE async boundary** — probe fan-out: `const routes = await
    Promise.all(targets.map(async sid => watcher|pane|offline-queued|
    turn-boundary))` (mail.ts:924–930). `pane` requires `await
    ownedPaneDeliverable(sid)`.
  - **post-probe quiesce gate** (mail.ts:931–933): `if (!isOpen()) return
    quiescingPostMailResult()` — "route probes are not cancellable; quiesce
    turns their continuation into an explicit refusal before mail() performs any
    SQLite mutation." **Load-bearing ordering.**
  - **sync tail** (mail.ts:938–974): `mail()` inserts, `refusedAll` → 429, `tick
    + onMutate`, success returns a **bare `{ok:true, delivered, targets[…],
    truncated?…}`** object (NOT `{status,body}`); the error arms return
    `{status,body}`.
- Sub-leg: `ownedPaneDeliverable` `mail.ts:527–539` — async; `probe:true` path
  awaits `findScopedWindow` + `tmuxAdapter.paneCurrentCommand`; `probe:false`
  short-circuits (no await). **Shared leaf** — `/state` cheap-mode snapshot uses
  `probe:false`; the mail success path uses `probe:true`. (P9.4's deferred leg
  1C.)

### Two documented boundary items — NOT P9.5 slices (recorded for honesty)

**EXCLUDED — POST `/api/worktrees/remove` = P9.2 Slice 3.** `http.ts` references
`core.removeWorktree` at `2530` (a `Parameters<typeof core.removeWorktree>[0]`
type expression inside the remove handler). Per task scope this is P9.2's third
slice, not P9.5. Boundary call: P9.5 leaves it untouched; the census below does
NOT count it.

**EDGE — the PermissionRequest/AskUserQuestion telemetry fast-path
(`http.ts:2479–2486`).** Inside the already-Effect `/hook/` surface (P6.4), a
`PermissionRequest` whose `tool_name === 'AskUserQuestion'` fires a
fire-and-forget `core.applyEvent({…})` and answers a **constant `200 {}`**,
bypassing `dispatchHook`. Its HTTP response carries no core-computed body, so
there is nothing to preserve for wire-fidelity. Classification: part of the
settled P6.4 hook surface; recommend **freeze / OUT of P9.5** (see §5 OQ-2).

### "Verified nothing else remains" — the census

Every direct `core.*` call in the route-dispatch body (`http.ts` 2226–3080) was
enumerated (`awk … /core\./`) and classified:

| Location | core call | Status |
|---|---|---|
| 2285 | `resolveSettings()` | **P9.5-B (leftover)** |
| 2306–2323 | `fsList/fsRead/fsSearch/*Home` | P9.3 DONE (`settleFilesystemOperation`, http.ts:848) |
| 2336 | `ackMail(ackIds…)` | P10 hold (GET `/mail`); shares P9.5-A leaf |
| 2341 | `drainMail(…, {lease:true})` | P10 hold (GET `/mail`) |
| 2480 | `applyEvent(…)` | EDGE, P6.4 hook fast-path (§5 OQ-2) |
| 2514 | `ackMail([mail_id])` | **P9.5-A (leftover)** |
| 2530 | `removeWorktree` (type) | EXCLUDED = P9.2 slice 3 |
| 2612 | `armUnsupervised()` | Effect DONE (2601 primary; 2612 is the legacy rollback seam) |
| 2691 | `spawn(ev)` (2695 legacy) | Effect DONE (`settleEffectSpawnRoute`, 2687; 2695 rollback seam) |
| 2725/2733 | `spawnKill` | Effect DONE (control async) |
| 2760 | `revive` | Effect DONE (control async) |
| 2797/2808 | `adoptSession` | Effect DONE (control async) |
| 2863 | `applyCustomName` | Effect DONE (`nameControl`) |
| 2943 | `enableRemote` | Effect DONE (`controlSync`, rc) |
| 2965–2998 | `questions.answer/dismiss` | Effect DONE (`controlSync`/`questionsDismiss`) |
| 3013–3055 | `planMark/assignPlan` | Effect DONE (`controlSync`) |
| 3105 | `questions.setBoardConsumerProbe` | wiring/init, not a route |

The POST `/mail` core (`postMailImpl`) does not surface here because it is
reached through `dispatchMail → mailCapabilities → core.postMail`
(mail.ts:930) — it is item **C**. Dispatch-helper inventory confirms every other
route already owns an Effect settler: `dispatchHealth/State/Worktrees/
PasteImage/Settings/Command/Mail/Cleanup/Hook`, `settleEffect{Snapshot,SoftRead,
Mutating,AsyncMutating,Spawn,Hook}Route`, `settleControlAsyncRoute`,
`settleFilesystemOperation`, and the `effectRoutes.*` builders. **No fourth
legacy-only application service exists in `http.ts` at `0a18335c`.** GET `/mail`
(2329) and GET `/api/watch` (2346) remain P10 holds; `/ws/term` remains P7.

**Authoritative leftover count: 3** (A, B, C). Excluded: 1 (worktrees/remove).
Edge/frozen: 1 (hook telemetry fast-path).

---

## §2 — Target shapes under landed conventions

**Every P9.5 route maps onto an already-landed settler. NONE needs the P9.2 §6
OQ-3 parameterized settler** (both defect dialects — `500 {}` for the outer
catch and `500 {err:internal}` for the inner POST catch — are already realized
by existing settlers; the try-nesting decides which each route inherits).

### A) POST `/mail/ack` → `settleEffectMutatingRoute` + `CONTROL_DEFECT`

Sync + mutating. Direct analogue of **POST `/api/spawn/arm-unsupervised`**
(http.ts:2588–2613), which mints a token synchronously and rides
`settleEffectMutatingRoute(…, CONTROL_DEFECT)`.

- New workflow builder `effectRoutes.mailAck(caps)` returning
  `Effect<{status:200, body:{ok:true, acked:N}}, never, never>`; the workflow
  body is `const out = caps.ack(); return {status:200, body:{ok:true, ...out}}`.
- Capability bridge (transport side, http.ts): `mailAckCapabilities(ev) = {
  ack: () => core.ackMail([(ev as {mail_id?}).mail_id]) }` — the `mail_id`
  extraction and `[…]` wrapping stay verbatim at the transport (mirrors
  arm-unsupervised's `run: () => core.armUnsupervised()`). **`ackMail` stays a
  sync frozen leaf.**
- Dispatch: replace the inline block (2513–2517) with the
  `if (effectRoutes) { settleEffectMutatingRoute(effectRoutes, 'POST /mail/ack',
  effectRoutes.mailAck(mailAckCapabilities(ev)), res, CONTROL_DEFECT); return; }`
  primary + the verbatim legacy `json(res,200,{ok:true,...out})` below as the
  rollback seam.
- Settler behavior inherited: success → `json(res, 200, body)`; quiesce → frozen
  `503 {ok:false, reason:'shutting-down'}` (an intra-quiesce admission refusal
  never replays the ack); defect → `CONTROL_DEFECT` (`'fleetd handler error:'` +
  `500 {err:'internal'}`), byte-matching the inner-catch (3062) the legacy throw
  lands in.

### B) GET `/api/settings` → `settleEffectSnapshotRoute`

Sync + always-200 read. Direct analogue of **GET `/state`** (`dispatchState` →
`settleEffectSnapshotRoute`, http.ts:947), whose defect arm emits exactly
`500 {}` + `'fleetd request error:'` — the same outer-catch (3077) dialect this
route already inherits.

- New workflow builder `effectRoutes.settingsSnapshot(caps)` returning
  `Effect<{ok:true, settings:{…}}, never, never>` — the **whole body** is the
  success value (the settler does `json(res, 200, outcome.value)`).
- Capability bridge: `settingsSnapshotCapabilities() = { resolve: () =>
  core.resolveSettings() }`; the workflow body is `return {ok:true,
  settings: caps.resolve()}`. **`resolveSettings` stays a sync frozen leaf**
  (gateway masking preserved — the value is passed through untouched).
- Dispatch: new `dispatchSettingsRead(res)` (mirrors `dispatchState`): `if
  (!effectRoutes) { <legacy 2285 body> ; return } settleEffectSnapshotRoute(…,
  'GET /api/settings', effectRoutes.settingsSnapshot(caps), res, legacyRead)`
  where `legacyRead(res)` is the verbatim `json(res,200,{ok:true,settings:
  core.resolveSettings()})` rollback seam.
- Settler behavior inherited: success → `json(res, 200, value)`; quiesce →
  `legacy(res)` (a snapshot read answers 200 exactly as before shutdown); defect
  → rethrow → `'fleetd request error:'` + `500 {}` (byte-identical to today).

### C) POST `/mail` core → coarse degenerate Effect discharged via `runControlDetached` (transport unchanged)

The transport is done; only `postMailImpl` moves. Follows **P9.2 §6 OQ-1
(degenerate zero-gate core)** for granularity and **P9.4's pane-delivery
discharge** (`own(runControlDetached(effect))`, NOT `ingress.runPromise`) for the
runner.

- New Effect core `postMailEffect(args) = Effect.promise(() =>
  postMailImplLegacy(args))` — a **single coarse `Effect.promise` tail wrapping
  the verbatim async body** (sync validation prefix + probe fan-out + internal
  quiesce gate + sync inserts), behind `EFFECT_CORE_POST_MAIL` with a
  `postMailImplLegacy` twin (the current body, renamed) as the rollback seam.
  R = never, E = never; expected outcomes (422/409/429/quiescing/success) are
  DATA the caller already returns — only a genuine throw dies.
- Discharge at the capability boundary — `postMail` (mail.ts:977–980) becomes:
  keep the outer `if (!isOpen()) return Promise.resolve(quiescingPostMailResult())`
  pre-gate verbatim; then `return own(EFFECT_CORE_POST_MAIL ?
  runControlDetached(postMailEffect(args)) : postMailImpl(args))`. `postMail`
  still returns a **native Promise**, so `mailCapabilities`, the `startOnce`
  recorder, and `settleEffectAsyncMutatingRoute`'s JOIN-on-interrupt are
  **untouched** — no transport edit, no new run* site (`runControlDetached` is
  the existing context-free control-detached runner already used by P9.4's
  pane-delivery, ingress-supervisor-live.ts:52).
- `ownedPaneDeliverable` (mail.ts:527–539) is **NOT converted** — frozen shared
  leaf (see §4). This resolves P9.4's deferred leg 1C as *freeze-as-leaf*, the
  same doctrine applied to `ackMail`/`resolveSettings`.

Settler naming summary: A → `settleEffectMutatingRoute`; B →
`settleEffectSnapshotRoute`; C → `settleEffectAsyncMutatingRoute` (already
wired, no change). **Parameterized settler needed: none.**

---

## §3 — Minimal slice plan

Three flag-gated slices, each with a legacy rollback seam (twin/null-builder),
ordered simplest-first. No slice adds a run* call site or a settler.

**Slice 1 — GET `/api/settings` (B).** Zero-mutation read; reuses the proven
`/state` settler. Add `effectRoutes.settingsSnapshot` builder +
`dispatchSettingsRead` + `EFFECT_CORE_SETTINGS_READ` flag + legacy rollback.
- Characterization gap: pin the `200 {ok:true, settings:{…}}` envelope
  (especially the **masked** `gateway` field and `hold_ms`) as bytes — `/state`
  corpus exercises `resolveSettings`'s *values* but likely not the
  `/api/settings` *envelope*. Add one HTTP-level snapshot pin. The `500 {}`
  defect arm is near-unreachable (sync leaf) but shares `/state`'s already-pinned
  bytes.

**Slice 2 — POST `/mail/ack` (A).** Sync mutate; reuses the arm-unsupervised
settler. Add `effectRoutes.mailAck` builder + `mailAckCapabilities` +
`EFFECT_CORE_MAIL_ACK` flag + legacy rollback. **Freeze `ackMail` leaf + lease
SQL.**
- Characterization gap: pin the `200 {ok:true, acked:N}` body across `mail_id`
  present-integer / absent / non-array / non-safe-integer (the `acked:0` folds)
  and the guarded-UPDATE `changes` count. Pin the `CONTROL_DEFECT` (`500
  {err:internal}`) arm as unreachable-by-construction (§5 OQ-1). BUG-034 tests
  likely cover the *lease* behavior but not the *HTTP envelope* — add HTTP pins.

**Slice 3 — POST `/mail` core (C).** Async core → coarse degenerate Effect +
`runControlDetached` discharge; transport unchanged. Add `postMailEffect` +
`postMailImplLegacy` twin + `EFFECT_CORE_POST_MAIL` flag. **Freeze
`ownedPaneDeliverable` + tmux leaves + the internal quiesce-gate ordering.**
- Characterization gap: the POST `/mail` wire is ALREADY frozen by P9.4's mail
  corpus (q-corpus ≈334; see memory `p9-4-mail-effect-core`). Verify those pins
  span the full route matrix (`watcher / pane / offline-queued / turn-boundary /
  refused`) × (`422 / 409 / 429 refusedAll / quiescing / success + truncation`).
  If the `pane` route (the only leg touching `ownedPaneDeliverable`'s tmux
  probe) is under-covered, add a pin BEFORE moving the core.

Order rationale: B (read, no mutation, reuses `/state`) → A (sync mutate, new
builder, freezes lease) → C (async core, most danger, but transport already
proven). Independent flags mean any slice can land or roll back alone.

---

## §4 — Danger notes

**D1 — ackMail lease: the precise P9.5-vs-P10 boundary (the task's headline
question).** `ackMail` (mail.ts:400–410) runs `q.ackMail.run(now, id)` — the
`delivered_at IS NULL`-guarded UPDATE that **finalizes** a BUG-034 lease
(`drainMail` LEASES via `claimed_at`; `ackMail` finalizes via `delivered_at`).
The lease *protocol* — drain-leasing, finalization, and the retention-sweep
re-delivery of un-acked claims — is a **P10** concern (GET `/mail` at
http.ts:2329/2336/2341, GET `/api/watch` at 2346). P9.5 converts **only the POST
`/mail/ack` HTTP route/transport onto `settleEffectMutatingRoute`**, keeping
`ackMail` a *sync capability leaf*. P9.5 must NOT touch: the `q.ackMail` SQL,
`drainMail` leasing, the lease/quiesce interaction, or the retention sweep. The
leaf is co-owned with GET `/mail` (P10) — moving it now would entangle two
migration phases.

**D2 — resolveSettings is a triple-shared leaf.** Read by GET `/api/settings`
(P9.5-B), by POST `/api/settings`'s success body (Effect DONE, settings.ts:713/
739), and by the `/state` broadcast. Its `gateway` masking (settings.ts:797–799)
is a *credential-safety* invariant. P9.5 converts the *route*; the leaf and its
masking stay frozen (pass-through only).

**D3 — postMail's internal quiesce gate ordering is load-bearing.** The `if
(!isOpen()) return quiescingPostMailResult()` at mail.ts:931–933 sits BETWEEN the
non-cancellable probe fan-out and the first SQLite `mail()` insert, deliberately
refusing after probes but before any mutation. A **coarse Effect.promise wrapping
the verbatim body preserves this gate inside the wrapped thunk** — this is the
reason to use the degenerate-core shape and NOT to restructure the probe
fan-out into separate Effect steps (which could reorder the gate). The OUTER
`postMail` pre-gate (mail.ts:978) must also stay verbatim, before the Effect is
even constructed.

**D4 — the mail insert must survive shutdown-join; discharge with the
context-free runner.** `settleEffectAsyncMutatingRoute` JOINs the recorded native
Promise on an interrupts-only Exit (closeClients waits for the in-flight,
un-cancellable SQLite write). Discharging the core with `runControlDetached`
(`Context.empty()`, NOT supervisor-tracked) preserves "supervisor.interrupt()
cannot cancel a started write" — the exact frozen invariant. Do NOT discharge
with `ingress.runPromise`/`runRequest` (would double-run through the transport
and could make the write interruptible).

**D5 — ownedPaneDeliverable / tmux probes are P10-adjacent, freeze.** Shared with
`/state` cheap-mode (`probe:false`). Its `findScopedWindow`/`paneCurrentCommand`
are tmux I/O leaves. Converting it buys no wire fidelity (its boolean is consumed
inside postMail) and risks the `/state` snapshot path. Freeze — resolves P9.4 1C.

**D6 — do NOT move, this phase:** GET `/mail` (P10), GET `/api/watch` (P10),
`/ws/term` (P7), POST `/api/worktrees/remove` (P9.2 slice 3), the
`applyEvent` hook telemetry fast-path (P6.4 surface, §5 OQ-2). Each shares state
or a route family with an item above; touching them now crosses a phase line.

**D7 — success-body shape asymmetry (C).** `postMailImpl` returns a **bare
`{ok:true,…}`** on success but `{status,body}` on error; the transport's
`onFulfilled` (`rec.status ?? 200, rec.body ?? out`, http.ts:1523–1525) already
absorbs both. A coarse core preserves this because it returns the identical union
— do not "normalize" the bare success into `{status,body}` (would double-wrap the
body).

---

## §5 — Open questions (with recommendations)

**OQ-1 — POST `/mail/ack` defect dialect: `CONTROL_DEFECT`, or is the arm
unreachable-by-construction?** `ackMail` is sync and type-guards every input
(`Number.isSafeInteger`), so the `500 {err:internal}` arm cannot fire from the
core in practice. **Recommendation:** wire `CONTROL_DEFECT` for doctrinal
byte-fidelity with the inner catch (3062) the legacy throw lands in, and add a
characterization comment marking the arm unreachable-by-construction (mirroring
arm-unsupervised's stance). Do not invent a mail-specific defect body.

**OQ-2 — Is the AskUserQuestion `applyEvent` fast-path (http.ts:2479–2486) P9.5
or settled P6.4?** It lives inside the already-Effect `/hook/` surface, answers a
**constant `200 {}`**, and its only core touch is a fire-and-forget
`core.applyEvent` telemetry write with no wire-observable result.
**Recommendation:** OUT of P9.5 — freeze. Routing a constant-response
fire-and-forget through Effect yields zero fidelity benefit and widens the hook
surface P6.4 already closed. Flag to the orchestrator for an explicit P6.4-owned
disposition.

**OQ-3 — postMail core granularity: coarse degenerate vs structured probe-step.**
**Recommendation:** coarse degenerate `Effect.promise` (P9.2 §6 OQ-1), freezing
`ownedPaneDeliverable` and the tmux leaves. The internal quiesce-gate ordering
(D3) and the non-cancellable insert (D4) make any finer decomposition strictly
riskier for no fidelity gain. This is the exact resolution P9.4 deferred (§1D/
§5-Q1): D1(fan-out)+1C(pane) collapse into one coarse core + one frozen leaf.

**OQ-4 — one slice or three?** **Recommendation:** three (per §3). The items
share no code and split cleanly by settler + risk (read / sync-mutate-with-lease
/ async-core-with-tmux). Independent flags keep the ingress pin at 2 and let the
riskiest (C) land last behind its own rollback seam.

**OQ-5 — after P9.5, what closes the HTTP ladder?** Only P10 holds (GET `/mail`,
GET `/api/watch` and the lease/retention machinery) and P7 (`/ws/term`) remain
HTTP/WS-triggered. **Recommendation:** state in the P9.5 landing note that P9.5
is the *last application-service* conversion and that the residual is entirely
P10/P7 by design, so the completion map can mark the P9 HTTP-service column
closed.

---

## §6 — ORCHESTRATOR ADJUDICATION (Fable, 2026-08-24)

**Inventory ACCEPTED as the authoritative P9 exit-gate account:** three leftover application services (settings read, /mail/ack route, postMail core); the residual (GET /mail, GET /api/watch, /ws/term) is P10/P7 by design — OQ-5 ACCEPTED, P9.5 is the LAST application-service conversion package.

**OQ-1 ACCEPTED:** /mail/ack rides the mutating family with CONTROL_DEFECT wired and the defect arm documented unreachable-by-construction (sync leaf). **OQ-2 ACCEPTED:** the hook telemetry fast-path stays OUT (P6.4 disposition stands). **OQ-3 ACCEPTED:** postMail converts as a coarse degenerate core via runControlDetached with the pane/tmux leaves frozen — this RESOLVES the P9.4-deferred legs 1C/1D; the internal quiesce gate's position between probe fan-out and first insert is BYTE-FROZEN inside the coarse thunk (the §4 danger is binding: no finer decomposition). **OQ-4 ACCEPTED:** three slices, simplest-first.

**Reviews RULED:** slices 1 (settings) and 2 (/mail/ack) = orchestrator line review (sync leaves, existing byte contracts, characterization-first pins mandatory); slice 3 (postMail core, the quiesce-gate danger) = adversarial-review-mandatory.

**Sequencing:** P9.5 executes AFTER P9.2 slices 2+3 land (shared http.ts anchors would conflict); single worker may take slices 1+2 together, slice 3 separate.
