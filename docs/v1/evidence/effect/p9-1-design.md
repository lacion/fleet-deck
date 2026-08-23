I now have the complete picture — spawns.ts in full, the dismiss pair in retention.ts, the P6.4 control transport seam, the custody/claim atomicity anchors, and the test inventory. Writing the deliverable.

---

# P9.1 DESIGN — Converting `spawns.ts` async orchestration to Effects

**Scope note.** This is a design/adjudication document. It is READ-ONLY output; no repo files were written. Every claim carries a `file:line` anchor against `fd/v1-effect-feasibility` @ `f395c81e`. After adjudication it becomes the P9.1 execution spec.

**One framing fact that shapes everything below.** `spawns.ts` is *mixed* (`p9-completion-map.md`). Its three **root-fiber** legs are already Effects and **must not be reopened**: `spawnLivenessTick` (`spawns.ts:3181`), `reconcileSpawns` (`spawns.ts:3395`), `reconcileClearForks` (`spawns.ts:3576`), with `healInterruptedRevives` (`spawns.ts:3548`) as their shared pure-SQLite helper — all delivered under P8.6, run under the root runtime, `yield* Store`. **P9.1 is exclusively the request-path async shell**: the orchestration reachable through the HTTP bridge (`spawn`, `revive`, `adoptSession`, `spawnKill`, `enableRemote`, and the dismiss pair in `retention.ts`) plus the P1 ownership/detached-provisioning machinery those paths ride. Per convention these are **HTTP-CAPABILITY**: capabilities-as-params, `R = never`, `E = never` (expected outcomes are `ControlWire` *data*, not typed errors), **do NOT `yield* Store`** (`http-workflows/control.ts:8-14`; `db-workflows/retention.ts:10-15`).

---

## §1 — THE ASYNC-SHELL INVENTORY

Every Promise-returning orchestration path on the request side, with entry point, await points, cancellation, failure dialect, and atomicity constraints.

### 1A. `spawn(body)` — POST `/api/spawn`
- **Entry:** `spawns.ts:1250`; dispatched **legacy-only** at `http.ts:2427-2482` (no `effectRoutes` branch — this route has *not* been through P6.4). Body pre-validated at `http.ts:2436` (`validateSpawnRequest`); the `.then/.catch` at `http.ts:2464-2480` maps `{status,body}`→JSON and a thrown error→`500 { reason: spawnFailureReason(err) }`.
- **Exposed as** `ownedSpawn` (`spawns.ts:3676`): `spawnMaintenance.run(() => spawn(body)) ?? Promise.resolve(shuttingDown())`.
- **Three internal sub-modes:**
  1. **repo/branch mode** (`hasRepo`, `spawns.ts:1494`): `await validateBranch` (:1516) → `await resolveTarget` (:1536) → `claimTarget` (:1555, sync check-and-set) → then either **in-place** (synchronous through worktree materialization) or **detached clone** (see 1B).
  2. **cwd mode** (`spawns.ts:1939`): "remains synchronous through optional worktree creation" — a `git worktree add` via `execFileP` is awaited but the response is not detached.
  3. **plan-claim wrapper** (BUG-040, see 1H).
- **Await points:** `validateBranch`, `resolveTarget`, `cloneRepo`, `materializeBranch`, `finishMaterialization`, `launchPane`, `git worktree add` (cwd mode), `spawnCompensate` on failure.
- **Cancellation:** repo/clone mode is **detached + abortable** (1B). in-place/cwd modes run to completion on the request turn (no signal).
- **Failure dialect:** expected outcomes are returned control results `{status, body}` (400 bad body, 404 no plan, 409 claim lost / target busy, 202 detached). A genuine throw propagates to `http.ts:2468` → `500 spawnFailureReason`. Inside, `wrapSpawnFailure` (`spawns.ts:1483`) and explicit `releasePlanClaim()` calls at every early-return failure (`:1518,:1530,:1543,:1557,…`) undo the plan claim.
- **Atomicity constraints:** BUG-040 claim-before-launch (`:1441`), `claimTarget` single-flight (`:1555`), `card()` get-or-create no-await (`derive.ts:605`), clone-slot + custody lease.

