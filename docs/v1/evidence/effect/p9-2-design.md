# P9.2 Design — repo/worktree/git async shells become Effects

> **Stamped into the repo 2026-08-24.** Copied verbatim from the adjudicated draft `/tmp/fd-effect/p9-2-design-draft.md`; §6 carries the orchestrator's rulings. Ready to execute on the main tree now that P9.1 has closed.

**Scope:** `src/daemon/repos.ts` + `src/daemon/worktrees.ts` and their three HTTP routes:
`GET /api/worktrees`, `POST /api/worktrees/remove`, `POST /api/repos/preflight`.

**Status:** DRAFT for orchestrator review. Mirrors the `p9-1-design.md` §1–§5 skeleton.

**Worktree / branch:** `/tmp/fd-wt-p9-2-design` @ `tmp/p9-2-design` (HEAD `0dfce719`).

**Established pattern this draft builds on (all landed pre-P9.2):**
- HTTP-CAPABILITY pattern: cores/workflows are `Effect.Effect<{status,body}, never, never>` — R=never,
  E=never, no `yield* Store`. Expected outcomes (400/409/500/200/504…) are `{status, body}` **DATA**.
  Only a genuine throw becomes a `die` → the transport's defect arm.
- Two runners (P8/P9.1): **runControlDetached** = `Effect.runPromiseWith(Context.empty())` in the ingress
  supervisor, threaded into the CORE via `createCore(db, { …, runControlDetached: ingress.runControlDetached })`
  (`program.ts:791`), reaching every domain factory through `ctx.runControlDetached` (`derive.ts:1124`).
  **runRequest** = `ingress.runPromiseExit` = `httpServer.service.runRequest`, threaded into the TRANSPORT
  via `http.installEffectRoutes({ runRequest, … })` (`program.ts:875`).
- The **only** legitimate `run*` call sites are inside the ingress supervisor
  (`tests/effect/ingress-supervisor.test.ts` pins `supervisor.runPromise` / `runFork` / `runPromiseExit`).
  **This design adds NO new `run*` call site** — it reuses `runRequest` and/or `ctx.runControlDetached`.

---

## §1 INVENTORY — every async leg

Legs are lettered. Line numbers are current (`0dfce719`). "awaits" column: `git` = child-process git exec
(`execFileP`), `FS` = filesystem, `DB` = sqlite. Sync atomic blocks that border a leg are called out inline.

### 1.A — `worktrees.ts` async legs (module `createWorktrees(ctx)`; `derive.ts:1256`)

| Leg | Symbol / line | Awaits | Notes |
|-----|---------------|--------|-------|
| W-a | `gitCommonDir(dir)` `:103` | git `rev-parse --git-common-dir` | pure probe; used by ownership + inspect |
| W-b | `repoOwnsWorktree(repoRoot, wt)` `:127` | git (via W-a ×2) | **BUG-059 guard** — data-loss gate; returns bool, throws only on unexpected |
| W-c | `refreshRemoteKnowledge(...)` `:214` | git `fetch`/`remote` | best-effort; swallows failures |
| W-d | `inspectWorktree(row)` `:232` | `Promise.all` git probes ×5 → then ×3 | read-only inspector; per-worktree snapshot |
| W-e | `worktrees()` `:336` | `mapLimit(rows, 4, inspectWorktree)` | **GET route core**; concurrency-4 fan-out over W-d; returns `{ok:true, worktrees:[…]}` |
| W-f | `branchTipOid(...)` `:393` | git `rev-parse` | CAS witness (`inspected_tip`) for branch deletion |
| W-g | `pruneWorktreeMetadata(...)` `:402` | git `worktree prune` | metadata cleanup |
| W-h | `removeWorktree(ev)` `:416` | **deeply interleaved** — see below | **remove route core** |

**W-h `removeWorktree` — the await/sync interleave (the hard leg):**

