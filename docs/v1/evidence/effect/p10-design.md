> **Stamped 2026-08-24.** This is the canonical, adjudicated P10 design, stamped
> verbatim (including the §6 orchestrator adjudication) from the working draft
> `p10-design-draft.md`. Its line anchors are against the design-time HEAD
> `b3666ca8` (P9 COMPLETE); the landed slices moved some anchors — the
> **as-landed** commit ladder, verdicts, findings, seams, suites, and bundle are
> recorded in the sibling close record [`p10-close.md`](./p10-close.md). Nothing
> below was edited on stamping.

---

# P10 design draft — holds / fail-open cleanup

**Status:** DESIGN / INVENTORY ONLY. No source changed. This document is the sole
artifact. All line anchors are against worktree `/tmp/fd-wt-p9-4-design` @ HEAD
`b3666ca8` (P9 COMPLETE). Follows the `p9-1-design.md` template (§1 lettered
inventory → §2 target shapes → §3 slice plan → §4 danger notes → §5 open
questions).

**Scope (per `docs/v1/effect-migration-plan.md` P10 box, lines 967-989, and
`docs/v1/evidence/effect/p9-completion-map.md`):** the four P10-DEFERRED
surfaces — (a) the hook **HOLD relay** (permission / elicitation / choice) and
its UX-2.1 **re-arm** chain, (b) **GET /api/watch** (held long-poll), (c) **GET
/mail** (leased drain), (d) the questions **orphan sweep** `setInterval`
(questions.ts:1432) — plus the P10.4 requirement that root quiesce settle every
outstanding hold to canonical `200 {}` before HTTP stops.

**The one doctrine that governs the whole package:** the hook fail-open contract
(`tests/p6-hook-failopen-contract.test.ts`, invariants in
`docs/v1/effect-feasibility.md`). Every `/hook/*` answer is HTTP `200 {}` unless a
handler deliberately produced output — never 401/500/503, never a Cause / stack /
token / path. Holds were deferred at P6.4 because they hold the response open
*across time*; converting them means preserving that fail-open through a parked
response with multiple racing settle legs. **Fail-open must never become a failed
Effect.** The prior art is `mapHookExit` (hook-policy.ts:45) +
`settleEffectHookRoute` B1/B2 (http.ts:2176) — a *total* Exit→`{body:{}}` fold on
the transport terminal.

---

## §1 — Inventory: every async leg, timer, and held-response lifetime in scope

Each entry gives Entry / Await legs (what can settle the response) / Cancellation
/ Wire dialect / Atomicity, with file:line evidence.

### 1A — Hook HOLD relay (permission / elicitation / choice) — the P10 core

**Entry.** http.ts:2682-2689: for `name ∈ {PermissionRequest, Elicitation,
AskUserQuestion}` (after the AskUserQuestion→PermissionRequest instant-`{}` guard
at 2674-2681) → `holdHook(res, ev, name)` (http.ts:2100), `return // Phase 3/4
hold-open relay`.

**holdHook body (http.ts:2100-2137):**
- quiescing → `json(res, 200, {})` immediately (2101-2107).
- `row = core.hookHoldQuestion(ev, name)` (2110). Returns `{id} | null`; `null`
  (observation-only session, or rolled-back ExitPlanMode intake) →
  `json(res,200,{})` fail-open.
- `core.questions.attachHold(held, (obj) => json(res, 200, obj))` (2123-2126).
- `res.on('close', () => core.questions.socketClosed(held.id))` (2131).
- comment `// response intentionally left open` (2136).

**attachHold (questions.ts:459):** `!active()` → `respondFailOpen(respond)`
(=`respond({},200)`, questions.ts:391); per-session cap `MAX_HOLDS_PER_SESSION=4`
evicts the oldest via `settleExpired`; arms
`setTimeout(() => settleExpired(row.id), expires_at - now).unref()`;
`holds.set(id, {session_id, respond, timer})` (questions.ts:348 type
`HoldEntry`).

**Await legs — the FIRST-SETTLEMENT race (whichever wins settles `res` once):**
1. **Board answer** → `answer()` (questions.ts:867) → `respond(decision)` — the
   ONLY non-`{}` body.