### 1B. Detached clone provisioning — the "p1 detached-clone join"
- **Site:** `spawns.ts:1826-1936`. `controller = new AbortController()` (:1826); `provisioningDone` Promise + `resolveProvisioning` (:1827-1830); `provisioningOp = {controller, done}` registered in `provisioningOps` Map by `spawn_id` (:1832).
- **Owned detached chain:** `spawnMaintenance.run(async () => {…})` (:1833): `cloneRepo` → `materializeBranch` → `finishMaterialization` → `launchPane`, with `controller.signal.aborted` re-checks between every await (`:1845,:1859,:1861`), a full `spawnCompensate` in `catch` (:1888), and a `finally` (:1900-1907) that releases clone slot + target, deletes the op **only if identity still matches** (`provisioningOps.get(spawn_id) === provisioningOp`, :1903), and calls `resolveProvisioning()`.
- **Response:** returns `202 {ok,provisioning:true,…}` immediately (:1925). Detached rejection swallowed to a log via `void provisioning.catch` (:1920).
- **Pre-abort guard:** if quiescing before launch, `spawnCompensate({cancelled:true})` + release + `503` (:1806-1824).
- **Cancellation semantics today:** AbortController; joined by (a) `spawnKill` (1E) and (b) `quiesceSpawns` which aborts **all** ops (`spawns.ts:3656`). **This is the migration-plan's explicit "keep detached supervisor/CLI launches out of the shared scoped process runner until survival & signal semantics have dedicated tests" case** (`effect-migration-plan.md` P9 note).

### 1C. `revive(spawn_id, body)` — POST `/api/spawn/:id/revive`
- **Entry:** `spawns.ts:2078`; dispatched at `http.ts:2512-2529`, **Effect-wrapped at transport** (`settleControlAsyncRoute`, `http.ts:2520`) with a legacy fall-through (`:2529`). Exposed as `ownedRevive` (`spawns.ts:3678`).
- **Single-flight:** `revivingSessions` Set (`spawns.ts:854`).
- **Shares the launch tail** `launchResume` (1D).
- **Failure dialect:** control result; collision/cap/terminal-evidence checks return status codes (owned by derive/spawns), throw→transport 500.

### 1D. `launchResume` — the shared launch tail (revive + adopt + **resurrect**)
- **Site:** `spawns.ts:2363`. R2-5 owner re-check (`:2416`), `insertProvisionalSpawn`.
- **MIXED CALLER SET (critical):** invoked by `revive` (request path) and `adoptSession` (request path) **and** by `resurrectSpawn` (`spawns.ts:905`), which is called from the **root-fiber** liveness/reconcile legs (`spawns.ts:3376` in `runSpawnLivenessTick`). So the launch machinery has **both** an HTTP-capability caller and a root caller. This is the central §4 danger.

### 1E. `spawnKill(spawn_id, force)` — POST `/api/spawn/:id/kill`
- **Entry:** `spawns.ts:2978`; dispatched at `http.ts:2483-2510`, Effect-wrapped (`settleControlAsyncRoute`, `:2489`) + legacy fall-through (`:2502`). Exposed `ownedSpawnKill` (`spawns.ts:3689`).
- **Provisioning-cancel path:** reads `provisioningOps.get(spawn_id)` (`:2986`), `op.controller.abort()` (:2988), `await Promise.race([op.done.then('done'), bounded(5s)])` (:2994) → `202 {cancelling}` on timeout (:2998).
- **Other awaits:** `killWindowVerified`; H-R5 stale-id refusal via `currentWindowOwner` (`spawns.ts:3050`).
- **Failure dialect:** control result (404/409/410/202), throw→transport 500.

### 1F. `enableRemote(spawn_id)` / `enableRemoteOnce` — POST `/api/spawn/:id/rc`
- **Entry:** `enableRemote` `spawns.ts:2829`, `enableRemoteOnce` `:2840`; dispatched `http.ts:2703-2712`, Effect-wrapped (`:2703`) + legacy fall-through (`:2712`). Exposed `ownedEnableRemote` (`spawns.ts:3687`).
- **Single-flight:** `remoteEnables` Map (`spawns.ts:2828`), `rcInputLocks` Map (`spawns.ts:866`). **Harvest race:** `Promise.race([...],6s)` (`spawns.ts:2964`).
- Related detached: `harvestRemote` (`:936`), `delayedRemoteHarvest` (`:978`, unref timer via `spawnMaintenance.schedule`), `scheduleRegistrationRemoteHarvest` (`:997`, memoized in `registrationRemoteHarvests` Map `:843`).