```
:431  await acquireWorktreePathLock(worktreePath)      # BUG-060: lock HELD for the whole removal
:~435 sync gates (arg validation → {status:400/409, body} DATA on failure)
:443  await inspectWorktree(...)                        # git (W-d)
:450  await branchTipOid(...) → inspected_tip           # git (W-f)  CAS witness captured here
:~455 sync verdict gate (dirty / ahead → {status:409, body} DATA)
:473  await execFileP('git', ['rev-parse','--show-toplevel'], …)   # git
:488  await repoOwnsWorktree(...)                        # git (W-b)  BUG-059 data-loss gate → 409 DATA
:510  sync liveness re-check (session still ended?)      # in-memory
:526  const releaseCustody = claimWorktreeCustody?.(worktreePath,'remove')   # *** NO await: in-memory check-and-set ***
:~530 destructive tail:  chmodSync → git worktree remove → rmSync fallback → pruneWorktreeMetadata (W-g)
                          → CAS branch -D / update-ref against inspected_tip (W-f witness)
:665  final liveness gate (session still ended?)         # in-memory → {status:409, body} DATA if revived
:696  db.exec('BEGIN IMMEDIATE')                         # *** purgeRows atomic block, :696–729 ***
:~700   purgeRows():  expireMailForSession + expireQuestionsForSession  FIRST
                      → deleteWorktreeSpawns → deleteEndedSession
:~725   COMMIT
        catch → ROLLBACK + return {status:500, body:{ok:false, reason:`could not purge worktree rows: ${detail}`}}
:~730 releaseCustody()  (finally)
      return {status:200, body:{ok:true, …}}
```

Every expected verdict is returned as `{status,body}` DATA (400 / 409 ×many / 500 / 200). Only an unexpected
throw rejects the Promise → the route `.catch` maps it to a fail-generic 500.

### 1.B — `repos.ts` async legs (module `createRepos(ctx)`; `derive.ts:1194`)

| Leg | Symbol / line | Awaits | Notes |
|-----|---------------|--------|-------|
| R-a | `resolveTarget(body)` `:956` | git / FS (repos_dir inspect) | throws `namedError(status,msg)` on failure; **no DB, no CAS** |
| R-b | `probeRepoAccess(origin)` `:741` | git `ls-remote --  <origin> HEAD` | `accessCache` TTL check (sync) → exec → `!ok` builds `RepoAccessHelp` (504/409) |
| R-c | `preflightRepo(body)` `:763` | R-a then (clone-mode only) R-b | **preflight route core** |

**R-c `preflightRepo` — control flow (the simplest of the three; no atomic DB, no in-memory CAS):**

```
:769  try { target = await resolveTarget(body) }            # R-a
:772  catch → return { status: errStatus(err) ?? 400, body:{ok:false, reason: errMessage(err)} }   # DATA
      if target.mode === 'local' → return {status:200, body:{ok:true, mode:'local'}}                # DATA
      access = await probeRepoAccess(target.origin_url)      # R-b
      ok  → return {status:200, body:{ok:true, mode:'clone', provider, transport}}                  # DATA
      !ok → return {status:access.status, body:{ok:false, reason, git_access}}                       # DATA (504/409)
```

`preflightRepo` already returns ALL outcomes as `{status,body}` DATA; a throw only escapes on a truly
unexpected fault → route `.catch` → 500.

### 1.C — Failure dialect on the wire (exact bytes)

Source: `http.ts`. These are the **byte contracts** the design must preserve.

**GET `/api/worktrees`** (`http.ts:2194–2208`) — **fail-SOFT, never 500:**
```js
core.worktrees()
  .then(out => json(res, 200, out))
  .catch(err => { console.error('fleetd worktree inspector error:', err);
                  json(res, 200, { ok:true, worktrees:[] }); })
```
- success → `200` + `worktrees()` value (`{ok:true, worktrees:[…]}`).
- **any rejection → `200` `{ok:true, worktrees:[]}`** + log `fleetd worktree inspector error:`.

**POST `/api/worktrees/remove`** (`http.ts:2437–2450`):
```js
core.removeWorktree(ev)
  .then(out => json(res, out.status, out.body))
  .catch(err => { console.error('fleetd worktree removal error:', err);
                  json(res, 500, { ok:false, reason:'internal' }); })
```
- resolve → `out.status` + `out.body` verbatim (200 / 400 / 409×many / 500-purge).
- reject → `500` `{ok:false, reason:'internal'}` + log `fleetd worktree removal error:`.