2. **Hold-window lapse** → the unref'd per-hold timer → `settleExpired`
   (questions.ts:613) → `respond({})` + `scheduleRearm` (see 1B).
3. **Socket disconnect** → `res.on('close')` → `socketClosed(held.id)`
   (questions.ts:811) — drops the entry, no respond (peer is gone).
4. **Per-session cap eviction** → `settleExpired(oldest)` inside `attachHold`.
5. **Board disconnect** (last snapshot WS closes, `snapshotClients.size===0`) →
   `core.questions.failOpenAllHolds()` (http.ts:3504-3506) → responder-first
   snapshot, `respond({})`, **re-arm suppressed** (the terminal now owns each
   question; no board consumer for a successor).
6. **Shutdown quiesce** → `releaseHeldResponses()` (http.ts:4145) →
   `questions.releaseAll()` (questions.ts:525) → every held `respond({})` before
   native stop (P10.4). See §1G + §4-D2.
7. **`dismiss`** (questions.ts:1272) and activity-driven expiry
   `expireOnActivity` (questions.ts:1157) / `expireOrphans` (1308) —
   telemetry-driven retirements that also fire `respond({})` on a held row.

**Cancellation.** `timer.unref()` (never keeps the process alive);
`res.on('close')` unregisters. `respond` is guarded by the `holds` map identity —
once removed, no double-write.

**Wire dialect.** Success = the board's decision object; **every** other leg =
`200 {}`. `respondFailOpen` centralizes the `{}` , status 200 contract
(questions.ts:388-393).

**Atomicity (HOTSPOT — completion-map "questions holds + DB same tick").**
`hookHoldQuestion` (events.ts:874) is where the durable row is born. For
ExitPlanMode `PermissionRequest` it wraps `questions.create` + plan capture in one
`db.exec('BEGIN IMMEDIATE')` transaction (events.ts:916; M-B6 / BUG-112: both
rows persist or neither, telemetry applied only after COMMIT). The map insert
(`holds.set`) happens in `attachHold` in the SAME request tick, after the row
exists. **Do not split map-vs-row across fibers** (completion-map atomicity note).

### 1B — UX 2.1 re-arm chain (in-memory timer machinery)

**Entry.** `settleExpired` (questions.ts:613) calls `scheduleRearm`
(questions.ts:656) after a lapsed hold. **Timers/maps:**
- `REARM_GRACE_MS = 3000` grace timer per expired row; on fire → `fireRearm`
  (questions.ts:705) creates a fresh *mail-delivered* successor row.
- `MAX_REARMS = 2` (questions.ts:197) chain cap (~3 total human chances, ~30 min
  at the 600 s default).
- maps `rearmById` / `rearmMeta` / `rearmChains` (questions.ts:359-361);
  `recycleRearm` (740), `cancelRearm` (771), `disarmRearmsForSession` (785).
- **In-memory only, deliberately** (questions.ts:349-361): "a dead socket can
  never be re-parked"; a daemon restart forfeits the grace window → fails safe to
  pre-2.1.

**Wire dialect.** No held response of its own; it manufactures a successor card
delivered as *mail* (→ 1E/1F path). Suppressed by `failOpenAllHolds` (1A-5).

### 1C — Orphan sweep (`setInterval`) — questions.ts:1432

```
const sweep = setInterval(() => { if (!active()) return; try { expireOrphans() } catch {} }, orphanSweepMs);
sweep.unref();
```
- `orphanSweepMs = SWEEP_MS = 5000` (injectable via `createQuestions` opts).
- `expireOrphans` (questions.ts:1308): deliberate **redundant** cleanup of
  pending holds whose owning session/card is gone. `catch {}` = fail-open /
  audit-silent by design.
- Cleared in `close()` (questions.ts:1452) via `clearInterval(sweep)`.
- Completion-map: "P1 timer, unref'd, keep on explicit P1 handle until P10."

### 1D — BUG-138 completed-correlation ledger (in-memory, TTL timers)