### 1G. `adoptSession(session_id, body, opts)` — POST `/api/sessions/:sid/adopt`
- **Entry:** `spawns.ts:2554`; dispatched `http.ts:2560`, Effect-wrapped. Exposed `ownedAdoptSession` (`spawns.ts:3680`). Shares `launchResume` (1D).

### 1H. The dismiss pair — POST `/api/sessions/:sid/dismiss` and `/dismiss/retry`
- **`dismissSession(sid)`** `retention.ts:525`; dispatched `http.ts:2648`, Effect-wrapped (`settleControlAsyncRoute`, `:2648`) + legacy fall-through (`:2658`). **Structure:** a **sync atomic DB block with NO awaits** (`retention.ts:556-573` — `setArchived` compare-and-set `:561`, mail/questions expiry, `goneSessionSpawns`, `deleteTouchesForSession`) followed by a **window-kill phase (the only awaits)** that re-reads `alive()` after each await to bail on a mid-await resurrection (`:575-579`). BUG-145 incomplete-result shape (`:588-604`, `retry:true`).
- **`dismissRetry(sid)`** `retention.ts:703`; dispatched `http.ts:2677`. `await listScopedWindows` (:720) then per-window `await killWindowVerified` (:733) with stale/gone handling.
- **Failure dialect:** pure control results; no throws for expected outcomes.

### 1I. `armUnsupervised()` — POST `/api/spawn/arm-unsupervised`
- **Entry:** `spawns.ts:545` region (returned at `:3708`); dispatched `http.ts:2394-2402`, **legacy-only, SYNCHRONOUS** — mints a token, `json(res,200,{arm_token})` (:2400). No Promise. Trivial slice (mirrors `questionsDismissWorkflow` sync shape).