**POST `/api/repos/preflight`** (`http.ts:2526–2549`) — **body validation FIRST:**
```js
const preflightError = repoPreflightBodyError(body);
if (preflightError) return json(res, 400, { ok:false, reason: preflightError });
core.preflightRepo({ repo, repo_host, repo_transport, repo_org })
  .then(out => json(res, out.status, out.body))
  .catch(err => { console.error('fleetd repo preflight error:', err);
                  json(res, 500, { ok:false, reason:'Git access check failed internally' }); })
```
- pre-core validation reject → `400` `{ok:false, reason: preflightError}` (**stays in transport, never enters Effect**).
- resolve → `out.status` + `out.body` verbatim (200 clone/local, 504/409 access).
- reject → `500` `{ok:false, reason:'Git access check failed internally'}` (**DISTINCT** from remove's `'internal'`)
  + log `fleetd repo preflight error:`.

**Byte-diff to freeze:** remove's generic-500 body = `{ok:false,reason:'internal'}`; preflight's =
`{ok:false,reason:'Git access check failed internally'}`. The `settleControlAsyncRoute` /
`controlAsyncWorkflow` fold hardcodes `{ok:false,reason:'internal'}` — so **remove can reuse it, preflight cannot.**

### 1.D — Callers (HTTP-only vs shared)

| Core | HTTP route | Other callers | Shared-invariant flag |
|------|-----------|---------------|-----------------------|
| `worktrees()` (W-e) | GET /api/worktrees | direct-drive tests only | HTTP-only in prod |
| `removeWorktree` (W-h) | POST /api/worktrees/remove | direct-drive tests only | **shares `claimWorktreeCustody` (ctx.claimWorktreeCustody, `derive.ts:1244`) with `revive`** — custody lease is the cross-flow contract |
| `preflightRepo` (R-c) | POST /api/repos/preflight | direct-drive tests only | HTTP-only in prod |
| `probeRepoAccess` (R-b) | (via preflight) | **also used by spawn/clone paths** (accessCache shared) | cache is process-global |

**Mixed-caller danger:** `claimWorktreeCustody` is an in-memory check-and-set shared by `removeWorktree` AND
`revive`. The no-await invariant (§4) is what makes it a correct mutex; any conversion that inserts an await
between the liveness re-check (`:510`) and the claim (`:526`) breaks BOTH flows. `probeRepoAccess`'s
`accessCache` is process-global and touched by non-HTTP clone paths — its TTL/entry shape must not be perturbed.

---

## §2 TARGET SHAPES

### 2.0 — How the runner reaches these modules (THE key design question — answered)

**Answer: it already does. No new plumbing, no new `run*` site.**

- `program.ts:791` — `core = createCore(db, { port, version, runControlDetached: ingress.runControlDetached })`.
- `derive.ts:1124` — `const ctx = { …, runControlDetached, … }` is assembled **before** the domain factories run:
  `createRepos(ctx)` (`:1194`), `createWorktrees(ctx)` (`:1256`), each via `Object.assign(ctx, create*(ctx))`.
  The comment at `derive.ts:1122–1123` documents the intent: threaded so cores discharge through the ingress
  runner; `undefined` in standalone factory tests → legacy fallback.
- So `repos.ts` / `worktrees.ts` **already receive `ctx.runControlDetached`** exactly as `createSpawns` /
  `createRetention` do — they simply **don't consume it yet.**
- `program.ts:875` — `installEffectRoutes({ runRequest: (op,eff)=>httpServer.service.runRequest(op,eff), … })`
  already threads **runRequest** into the transport. `HttpEffectRoutes` currently has **no** worktrees /
  remove / preflight builders — those must be **added** (transport-side), but the runner is already present.

**Two ways to consume it (recommendation in §5 OQ-1):**

- **(A) Transport-only** (spawn Slice 6a / `controlAsyncWorkflow` style): wrap the **existing legacy core
  Promise** in an Effect workflow discharged under **runRequest**. ZERO edits to `repos.ts` / `worktrees.ts`.
  Byte-frozen invariants (custody lease, purgeRows, git argv) are preserved *because the destructive body is
  never touched.* This is the safest slice-1 and the recommended default.
- **(B) Core conversion** (dismiss Slice 2 style): build `removeWorktreeEffect` etc. discharged via
  `ctx.runControlDetached`, with the sync gates lifted into `Effect.sync` and the async tail in
  `Effect.promise`. For `preflightRepo` this is clean. For `removeWorktree` it is **not** — the first await
  (path lock, `:431`) sits right after one sync gate and everything downstream is await-interleaved, so its
  Effect degenerates to a single coarse `Effect.promise(() => legacyBody())` — i.e. no real gate/atomic
  separation is bought, and the custody lease + purgeRows stay inside the verbatim body regardless.

### 2.1 — GET `/api/worktrees` (READ, fail-soft)

- **Core shape:** `worktreesSnapshotEffect: Effect.Effect<{ok:true, worktrees:…[]}, never, never>` that folds
  `worktrees()` rejection **into the fail-soft success wire** `{ok:true, worktrees:[]}` (mirroring how
  `controlAsyncWorkflow` folds rejection into a data outcome). Because the route never emits 500, this is NOT
  a `settleEffectSnapshotRoute` client (that settler's defect arm is 500) — it needs a **fail-soft read
  settler** whose *every* non-success arm still renders `200 {ok:true, worktrees:[]}`.
- **Settler:** NEW `settleEffectSoftReadRoute` (or reuse a read settler configured so quiesce + defect both
  map to the fail-soft wire). The fold-in-workflow approach is cleaner: keep the settler trivial (always 200)
  and do the `.catch → {ok:true,worktrees:[]}` inside the workflow, preserving the log
  `fleetd worktree inspector error:` on the caught path.
- **Boundary stop:** at `installEffectRoutes` — the transport renders `200`; the workflow owns the fail-soft
  fold and the console.error.

### 2.2 — POST `/api/worktrees/remove` (ASYNC mutating)

- **Core shape:** `removeWorktreeWorkflow` = `controlAsyncWorkflow`-shaped:
  `Effect.sync(() => caps.run()).pipe(Effect.flatMap(pending => Effect.promise(() => pending.then(out => out,
  err => { caps.onError(err); return {status:500, body:{ok:false, reason:'internal'}}; }))))`.
  `caps.run()` = `() => core.removeWorktree(ev)` (the legacy Promise), `caps.onError` = the
  `console.error('fleetd worktree removal error:', …)` log.
- **Settler:** `settleControlAsyncRoute` / `settleEffectAsyncMutatingRoute` with `startOnce` JOIN-on-interrupt
  semantics — because `removeWorktree` is a real async mutation with a destructive tail that must not be
  double-started. Its generic-500 body `{ok:false,reason:'internal'}` **matches the fold verbatim**, so the
  existing controlAsync fold is byte-correct here.
- **Boundary stop:** the Effect owns run-once + fold-to-internal-500; the destructive body (lock, custody,
  purgeRows, CAS) stays **inside the untouched legacy `removeWorktree`** under option A.

### 2.3 — POST `/api/repos/preflight` (ASYNC mutating-ish; distinct 500 body)

- **Core shape:** a **dedicated** `preflightRepoWorkflow` = same async fold shape, but the rejection arm
  returns `{status:500, body:{ok:false, reason:'Git access check failed internally'}}` (NOT `'internal'`),
  and `caps.onError` logs `fleetd repo preflight error:`. **Cannot reuse `controlAsyncWorkflow`** because that
  hardcodes the `'internal'` body.
- **Body validation stays in transport:** `repoPreflightBodyError(body)` → `400` runs **before** the workflow
  is entered (it never reaches the Effect); preserve this ordering exactly.
- **Settler:** `settleControlAsyncRoute` variant parameterized with the preflight 500 body + log prefix, OR a
  small dedicated `settleEffectPreflightRoute`. Recommend parameterizing the async settler over
  `(generic500Body, errorPrefix)` so remove and preflight share the settler and differ only by config.
- **Boundary stop:** transport does body-validation + status rendering; workflow owns the async fold with the
  preflight-specific 500 bytes.

### 2.4 — New `HttpEffectRoutes` builders required

`HttpEffectRoutes` today has **no** worktrees / remove / preflight members. Add three (names illustrative):
`worktreesSnapshot`, `worktreeRemove`, `repoPreflight`. Each is `null` until `installEffectRoutes` injects it
→ **null = per-route rollback seam** (legacy `.then/.catch` handler stays reachable, exactly like the existing
`effectRoutes: … | null` convention).

---

## §3 SLICE PLAN

Each slice: minimal green step + characterization inventory (**run read-only to confirm** — results below) +
GAPS to pin first + blast radius + rollback seam + review requirement.

**Characterization coverage confirmed GREEN (read-only, this session):**
- `tests/repos.test.ts` + `tests/repo-identity.test.ts` + `tests/worktree-chmod-symlink.test.ts` →
  **49 tests, 0 fail** (repos core + repo identity + chmod/symlink).
- `tests/worktrees.test.ts` → **20 tests, 0 fail** (GET + remove **wire** via HTTP 146–348; core via
  direct-drive `createWorktrees` 448+).
- `tests/derive-audit-reliability.test.ts` → **33 tests, 0 fail** (removeWorktree data-loss / atomicity
  guards H-R1, R2–8, BUG-059/060 via `createCore` 371).
- Preflight **wire** additionally pinned by `tests/spawn-repo.test.ts` (`postJson` 535/556) — **NOT run this
  session** (GAP-1 below).

### Slice 0 — Pin gaps first (tests only; no source change)

- **GAP-1:** confirm `tests/spawn-repo.test.ts` preflight wire assertions cover **both** the 200 clone/local
  bodies AND the 504/409 access bodies AND the distinct 500 `'Git access check failed internally'`. Run
  read-only; if the 500 body is not pinned, add a characterization test **before** touching the route.
- **GAP-2:** confirm a test pins GET `/api/worktrees` **fail-soft 200 on core rejection** (not just the happy
  path). `worktrees.test.ts` pins the happy wire; verify the rejection→`{ok:true,worktrees:[]}` arm is pinned.
  If not, pin it first — it is the whole point of the read settler.
- **GAP-3:** confirm the remove generic-500 (`{ok:false,reason:'internal'}` on unexpected throw) is pinned
  distinctly from the purge-500 (`could not purge worktree rows: …`). Both are 500 but different bodies.
- Blast radius: none (tests). Rollback: n/a. Review: none.

### Slice 1 — GET `/api/worktrees` → Effect (READ, lowest risk)

- Add `worktreesSnapshot` builder + fail-soft read workflow (fold rejection → `{ok:true,worktrees:[]}`,
  keep `console.error('fleetd worktree inspector error:')`). Wire via `installEffectRoutes`; keep legacy
  `.then/.catch` behind `effectRoutes == null`.
- Characterization: `worktrees.test.ts` (GET wire) + GAP-2 pin.
- Blast radius: read-only route; no DB, no destructive path. Rollback seam: null builder → legacy handler.
- Review: LOW (read; fail-soft byte contract is the only thing to get right).

### Slice 2 — POST `/api/repos/preflight` → Effect (async, no DB/CAS)

- Add `repoPreflight` builder + dedicated `preflightRepoWorkflow` (distinct 500 body + `fleetd repo
  preflight error:` log). Keep `repoPreflightBodyError → 400` in transport, before the workflow.
- Characterization: `spawn-repo.test.ts` preflight wire + `repos.test.ts` core + GAP-1 pin.
- Blast radius: preflight only; `probeRepoAccess.accessCache` is read but **not** re-shaped (option A leaves
  `preflightRepo` body untouched). Rollback: null builder → legacy handler.
- Review: MEDIUM (distinct 500 bytes; body-validation ordering; accessCache untouched).

### Slice 3 — POST `/api/worktrees/remove` → Effect (async mutating; HIGHEST risk)

- Add `worktreeRemove` builder + `removeWorktreeWorkflow` (controlAsync fold; `{ok:false,reason:'internal'}`
  matches). **Option A only for slice 3** — wrap the untouched legacy `removeWorktree` Promise; do NOT lift
  gates. This keeps the custody lease no-await invariant and the purgeRows atomic block **verbatim inside the
  legacy body**, which is the whole safety argument.
- Characterization: `worktrees.test.ts` (remove wire) + `derive-audit-reliability.test.ts` (data-loss /
  atomicity guards) + GAP-3 pin.
- Blast radius: destructive removal path; shares `claimWorktreeCustody` with `revive`; touches purgeRows.
  Rollback seam: null builder → legacy handler (the legacy body is unchanged, so rollback is a one-line flip).
- Review: **HIGH** — must diff against P9.1's landed custody/revive code; must confirm start-once JOIN
  semantics don't double-start the destructive tail; must confirm the fold body is byte-identical.

**Recommended order:** Slice 0 → 1 → 2 → 3 (ascending risk; each independently revertable via null builder).

---

## §4 DANGER NOTES (byte-frozen requirements)

1. **purgeRows expiry-before-delete ordering** (`worktrees.ts:696–729`): inside `BEGIN IMMEDIATE`, the calls
   MUST stay `expireMailForSession` + `expireQuestionsForSession` **FIRST**, then `deleteWorktreeSpawns`, then
   `deleteEndedSession`, then `COMMIT`; `catch → ROLLBACK` + `{status:500, body:{ok:false,
   reason:\`could not purge worktree rows: ${detail}\`}}`. **Option A keeps this untouched** — do not lift it
   into an Effect.
2. **Custody lease no-await invariant** (`worktrees.ts:526`, `ctx.claimWorktreeCustody` @ `derive.ts:1244`):
   the in-memory check-and-set between the liveness re-check (`:510`) and the claim (`:526`) has **NO await**.
   Shared by `removeWorktree` AND `revive`. Any conversion inserting an await/yield here breaks the mutex for
   **both** flows. Option A never touches it.
3. **git argv/env must not be perturbed:** `GIT_TERMINAL_PROMPT=0`, `GCM_INTERACTIVE=Never`, `killTree:true`,
   timeouts/signals (`repos.ts:741` ls-remote; `worktrees.ts` probes). Credential redaction
   (`redactGitText` / `scrubUrlCredentials` / `originSecrets`, `repos.ts:98/106`) must remain on every error
   path. Option A leaves all of this inside the untouched cores.
4. **BUG-059 / BUG-060 / CAS:** `repoOwnsWorktree` data-loss gate (`:127/:488`); path lock held whole removal
   (`:431`, BUG-060); CAS branch deletion against `inspected_tip` witnessed at `:450` (W-f). Do not reorder
   the lock/inspect/witness/verdict sequence.
5. **Shared with P9.1's landed code:** the custody lease + `revive` sharing, `ctx.runControlDetached` threading
   (`derive.ts:1124`), and the `controlAsyncWorkflow` fold (`http-workflows/control.ts`) all shipped in
   P8/P9.1. Slice 3 review MUST diff against them — especially that reusing the fold does not change the
   `'internal'` body and that JOIN-on-interrupt does not double-start the destructive tail.
6. **Two distinct generic-500 bodies:** remove = `{ok:false,reason:'internal'}`; preflight =
   `{ok:false,reason:'Git access check failed internally'}`. Do not collapse them by sharing an unparameterized
   settler/workflow.
7. **GET fail-soft never-500:** GET `/api/worktrees` MUST emit `200 {ok:true,worktrees:[]}` on every failure
   arm. It is the one route where a defect must NOT surface as 500.
8. **Preflight body-validation stays in transport:** `repoPreflightBodyError → 400` runs before the Effect;
   it must not migrate into the workflow (else a 400 becomes reachable only after the runner spins up).

---

## §5 OPEN QUESTIONS (each with a recommendation)

**OQ-1 — Transport-only (A) vs core conversion (B)?**
*Recommendation: **transport-only (A) for all three routes**, at least for the first landing.* The runner
already reaches the cores, so B buys nothing for `removeWorktree` (its first await is the path lock — the
Effect degenerates to one coarse `Effect.promise(legacyBody)`), while A preserves every byte-frozen invariant
by construction (custody lease, purgeRows, git argv stay inside the untouched core). `preflightRepo` is the
only clean B candidate; still recommend A first for uniformity, then optionally lift `preflightRepo` gates in
a later, isolated slice if the orchestrator wants the core-level Effect surface.

**OQ-2 — Fail-soft GET: fold-in-workflow vs new settler?**
*Recommendation: fold the rejection inside the workflow* (`worktrees()` `.catch → {ok:true,worktrees:[]}` +
keep the log), and keep the settler a trivial always-200 renderer. Cleaner than a bespoke defect→soft-wire
settler and keeps the fail-soft contract in one readable place. (Confirm GAP-2 pins the rejection arm first.)

**OQ-3 — One parameterized async settler or two?**
*Recommendation: parameterize `settleControlAsyncRoute` over `(generic500Body, errorPrefix)`* so remove
(`'internal'` / `fleetd worktree removal error:`) and preflight (`'Git access check failed internally'` /
`fleetd repo preflight error:`) share the settler and differ only by config. Avoids two near-duplicate
settlers while keeping the two byte contracts distinct.

**OQ-4 — Slice granularity / ordering?**
*Recommendation: land Slice 0 (gap pins) → 1 (GET) → 2 (preflight) → 3 (remove) as four PRs*, ascending risk,
each revertable by nulling its `HttpEffectRoutes` builder. Do not bundle remove with the others — it is the
only HIGH-review slice and the only one touching the destructive/atomic path.

**OQ-5 — Should `HttpEffectRoutes` gain a route group toggle, or per-route null?**
*Recommendation: per-route null* (existing convention). Three independent builders, three independent rollback
seams; matches the current `effectRoutes: … | null` idiom and keeps blast radius per-route.

**OQ-6 — Confirm GAP-1 (preflight 500 body) is pinned before Slice 2.**
*Recommendation: run `tests/spawn-repo.test.ts` read-only and grep for `'Git access check failed
internally'`; if absent, add a characterization test first.* (Not run this session — flagged as the one
un-verified wire.)

---

## §6 — ORCHESTRATOR ADJUDICATION (Fable, 2026-08-23)

**OQ-1 RULED — hybrid, overruling transport-only-A:** each route slice does the transport wiring per §2 AND the P9.1-style *degenerate* core conversion: the core gains an Effect twin whose sync prefix holds ONLY what is genuinely sync-terminal today (zero-gate cores may be a bare coarse `Effect.promise` tail), the tail is the VERBATIM legacy body extracted as a named `run*` function, an `EFFECT_CORE_*` flag with a verbatim `*Legacy` twin (these cores are small; the §8 no-full-body rule was about spawn's size and does not apply), dispatchers keep their names, discharge via the ctx-resident `runControlDetached` (no new run* sites — the ingress pin stays at 2). Rationale: the P9 exit gate says "all daemon asynchronous application workflows are Effects" — transport-only would leave the three cores as native async fns and game the adapter inventory; the degenerate form costs a proven-safe body move and keeps every byte-frozen invariant (custody lease, purgeRows ordering, git argv) verbatim inside the coarse tail, exactly as slices 3–5 did on scarier code. A's safety argument survives intact under B'.

**Defect-identity note (verified in vendored source, binding on all P9.2 evidence):** `runPromiseWith` rejects with `causeSquash(exit.cause)` = the RAW first Fail error / Die defect (node_modules/effect/src/internal/effect.ts:299-309, :5475-5489). Rejections crossing the converted dispatchers are therefore identity-preserved — the fail-soft fold and both 500 dialects see the same error object as legacy, log lines included. Characterization must still pin one full-dispatcher rejection per route (flag true vs false, identical wire + log dialect).

**OQ-2 ACCEPTED:** fold the GET fail-soft inside the workflow (matches the legacy fold's placement); pin GAP-2 (rejection arm) first.

**OQ-3 MODIFIED:** parameterize via OPTIONAL config args whose defaults reproduce today's bytes, so every landed caller is untouched and the landed suites must pass with zero edits; the two new routes pass `(generic500Body, errorPrefix)` explicitly. If default-preserving parameterization is not cleanly provable, add the parameterized settler ALONGSIDE and leave `settleControlAsyncRoute` byte-identical.

**OQ-4 ACCEPTED (as commits on fd/v1-effect-feasibility, not PRs):** order 0→1→2→3. Reviews: Slice 0 = orchestrator line review; Slice 1 (GET) = line review; Slices 2 (preflight) and 3 (remove) = adversarial-review-mandatory.

**OQ-5 ACCEPTED:** per-route null builders + the EFFECT_CORE_* core flags from OQ-1 (two independent rollback layers).

**OQ-6 ACCEPTED:** folded into Slice 0 — verify or add the preflight-500 pin before Slice 2 starts.

**Sequencing:** P9.2 execution starts only after P9.1 closes (its transport work shares http.ts/program.ts with slice 6b).