`completedKeys: Map<sid, Map<toolCallKey, count>>` + `completedKeyTimers: Set`
(questions.ts:374-375); `COMPLETED_KEY_TTL_MS = 60000`. Lets a correlated
PostToolUse be *consumed* against a prior board answer so a twin hold on identical
`(tool_name, tool_input)` survives its sibling's completion
(`noteCompleted`/`consumeCompleted`). Same daemon-lifetime ephemera class as
`holds`. All timers cleared in `close()`.

### 1E — GET /api/watch (watchHook) — leftover held long-poll

**Entry.** http.ts:2541 route → `watchHook(_req, res, url)` (http.ts:2319).
- quiescing → immediate `json(res, 200, {status:'idle', session_alive:false,
  pending:0})` (2321-2324).
- `holdMs = clamp(0..25_000, default 25_000)` (2326).
- `wg` watch-generation token (BUG-105): `''`→`null` else register newest-wins
  `core.registerWatchGen` (2332-2334).
- `attempt()` = `watchInfo` + `claimMail(sid, wg)` (2336-2342) — **BUG-034 lease
  claim** (claimed mail = expiring in-flight lease). Immediate attempt → `json`
  and return (2344-2348); else park.

**Await legs (idempotent `finish`, latch `settled`, http.ts:2355):**
1. **Waiter wake** → `core.addWatchWaiter(sid, cb)` (2375); cb re-runs `attempt`,
   `finish(out)` if mail.
2. **Hold-timer lapse** → `setTimeout(..., holdMs).unref()` (2371-2374) →
   `finish({status:'idle', ...watchInfo})`.
3. **Shutdown** → `closeForShutdown` in `activeWatchClosers` (2367-2370) →
   `finish` idle; invoked by `closeClientsOnce` (http.ts:4200-4206).
4. **Socket disconnect** → `res.on('close')` (2380-2385): sets `settled`, clears
   timer, unregisters, removes closer (no respond).

**Cancellation.** `timer.unref()`; `finish` clears timer + unregister +
`activeWatchClosers.delete`.

**Wire dialect.** ALWAYS `200`, body `{status:'idle'|'mail', ...}`. **NOT the
fail-open-`{}` contract** — it is an *idle poll fold*, not a hook. Distinct
terminal from 1A.

### 1F — GET /mail — leased drain (synchronous; NOT a held response)

http.ts:2524-2540. `ackMail(ackIds)` (2531) then `core.drainMail(sid,
{lease:true})` (2536); `json(res, 200, {mail, ack_mail_ids})`. **Fully
synchronous** — no parked response, no timer. Behind the CSRF wall for mutating
GETs (`crossSiteReason`, http.ts:2455 for `/mail` + `/api/watch`). **BUG-034
lease:** the board must hand the ids back on its next poll; a poll whose response
never reached the board leaves rows leased (not delivered) → retention sweep
releases → re-delivered, never lost (2526-2535). Its sibling ack, POST
`/mail/ack`, is ALREADY Effect (P9.5 slice 2, http.ts:2708-2729,
`settleEffectMutatingRoute` + `CONTROL_DEFECT`).

### 1G — Shutdown ordering (held responses vs closeClients) — P10.4 anchor

Lifecycle object http.ts:4276-4295. Ordered phases:
- `releaseHolds: releaseHeldResponses` (http.ts:4145). Returns
  `questions.releaseAll() ?? failOpenAllHolds()`. In `finally`, sets a
  one-shot barrier `holdsReleaseStarted`: collects `activeResponses.filter(hook)`
  promises and `Promise.allSettled(...).then(resolveHoldsReleased)` (http.ts:4152-4162)
  — **delays the native graceful-stop until every hook response present at the
  phase boundary is observable by Bun** (Bun 1.3.14 resets a held socket if
  `stop()` beats the fetch-Promise boundary).
- `closeClients: closeClientsHttp` (http.ts:4236 → `closeClientsOnce` 4194):
  first drains `activeWatchClosers` (each `closeWatch()` → 1E-3 idle finish,
  4200-4206) — comment 4197-4199: *"Watch polls are not hook decisions … Held
  hook decisions are owned by the preceding releaseHolds phase and deliberately
  stay separate."* Then forces faulted responses, `beginNativeClientClose`,
  awaits `activeResponses` promises.