### 1J. P1 maintenance ownership (the admission gate + join point)
- **Interfaces:** `SpawnMaintenance` (`spawns.ts:278`), `SpawnLifecycle` (`:288`).
- **State/ops:** `scheduledTasks` Set, `inFlightMaintenance` Set, `maintenancePhase` (`:424-543`); `runMaintenance` (`:439`), `scheduleMaintenance` (`:459`, unref'd timers, resolve-`cancelled()`-on-quiesce), `quiesceMaintenance` (`:519`), `closeMaintenance` (`:527`, while-loop awaiting in-flight).
- **Owned wrappers:** `ownedSpawn/ownedRevive/ownedAdoptSession/ownedEnableRemote/ownedSpawnKill` (`spawns.ts:3676-3690`), each `spawnMaintenance.run(fn) ?? Promise.resolve(shuttingDown())` where `shuttingDown()`=`503` (`:3672`).
- **Quiesce also aborts every provisioning op** (`spawns.ts:3656`).
- **This is a distinct seam from the P6.4 transport Effect.** Today it is imperative (Set + Promise tracking). Whether it stays imperative or becomes a scoped fiber pool is the sharpest open question (§5).

### 1K. Compensation & launch leaves (named-adapter candidates)
- `spawnCompensate` (`spawns.ts:1011`): awaits `killWindowVerified` + `execFileP git worktree remove/prune` + `fs.rmSync`.
- `launchPane` (`spawns.ts:1084`): `signal.aborted` checks (`:1100,:1209,:1214`), `launchOverride` callback vs tmux `ensureSession`+`newWindow`, status flip (`:1226`). Carries `signal?: AbortSignal` in `LaunchPaneArgs` (`:232`).

---

## §2 — THE TARGET SHAPE per path

**Uniform target for every request path (1C–1H):** an `Effect<ControlWire, never, never>`. Expected outcomes stay **`ControlWire` success data** (`http-workflows/control.ts:72`), never a typed error — the port's error channel does not widen and `mapEffectRouteExit` needs no new case (`control.ts:8-14`). A genuine throw becomes a **die** (transport defect arm → `500 {err:'internal'}`), exactly as `controlAsyncWorkflow` already specifies (`control.ts:43-59`). **`R = never`; no `yield* Store`** — SQLite (`q.*`), tmux, git, fs stay as `ctx` closures/capability params, because the request path is not the root fiber (`retention.ts:10-15`).

**Family pattern that applies:** the **P6.4 HTTP-CAPABILITY** pattern (control.ts), *not* the P8.6 root-yield pattern. The transport boundary already exists and is correct: `settleControlAsyncRoute` (`http.ts:1207`) with start-once + JOIN-on-interrupt, and `controlAsyncWorkflow` (`control.ts:96`). **P9.1 does not rebuild the transport.** It converts the *core orchestration* currently supplied as the native `run: () => Promise<ControlWire>` capability (`control.ts:84`) into an Effect.

**Boundary placement (the mechanical target):** each core function (`spawnKill`, `revive`, `adoptSession`, `enableRemote`, `dismissSession`, `dismissRetry`, and eventually `spawn`) becomes `…Effect(args): Effect<ControlWire, never, never>`. The `owned*` wrapper at the `createSpawns` return keeps P1 ownership and discharges the Effect to the Promise the transport still joins:
```
ownedSpawnKill = (id, force) =>
  spawnMaintenance.run(() => Effect.runPromise(spawnKillEffect(id, force)))
    ?? Promise.resolve(shuttingDown())
```
The `Effect.runPromise` here is the **named compatibility adapter** the exit gate permits ("remaining Promises are native callback/Response values inside named adapters"); it is inventoried, one per path. Whether to instead push the Effect all the way into the transport `run` capability (collapsing `runPromise`) is **Open Question Q1** — it touches `settleControlAsyncRoute`'s start-once/join contract and is likely out of P9.1 scope.

**Where typed errors go:** nowhere new. Expected failures = `ControlWire`. The only Effect-internal error surface is the failure-compensation flow (plan-claim release, `spawnCompensate`), which must remain **effectful undo that runs on both the failure and interrupt exits** — the natural fit is `Effect.onExit`/`Effect.ensuring` replacing the imperative `try/finally` + `wrapSpawnFailure` (`spawns.ts:1483`, `:1900`). The `releasePlanClaim` idempotence (guarded by `planClaim` non-null and via-match, `spawns.ts:1465-1476`) is preserved verbatim as the ensuring body.

**What stays a named native adapter (per P9 exit text):**
- The **detached clone chain** (1B): AbortController + `provisioningOps` + `provisioningDone` stays a bridge. `cloneRepo`/`materializeBranch`/`finishMaterialization`/`launchPane` stay behind `execFileP`/tmux adapters. Converting AbortController→`Fiber.interrupt` is deferred (migration-plan note; §4-D3; Q2).
- **tmux/git/fs leaves:** `killWindowVerified`, `listScopedWindows`, `capturePane`, `execFileP`, `fs.rmSync` — wrapped in `Effect.promise`/`Effect.tryPromise` at the leaf, or left as awaited calls inside a single `Effect.tryPromise` around the coarse operation (the retention.ts "one coarse operation, never per-statement" rule, `retention.ts:16-23`).
- **The unref'd harvest timers** (`delayedRemoteHarvest`, `spawnMaintenance.schedule`) — stay timer adapters; not orchestration.
- **`Effect.runPromise` at the owned* boundary** — one per converted path.

**The sync atomic blocks stay sync inside the Effect** (no `yield*` between claim and launch): BUG-040 claim (`spawns.ts:1441`), `claimTarget` (`:1555`), `card()` (`derive.ts:605`), custody lease check-and-set (`derive.ts:1236-1247`), and the dismiss atomic block (`retention.ts:556-573`). These run inside a single `Effect.sync`/`Effect.try` thunk — never suspended (`retention.ts:25-30`).

---

## §3 — SUB-SLICE PLAN

Ordered, commit-sized, characterization-first. **Rule:** no path is converted until the CHARACTERIZATION GAPS below it are closed by a *new* test that pins the exact bytes/timing of the legacy path FIRST. Each slice lands gate-green, Fable-reviewed; rollback = the legacy handler + a per-slice constructor flag.

### Slice 0 — `armUnsupervised` (sync warm-up)
- **Convert:** `/api/spawn/arm-unsupervised` to a `controlSyncWorkflow`-shaped Effect + wire an `effectRoutes` branch at `http.ts:2394`.
- **Characterization:** `tests/arm-gate.test.ts` (pins token mint/echo). **Gap:** none material; add one transport isolation test mirroring `questionsDismissWorkflow` in `tests/effect/http-workflow-control.test.ts`.
- **Blast radius:** one sync route. **Rollback:** `effectRoutes` unset → legacy `:2400`.
- **Adversarial review:** no (no launch/kill/provisioning).

### Slice 1 — Plan-claim compensation as `Effect.ensuring` (pure, no path moved)
- **Convert:** extract `releasePlanClaim`/`completePlanClaim`/`wrapSpawnFailure` (`spawns.ts:1465-1492`) into an ensuring-style combinator, still driven from the legacy `spawn`. This isolates the hardest correctness invariant (BUG-040 release-on-every-failure) *before* any control-flow rewrite.
- **Characterization:** `tests/plans.test.ts`, `tests/accept-plan-arm.test.ts`, `tests/accept-plan-mark.test.ts`, `tests/accept-plan-snapshot.test.ts`, `tests/accept-plan-isolation.test.ts`. **Gap:** add a test that forces a throw AFTER claim but BEFORE launch and asserts the plan reverts to `restoreStatus` with via-match (currently only implied).
- **Blast radius:** `spawn` internals only. **Rollback:** revert combinator.
- **Adversarial review:** **YES** (touches launch/claim atomicity).

### Slice 2 — `dismissSession` + `dismissRetry` (retention.ts) to Effect core
- **Why early:** cleanest failure dialect (pure control results, no throws), and the sync-atomic-block-then-awaits shape is the canonical demonstration of "sync stays sync, awaits become one coarse `Effect.tryPromise`."
- **Convert:** `dismissSessionEffect`/`dismissRetryEffect`; the atomic DB block stays a single `Effect.sync`; the window-kill loop with `alive()` resurrection-bail stays inside one coarse effect. Feed via the existing `run` capability (`http.ts:2652,:2681`).
- **Characterization:** `tests/dismiss.test.ts` (37k — covers offline-guard, active-spawn refusal, BUG-145 incomplete/retry, mid-await resurrection). **Gap:** add a test asserting the resurrection-bail fires when `archived_at` is cleared *between* two window kills (the `alive()` check at `retention.ts:579`), since Effect suspension points must not change that race.
- **Blast radius:** two routes; `retention.ts`. **Rollback:** per-path flag → legacy `dismissSession`.
- **Adversarial review:** **YES** (kill phase).

### Slice 3 — `spawnKill` to Effect core (incl. provisioning-cancel)
- **Convert:** `spawnKillEffect`; the `provisioningOps` abort + `Promise.race(5s)` join (`spawns.ts:2986-2998`) stays a **named adapter** wrapped in `Effect.tryPromise` (do NOT convert AbortController→interrupt here). H-R5 owner re-check stays sync.
- **Characterization:** `tests/dismiss.test.ts` + kill assertions in `tests/spawn.test.ts`/`tests/spawn-repo.test.ts`; `tests/p1-spawns-lifecycle.test.ts` (the clone-abort join, incl. the fixed flake). **Gap:** a test pinning the `202 {cancelling}` timeout-arm bytes (`spawns.ts:2998`) and that `op.done` resolution short-circuits the race.
- **Blast radius:** kill route + provisioning join. **Rollback:** per-path flag → legacy `:2502`.
- **Adversarial review:** **YES** (kill + provisioning cancel).

### Slice 4 — `enableRemote` to Effect core
- **Convert:** `enableRemoteEffect`; single-flight `remoteEnables`/`rcInputLocks` stay sync Maps; the 6s harvest race (`spawns.ts:2964`) stays a coarse adapter; keep `delayedRemoteHarvest` timers untouched.
- **Characterization:** rc coverage in `tests/spawn.test.ts`/`tests/spawn-repo.test.ts`. **Gap:** dedicated single-flight test (two concurrent `/rc` collapse to one harvest) — appears thin today; must be added.
- **Blast radius:** rc route. **Rollback:** per-path flag → legacy `:2712`.
- **Adversarial review:** borderline — **YES** (touches a launch-adjacent single-flight + detached harvest).

### Slice 5 — `revive` + `adoptSession` to Effect core, and `launchResume` factoring
- **Convert:** `reviveEffect`/`adoptSessionEffect`. **The shared `launchResume` tail (1D) is the crux:** it is called by these two request paths AND by root `resurrectSpawn`. Target: `launchResume` becomes an `Effect<…, never>` that is `R = never` (capabilities as params) so BOTH the request `runPromise` boundary AND the root path can run it without a Store dependency. `resurrectSpawn` (root, `spawns.ts:905`) already runs under the root runtime; it consumes the same effect via the root discharge with no `yield* Store` added (launch is capability-parameterized, not Store-backed).
- **Characterization:** `tests/revive.test.ts` (18k), `tests/adopt.test.ts` (31k), `tests/shell-spawn.test.ts`, and the liveness resurrection coverage in `tests/effect/db-workflows-spawn-liveness.test.ts`. **Gap:** a test asserting `resurrectSpawn`'s call into the converted `launchResume` produces **byte-identical** provisional-row insert + owner re-check (`spawns.ts:2416`) as the request path — the mixed-caller invariant.
- **Blast radius:** revive + adopt routes + the root resurrect caller. **Rollback:** per-path flags; `launchResume` keeps a legacy shim until both callers cut over.
- **Adversarial review:** **YES** (launch machinery + mixed root/request caller).

### Slice 6 — `spawn` to Effect core (POST `/api/spawn`), including the detached 202
- **Largest, last.** First bring `/api/spawn` under P6.4 transport (it is legacy-only today, `http.ts:2427`) — this is itself a discrete step (add the `effectRoutes` branch + a `settleControl*`-shaped settler, mirroring `settleControlAsyncRoute`). Then convert `spawnEffect`: the sync validation/claim/target shell becomes `Effect.sync`/`Effect.try`; the **detached clone chain (1B) stays behind the `provisioningOps` + AbortController bridge as a named adapter** (`Effect.tryPromise` around `spawnMaintenance.run(async…)`), returning `202` as `ControlWire` data.
- **Characterization:** `tests/spawn.test.ts` (60k), `tests/spawn-repo.test.ts` (46k), `tests/spawn-setup.test.ts`, `tests/spawn-unsupervised.test.ts`, `tests/spawn-repo-scratch-cleanup.test.ts` (H-R6 temp cleanup), `tests/worktrees.test.ts` (custody lease), `tests/p1-spawns-lifecycle.test.ts` (detached join/abort). **Gaps:** (a) the `500 spawnFailureReason` throw path (`http.ts:2479`) must be pinned as a **die→defect** mapping BEFORE conversion (redaction bytes matter); (b) a test that a spawn admitted-then-quiesced compensates via the pre-abort guard (`spawns.ts:1806-1824`) → `503`; (c) the `202`-then-detached-failure-tombstones path (failure after 202 lands in the detached catch, `http.ts:2475-2478`).
- **Blast radius:** the whole spawn route + detached provisioning. **Rollback:** `effectRoutes` unset → legacy `:2464`; the detached adapter is unchanged so rollback is transport-only.
- **Adversarial review:** **YES** (launch + clone + provisioning + claim).

### Slice 7 (optional / may defer past P9.1) — P1 ownership + AbortController→fiber
- Convert `spawnMaintenance` ownership to a scoped fiber pool and the detached AbortController to `Fiber.interrupt`. **Explicitly gated** by the migration-plan note ("dedicated survival & signal tests" first). Likely a separate work-package; see Q2.

---

## §4 — DANGER NOTES (byte-frozen requirements)

**D1 — Process-spawn invariants (§2 of the completion map) are untouched by Effect.** `launchPane` (`spawns.ts:1084`) and its tmux adapter build argv arrays, env (`env -u`/keep gateway handling), cwd, and the `--` end-of-options prompt-injection defense. Effect conversion must not reorder or re-quote any of it. **Requirement:** every converted slice that reaches `launchPane` asserts argv/env/cwd byte-identity against a captured legacy fixture. The launch adapter stays a native adapter; only the *decision* to call it moves into an Effect.

**D2 — Sync atomic blocks must not gain a suspension point.** BUG-040 claim (`spawns.ts:1441`), `claimTarget` (`:1555`), `card()` (`derive.ts:605`), custody lease check-and-set (`derive.ts:1236`), and the dismiss atomic block (`retention.ts:556-573`) each depend on running in **one JS turn with no await/yield between check and mutate**. In Effect these MUST be a single `Effect.sync`/`Effect.try` thunk — a `yield*` inserted mid-block re-opens the exact race each comment forbids (`derive.ts:1224`, `retention.ts:556`). Byte-frozen: no `yield*` inside these thunks.

**D3 — Detached-clone cancellation is AbortController, not interruption — keep it that way in P9.1.** `controller.signal.aborted` is re-checked at `spawns.ts:1845,:1859,:1861` and passed into `cloneRepo`/`materializeBranch`/`launchPane` via `signal`. `spawnKill` (`:2988`) and `quiesceSpawns` (`:3656`) drive it. Swapping to `Fiber.interrupt` changes *when* in-flight `execFileP` git processes actually stop and how `spawnCompensate` runs on the cancel exit — the migration-plan bars this until dedicated signal tests exist. **Requirement:** the AbortController + `provisioningOps` + `provisioningDone` bridge is byte-frozen through Slice 6; interruption conversion is Slice 7 / Q2.

**D4 — Compensation must run on the interrupt exit, not only the failure exit.** Today the detached `finally` (`spawns.ts:1900`) and the `catch` (`:1888`) run `spawnCompensate` + releases regardless of cancellation. If any converted path uses a forked/interruptible Effect, the compensation must move to `Effect.onExit`/`ensuring` so an interrupt-during-shutdown still compensates and releases the clone slot/target/claim. A naive `Effect.map` chain would drop compensation on interrupt.

**D5 — Mixed caller of `launchResume` (Slice 5).** `resurrectSpawn` (root) and `revive`/`adopt` (request) share it. Requirement: `launchResume` stays `R = never` (capability-parameterized) so it is dischargeable from both the root runtime and the request `runPromise` boundary **without** a `yield* Store`. Adding a Store dependency here would either break the request path (`R ≠ never`) or force a second discharge — both violate convention (`retention.ts:10-15`).

**D6 — The `500 spawnFailureReason` redaction path is contractual.** `http.ts:2479` maps an escaped throw to a single redacted line (`spawnFailureReason`, `spawns.ts:322`, using `redactGitText`/`scrubUrlCredentials`). When `spawn` becomes an Effect, a genuine throw must land as a **die** whose transport defect arm reproduces those exact redacted bytes — not a raw stack, not `{err:'internal'}`. Pin before converting (Slice 6 Gap-a).

**D7 — Quiesce/close ordering.** `closeMaintenance` (`spawns.ts:527`) while-loops awaiting `inFlightMaintenance`; the owned wrappers register there. If ownership stays imperative (recommended for P9.1), the `runPromise` returned by each owned* must still be the thing `inFlightMaintenance` tracks, or `closeClients` stops waiting for in-flight spawns/kills — a shutdown-correctness regression. Byte-frozen: the tracked Promise identity.

---

## §5 — OPEN QUESTIONS (need the orchestrator's ruling)

**Q1 — Where does the Effect boundary stop: transport `run` capability, or a `runPromise` adapter inside `createSpawns`?** Recommendation: stop at a per-path `Effect.runPromise` inside the `owned*` wrappers (keeps `settleControlAsyncRoute`'s start-once/join contract untouched, `http.ts:1207`); push into the transport `run` capability only if the orchestrator wants the native `Promise<ControlWire>` capability (`control.ts:84`) eliminated in P9.1. The latter is more invasive and re-opens the P6.4 transport seam.

**Q2 — Is the P1-ownership + AbortController→fiber conversion (Slice 7) in P9.1, or a separate work-package?** The migration-plan note gates it on dedicated survival/signal tests that don't exist yet. Recommendation: **defer** — P9.1 ships Slices 0–6 with ownership imperative and the detached bridge frozen; the exit gate's "every compatibility bridge is inventoried" is satisfied by naming `provisioningOps`/AbortController/`runPromise`/`spawnMaintenance.run` as the four inventoried bridges.

**Q3 — Does `/api/spawn` get its P6.4 transport wiring as part of Slice 6, or as a prerequisite slice of its own?** It is the only request path still legacy-only (`http.ts:2427`, no `effectRoutes` branch). It needs a settler (there is no `settleControlAsyncRoute` call for it today). Recommendation: split it — **Slice 6a** = bring `/api/spawn` under a `settleControl*` settler with the legacy handler as fall-through (no core change); **Slice 6b** = convert `spawnEffect`. This keeps the highest-blast-radius change reviewable in two green steps.

**Q4 — Adversarial-review scope confirmation.** Per the standing expectation, Slices 1, 2, 3, 5, 6 (all launch/kill/provisioning) are review-mandatory; Slice 4 (rc) is borderline-YES. Confirm whether Slice 0 (arm, sync) and any pure-refactor step may skip adversarial review to conserve reviewer budget.

---

**Inventory of the four compatibility bridges P9.1 leaves standing** (for the exit-gate ledger): (1) `Effect.runPromise` at each `owned*` boundary; (2) `spawnMaintenance.run` P1 ownership tracking (`spawns.ts:439`); (3) the `provisioningOps`/AbortController/`provisioningDone` detached-clone join (`spawns.ts:1826-1936`, `:2986-2998`, `:3656`); (4) the native tmux/git/fs leaf adapters (`killWindowVerified`, `execFileP`, `fs.rmSync`, `listScopedWindows`). All four are named, and none is a `yield* Store` violation because every request path stays `R = never`.

---

## §6 — ORCHESTRATOR ADJUDICATION (2026-08-23, design accepted as the P9.1 execution spec)

- **Q1 RULED:** the Effect boundary stops at per-path runners inside the `owned*` wrappers. Use the repo-sanctioned runner form (`Effect.runPromiseWith(Context.empty())`; bare `Effect.runPromise` is deny-listed). The fiber is deliberately unsupervised: the returned Promise is the object `spawnMaintenance.run` tracks and the transport joins, preserving D7's tracked-identity requirement byte-for-byte. The transport `run` capability keeps its native `Promise<ControlWire>` shape; eliminating it is out of P9.1 scope.
- **Q2 RULED:** Slice 7 is deferred out of P9.1. The four named bridges (per-path runner, `spawnMaintenance.run`, the `provisioningOps`/AbortController/`provisioningDone` join, the tmux/git/fs leaf adapters) constitute the inventoried compatibility bridges for the P9 exit gate.
- **Q3 RULED:** split confirmed — Slice 6a wires `/api/spawn` under a SPAWN-SPECIFIC settler (its defect arm must reproduce the redacted `spawnFailureReason` bytes per D6, never the control group's `{err:'internal'}`), legacy fall-through retained; Slice 6b converts `spawnEffect`.
- **Q4 RULED:** Slice 0 may skip the subagent adversarial review (orchestrator line review). Slices 1, 2, 3, 4, 5, 6a, 6b are adversarial-review-mandatory.
- Danger notes D1–D7 are binding, byte-frozen requirements on every slice brief.

## §7 — SLICE 1 REVIEW RECORD (2026-08-23)

Slice 1 landed with an independent adversarial verdict of SHIP-WITH-NITS (no
blockers; the twenty-row disposition table verified row-by-row; handOff
atomicity proven; the release-to-finally move shown unobservable through the
60 ms broadcast coalesce). Two nits, both scheduled:

1. **BINDING PRE-SLICE-6 REQUIREMENT:** the three repo-mode validation 400s
   (worktree-in-repo, branch-required, branch_mode-invalid) are deliberate
   leak-fixes now released by the structural finally, but no test pins them.
   Before Slice 6 converts spawn's control flow, add a characterization case
   (plan_id + repo body hitting each 400 → plan reverts to restoreStatus), and
   the Slice 6 author must keep those 400s INSIDE the ensuring body — pulling
   them into the claim prefix would silently re-open the leak.
2. Accepted: a pathologically throwing tick/onMutate can emit a duplicate feed
   line (never a duplicate DB write — the via-match WHERE makes the second
   UPDATE changes=0). Not fixable without breaking byte-order parity with the
   legacy releasePlanClaim.