- `closeHttpOnce` (http.ts:4241): `releaseHeldResponses()` FIRST (4248, "Held
  hooks must receive canonical 200 {} while Bun can still write"), then
  `closeClientsHttp`, then `forceStopHttp`.
- `questions.close()` (questions.ts:1452): `quiesce` → `releaseAll` →
  `clearInterval(sweep)` + clear all rearm/completed timers → `phase='closed'`.

**Invariant to preserve:** holds settle to `200 {}` in the `releaseHolds` phase,
BEFORE watch closers and BEFORE native stop; watch long-polls settle idle in
`closeClients`. Neither may wedge shutdown (all timers unref'd; barrier only waits
on already-published responses).

### 1H — The LOCKSTEP chain (verified product behavior — do not perturb)

- Daemon hold window: `resolveHoldMs` clamps to **≤ 650_000 ms** (650 s ceiling,
  questions.ts:223); `DEFAULT_HOLD_MS = 600_000`.
- Shim watchdog for hold events: `rearmWatchdog(66e4)` = **660_000 ms**
  (scripts/fleet-hook.mjs:591; `HOLD_EVENTS` set at 478; fetch aborts at
  `watchdogMs-400`).
- hooks.json `timeout` for the three hold hooks: **720** s (hooks/hooks.json:85,
  102, 119).
- Chain: **650 s (daemon) < 660 s (shim) < 720 s (hooks.json)** — else the
  board's answer lands on a dead socket and the hook fails open. Comment
  questions.ts:199-210.

### 1I — Tests that pin these surfaces (all GREEN, read-only, this HEAD)

| Test | Result | Pins |
|---|---|---|
| `board-hold-presence.test.ts` + `p1-question-retention-lifecycle.test.ts` | 4 pass (1.4 s) | board-consumer probe gating; retention lifecycle |
| `mail-delivery-lease.test.ts` | 5 pass (4.5 s) | BUG-034 lease claim/ack/lapse/re-deliver |
| `question-rearm.test.ts` + `watch-rewake.test.ts` | 27 pass (**64 s** — real hold/rearm timers) | re-arm chain + grace, MAX_REARMS; watch wake/idle/lease |
| (also present) `questions-audit`, `choice-relay`, `p1-mail-lifecycle`, `effect/http-workflow-control` | — | hold audit, choice pairing, mail lifecycle, control settlers |

The 64 s wall of the rearm/watch pair is the real-timer lockstep behavior; keep
these unmocked as the P10 exit oracle.

---

## §2 — Target shapes

Three distinct terminals, three answers. Contrast with the landed classes:
`settleEffectMutatingRoute` (http.ts:1264, → 503/500 via `mapEffectRouteExit`
http-policy.ts:406), `settleControlAsyncRoute` (http.ts:1418, start-once/join),
`settleEffectHookRoute` (http.ts:2176, fail-open via `mapHookExit`).

### 2A — GET /mail → mutating-settler slice (NOT a held response)

GET /mail is a synchronous leased GET (ack + drain). It maps cleanly onto the
**existing** mutating class already used by its sibling POST /mail/ack (P9.5):
a degenerate `Effect.sync` capability workflow (`ack` + `drain` thunks) settled by
`settleEffectMutatingRoute` with `CONTROL_DEFECT`. Wire bytes
(`{mail, ack_mail_ids}`) frozen. **No held-response machinery needed.** This is
arguably a P9-class conversion that merely lived in the P10 box.

### 2B — GET /api/watch → held-poll settler with an IDLE fold

A parked response with a first-settlement latch (`settled` today, http.ts:2350) —
the natural Effect shape is a **`Deferred<WatchResult>`** completed by whichever
leg wins (waiter / timer / shutdown / disconnect), the response written once from
the Deferred's value. Terminal fold = **idle-info**, NOT `mapHookExit`:
`{status:'idle', ...watchInfo}` on lapse/shutdown, `{status:'mail', ...}` on wake.
The 25 s bound becomes a `Clock`-driven timeout on the Deferred. `activeWatchClosers`
membership → the shutdown leg completing the Deferred idle. `res.on('close')` →
interrupt the fiber / abandon the Deferred (no write). **This is the
lower-risk introduction of the held-response primitive** (idle fold is forgiving;
no fail-open contract) — do it BEFORE 2C.

### 2C — Hook HOLD relay → held fail-open settle shape (the NEW shape)

This is the shape the whole package exists to design. Per plan P10.2: **model each
hold/rearm lifetime as a child `Scope` with a `Deferred` result and explicit
first-settlement semantics.**

- **Primitive:** `Deferred<HookResponse>` (HookResponse = `{body}` from
  hook-policy.ts:33 — no status field, non-200 unrepresentable). The parked `res`
  is written exactly once, from the winning leg, as `json(res, 200, plan.body)`.
- **First-settlement:** all N legs (1A.1–1A.7) race to `Deferred.complete`. Only
  the answer leg completes with the decision body; **every other leg completes
  through `mapHookExit`**, i.e. `{body:{}}`. Because a Deferred completes at most
  once, "first wins, rest are no-ops" is structural — the exact semantics of the
  `holds`-map identity guard today.
- **Fail-open is TOTAL and never a failed Effect (the hard rule).** The Deferred is
  `Deferred<HookResponse, never>` — its value is always a `{body}`; a die in any
  leg is folded to `{body:{}}` at completion via `mapHookExit`, never propagated as
  an `E`/defect out of the hold. The transport terminal is the *existing*
  `settleEffectHookRoute` discipline, extended across time:
  - **B1** (reply survives interruption): the answer/lapse completion must survive
    fiber interruption during shutdown — emitted from the terminal arm, as today.
  - **B2** (`HOOK_REPLY_FLOOR_MS = 5000` unref'd idempotent floor, http.ts:2154):
    a wedged runtime still emits `{}`. Keep it; it is the last-resort total fold.
- **Where the fold lives:** on the transport side (`mapHookExit`), NOT as a
  `catchAllCause` inside a workflow — identical reasoning to P6.4 hooks
  (hooks.ts:28-44): the workflow/legs stay `E = never` with no catch; a thrown
  handler becomes a die that `mapHookExit` collapses to `{}`.
- **Rollback seam:** keep the P1 hold manager (the `holds`/rearm Maps + `attachHold`
  + `settleExpired`) as the owner behind a policy adapter and an
  `EFFECT_CORE_HOLD_*` flag with a verbatim `*Legacy` twin (plan rollback rule,
  lines 987-988). The Deferred/Scope wraps the *response settlement*, not the
  durable row lifecycle, until the full race matrix (§3 Slice 0) is green.

### 2D — Orphan sweep → scoped `Effect.repeat`/`Schedule`/`Clock` (CONDITIONAL)

Per plan P10.3, convert the `setInterval` (1C) to a scoped fiber driven by
`Effect.repeat` + `Schedule.fixed(SWEEP_MS)` + the `Clock` service **only if** it
preserves (i) the deliberate *redundant* cleanup and (ii) fail-open
auditability (the silent `catch {}`). Note (from plan): Effect's `Scheduler`
service is fiber-dispatch, NOT the periodic-timer abstraction — use `Schedule`.
The re-arm/completed-key timers (1B/1D) stay **in-memory, un-converted** — same
"a dead socket can never be re-parked" reasoning as `holds`; Effect-ifying them
buys nothing and risks the lockstep. Recommend converting the sweep LAST, or
deferring past P10 if the redundancy proof is not clean.

---

## §3 — Slice plan (characterization-first; blast radius; rollback; review class)

Operating rule (plan lines 445-453): characterization test → smallest
behavior-preserving change → focused tests → global gate → regenerate bundle →
update ledger → revert on any hard-invariant break.

**Slice 0 — Characterization & fixtures (NO src change).** Add the P10.1 fixtures
under `tests/effect/fixtures/`: disconnect, timeout, no-board, persistence
failure (hookHoldQuestion rollback), duplicate completion (BUG-138), rearm race,
daemon shutdown, HTTP close, defects before/after mutation. Byte-freeze the wire
dialects for 1A (`{}` / decision), 1E (`{status,...}`), 1F (`{mail,ack_mail_ids}`).
_Blast radius:_ tests only. _Rollback:_ n/a. _Review:_ characterization
completeness (does the matrix cover all seven 1A legs + the lockstep?).

**Slice 1 — GET /mail → mutating settler (2A).** Lowest risk; reuses the P9.5
class. _Blast radius:_ one route. _Rollback:_ `effectRoutes`-null legacy path +
`EFFECT_CORE_GET_MAIL` twin. _Review:_ lease-byte parity (BUG-034: `ack_mail_ids`
identical; ack-before-drain order preserved).

**Slice 2 — GET /api/watch → held-poll Deferred with idle fold (2B).** Introduces
the held-response primitive on the forgiving surface. _Blast radius:_ watchHook +
`activeWatchClosers` shutdown wiring. _Rollback:_ legacy `watchHook` behind flag.
_Review:_ shutdown-race (closer completes idle exactly once; `res.on('close')`
abandons without write) + the 25 s Clock bound.

**Slice 3 — Hook HOLD relay → held fail-open settle shape (2C).** The core.
Largest blast radius. Convert the *response settlement* to `Deferred<HookResponse>`
+ child Scope; keep the durable row + rearm Maps in the P1 manager. _Blast radius:_
holdHook, attachHold's respond seam, releaseAll/failOpenAllHolds, all seven legs.
_Rollback:_ `EFFECT_CORE_HOLD_RELAY` + verbatim `holdHookLegacy`/`attachHoldLegacy`
twins; `effectRoutes`-null. _Review:_ **adversarial** — every leg must fold to
`200 {}` (only answer carries a body); no Cause/secret can reach the client
(feasibility invariant); B1/B2 preserved; atomicity of hookHoldQuestion untouched.

**Slice 4 — Root quiesce settles all holds before stop (P10.4).** Verify/port the
`releaseHolds → closeClients → forceStop` ordering (1G) under the Effect model:
holds settle `200 {}` even if Store or terminal teardown fails. _Blast radius:_
lifecycle coordinator. _Rollback:_ P1 aggregate `close()` path. _Review:_
failure-injection (Store throws / terminal teardown throws → holds STILL settle;
no hold timer or Deferred left live — the exit gate).

**Slice 5 — Orphan sweep → scoped Schedule fiber (2D), CONDITIONAL.** Only if the
redundant-cleanup + audit-silence proof is clean; else defer. _Blast radius:_ the
sweep interval + `close()`'s `clearInterval`. _Rollback:_ P1 `setInterval` behind
flag. _Review:_ Clock-driven cadence == SWEEP_MS; `expireOrphans` still redundant
with the per-hold timers; scope teardown clears the fiber.

**Exit gate (plan lines 985-986):** all existing + new hook / needs-you /
board-hold suites pass **from source AND bundle**; every shutdown/failure race
returns control to the native terminal; no hold timer or Deferred left live.

---

## §4 — Danger notes (byte-frozen behaviors P10 must not perturb)

- **D1 — The lockstep (1H).** 650 s daemon < 660 s shim < 720 s hooks.json. A
  `Clock`/`Schedule` port of any hold timer MUST preserve the ≤ 650_000 ms
  ceiling (`resolveHoldMs`, questions.ts:223) exactly. Drift here lands answers on
  dead sockets. The 64 s real-timer test wall (`question-rearm` + `watch-rewake`)
  is the oracle — never mock it away.
- **D2 — Held responses vs closeClients (1G).** Holds settle in the `releaseHolds`
  phase BEFORE watch closers and BEFORE native stop; the `holdsReleaseStarted`
  barrier (http.ts:4152-4162) exists because Bun 1.3.14 resets a held socket if
  `stop()` beats the fetch-Promise boundary. A Deferred port must keep the parked
  response *observable by Bun* before the native stop — do not move settlement
  after `forceStop`.
- **D3 — BUG-034 lease protocol (1E/1F).** GET /api/watch CLAIMS mail as an
  expiring in-flight lease (`claimMail`); GET /mail drains leased; POST /mail/ack
  + the `ack=` query finalize. An unacked lease MUST lapse → retention sweep
  releases → re-delivered. Never make a claim delivered without an ack path; never
  drop `ack_mail_ids`. `mail-delivery-lease.test.ts` pins this.
- **D4 — Fail-open must never become a failed Effect (2C).** The hold Deferred is
  `…, never`; every non-answer leg completes through `mapHookExit` →`{body:{}}`.
  No leg may surface a Cause, defect, quiesce error, token, path, or stack to the
  client. A die folds to `{}` (as P6.4 hooks already do). The B2 floor
  (`HOOK_REPLY_FLOOR_MS`) is the total last resort — keep it.
- **D5 — Atomicity (1A).** `hookHoldQuestion`'s BEGIN IMMEDIATE (events.ts:916,
  M-B6/BUG-112) and the create+map same-tick coupling: do NOT split map-vs-row
  across fibers, and do NOT relocate the plan capture out of the transaction.
- **D6 — Board-disconnect fail-open (1A-5).** `snapshotClients` 1→0 →
  `failOpenAllHolds` with **re-arm suppressed** (http.ts:3504). A converted
  release path must preserve both: release ALL holds AND suppress the successor
  card (no board consumer exists).
- **D7 — AskUserQuestion→PermissionRequest pairing.** The instant-`{}` guard
  (http.ts:2674-2681) prevents chaining two ~50 s hold windows. It must stay
  BEFORE the hold relay and never itself become a hold.
- **D8 — In-memory ephemera is deliberate.** holds / rearm / completedKeys are
  daemon-lifetime only by design (a restart abandons every held socket). Do not
  "durable-ize" them under the Effect port.

---

## §5 — Open questions (with recommendations)

- **Q1 — Child Scope per hold, or one hold-manager scope?** _Recommend:_ keep the
  P1 hold-manager Map (`holds` + rearm Maps) as the durable owner behind the
  policy adapter; model the `Deferred<HookResponse>` per hold for *response
  settlement only*. Do NOT move the Maps into Effect state until the full race
  matrix (Slice 0) is green (plan rollback rule). One scope owns the sweep fiber;
  per-hold Deferreds hang off the manager, not off request fibers (P6.4
  HTTP-CAPABILITY: R=never, request path is not the root fiber).
- **Q2 — Convert the orphan sweep at all (2D)?** _Recommend:_ CONDITIONAL / last,
  or defer past P10. It is unref'd, redundant, and audit-silent by design;
  `Effect.repeat`/`Schedule` buys uniformity but risks the redundancy/fail-open
  contract (plan P10.3 is explicitly conditional). Keep the P1 `setInterval`
  behind a flag regardless.
- **Q3 — One held-response primitive for 1A and 1E, or two?** _Recommend:_ ONE
  first-settlement `Deferred` primitive, **parameterized on the terminal fold** —
  `mapHookExit` (→`{}`) for hooks, idle-info for watch. Introduce it on watch
  (Slice 2, forgiving) before hooks (Slice 3, fail-open-critical).
- **Q4 — Does GET /mail belong in P10?** _Recommend:_ treat it as a trivial
  mutating-settler slice (2A / Slice 1). It is synchronous with no parked response;
  the genuine held-response core of P10 is **1A (hook holds) + 1E (watch)**. State
  this in the ledger so P10's scope isn't overstated.
- **Q5 — Rearm / completedKeys timers under Effect?** _Recommend:_ leave
  in-memory, un-converted (D8). Same reasoning as holds; converting risks the
  lockstep and buys nothing testable. Only their *teardown* must be reached by the
  scope `close()` (already is: questions.ts:1452).
- **Q6 — hookHoldQuestion rollback path (persistence failure fixture).** The M-B6
  transaction returns `null` → holdHook fails open `{}`. _Recommend:_ pin this as
  a Slice-0 fixture (defect BEFORE mutation → `{}`, terminal owns the decision);
  confirm the Effect port keeps the `null → 200 {}` fold verbatim.

---

### Appendix — current-HEAD anchor table (b3666ca8)

| Surface | File:line |
|---|---|
| hook hold relay dispatch | http.ts:2682-2689 |
| AskUserQuestion→PermissionRequest instant-`{}` | http.ts:2674-2681 |
| holdHook | http.ts:2100-2137 |
| settleEffectHookRoute (B1/B2) | http.ts:2176-2218 |
| HOOK_REPLY_FLOOR_MS | http.ts:2154 |
| watchHook (GET /api/watch) | http.ts:2319-2387 |
| GET /mail (leased drain) | http.ts:2524-2540 |
| POST /mail/ack (already Effect) | http.ts:2708-2729 |
| crossSiteReason CSRF wall | http.ts:2455 |
| releaseHeldResponses + barrier | http.ts:4145-4165 |
| closeClientsOnce (watch closers) | http.ts:4194-4234 |
| closeHttpOnce (release→close→stop) | http.ts:4241-4269 |
| lifecycle object | http.ts:4276-4295 |
| board-disconnect failOpenAllHolds | http.ts:3504-3506 |
| mapHookExit | hook-policy.ts:45 |
| hookDispatchWorkflow | app/http-workflows/hooks.ts:83 |
| mapEffectRouteExit | http-policy.ts:406 |
| settleEffectMutatingRoute / settleControlAsyncRoute | http.ts:1264 / 1418 |
| hookHoldQuestion (atomic intake) | events.ts:874; BEGIN IMMEDIATE 916 |
| resolveHoldMs (650 s ceiling) | questions.ts:211-228 |
| holds map / HoldEntry | questions.ts:348 |
| rearm maps | questions.ts:359-361 |
| completedKeys ledger | questions.ts:374-375 |
| attachHold | questions.ts:459 |
| releaseAll / failOpenAllHolds | questions.ts:525-569 |
| settleExpired | questions.ts:613 |
| scheduleRearm / fireRearm | questions.ts:656 / 705 |
| socketClosed | questions.ts:811 |
| orphan sweep setInterval | questions.ts:1432 |
| quiesce / close | questions.ts:1445 / 1452 |
| shim hold watchdog rearmWatchdog(66e4) | scripts/fleet-hook.mjs:591 |
| hooks.json hold timeout 720 | hooks/hooks.json:85,102,119 |

---

## §6 — ORCHESTRATOR ADJUDICATION (Fable, 2026-08-24)

**§2C ACCEPTED** — one Deferred<_, never> settle primitive under a child Scope, first-completion-wins, parameterized on the terminal fold (Q3 ACCEPTED): fail-open `{}` for hook holds via mapHookExit, idle-info fold for /api/watch. Fail-open stays TOTAL — a die folds to `{}`, never a failed Effect; the HOOK_REPLY_FLOOR_MS floor is byte-frozen.

**Q1 ACCEPTED and BINDING:** the P1 hold-manager Maps stay imperative behind the policy adapter; the Deferred wraps SETTLEMENT ONLY until the race matrix is green. This is the migration's standing doctrine (ownership imperative, join owned) — do not deepen ownership in P10.

**Q2 ACCEPTED:** orphan sweep is conditional/LAST; if the slice-5 analysis finds risk>value, record a DEFER with an explicit trigger (the R7/R8 convention) rather than forcing it.

**Q4 ACCEPTED:** GET /mail is a P9-class trivial slice — land it first as slice 1.

**Q5 ACCEPTED:** rearm/completedKeys timers stay in-memory.

**Reviews RULED:** slices 0-1 = orchestrator line review; slices 2 (watch Deferred), 3 (hook holds core), 4 (quiesce-settles-holds barrier — the D2 shutdown correctness) = adversarial-review-mandatory; slice 5 = per its own analysis.

**D1 BINDING:** the 650s < 660s < 720s lockstep constants are byte-frozen; the 64s real-timer wall (question-rearm + watch-rewake) is the per-slice exit oracle and must run 3x on both paths for slices 2-4.

**Sequencing:** slice 0 (characterization/fixtures) may start immediately in a worktree; slices 1+ serialize on the main tree after the P9 close-out docs land.
