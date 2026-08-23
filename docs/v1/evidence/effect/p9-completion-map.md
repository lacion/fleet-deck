# P9 completion map (HEAD `cd0470bb`)

P9 kickoff spec. Distilled from the P8.6 inventory report
(`/tmp/fd-effect/p8-6-inventory-report.md`, taken at `917c4dc8`) and
re-verified at implementation HEAD `cd0470bb`. P8 is closed: the five
root-context db-workflows slices completed the P8.6 checkbox for
root-owned workflows. Everything below is what P9 (and P10 / P7 / P13)
still owns.

**Repo:** `fd/v1-effect-feasibility` @ `cd0470bb`
(`feat(effect): the spawn-liveness tick yields Store`)
**Method:** read-only. Counts are `\bq\.<name>\.(run|get|all)\(` over
`src/daemon/*.ts` (product sources; `fleetd.bundle.mjs` excluded).
`mdns.ts` `q.` hits are DNS fields, not DB. `takeover.ts` /
`termbridge.ts` = **0** `q.*`.
**Stamp vs HEAD:** original 296 (memory) → **303 same-line matches** /
**302 `rg -c` lines** (two calls on `questions.ts:1399`) / **310
inclusive of 7 multiline `q.foo\n  .all(` sites**. Re-verified at
`cd0470bb`: leftover-HTTP `http.ts` lines, atomicity sites, and module
`q.*` surfaces did not move; `program.ts` wiring lines below are HEAD
(`provideService` is `:1312`).

`updateSession` (`statements.ts:853–865`) is the hottest write path
(dynamic cached UPDATE). **Not** a named `q.*` leaf; not in the 303.

---

## Classification rules (from the P8.6 convention header)

Quoted from `src/daemon/app/db-workflows/retention.ts:10–29`:

- **ROOT-CONVERTIBLE** — fiber owned by the root runtime → **may**
  `yield* Store`. The five root-owned workflows are **DONE** (P8.6).
- **HTTP-CAPABILITY** — request path is **not** the root fiber → keep
  capability params, `R=never` (P6.4). Converting the route was P6 or
  is P9.5. **Do not yield Store.**
- **P10-DEFERRED** — holds, GET `/mail`, GET `/api/watch`, questions
  orphan sweep (plan P5.5 / P10).
- **P7-DEFERRED** — `/ws/term` / termbridge. Zero DB leaves.
- **P9-SEAM** — remaining **async** Promise/git/spawn/mail-delivery /
  leftover-HTTP / takeover around still-sync `q.*` leaves.

`Store` is discharged **once** on the whole background gen:
`program.ts:1312` `Effect.provideService(Store, store.service)`. Root
slices add `yield* Store` in the workflow; no second discharge.

---

## 1. Per-module inventory

Counts: **n** = same-line `q.<stmt>.(run|get|all)(`; **n_ml** = n +
multiline extras.

### `spawns.ts` — 69 / 69

**Factory:** `createSpawns` (`spawns.ts` export). Wired
`derive.ts:1256–1269`.

| Surface | Context | Evidence |
|---|---|---|
| `spawnLivenessTick` | **root** agents-poll child; **P8.6 DONE** | `spawns.ts:3181`; `app/db-workflows/spawn-liveness.ts`; `agents-poll.ts` `ownedLivenessTick` / `runLiveness`; `program.ts:1230` `STORE_BACKED_LIVENESS` |
| `reconcileSpawns` | **root** boot; **P8.6 DONE** | `spawns.ts:3395` `Promise<void>`; `program.ts:1170`; `app/db-workflows/boot.ts` |
| `reconcileClearForks` | **root** boot, **sync**; **P8.6 DONE** | `spawns.ts:3576`; `program.ts:1167–1168`; `app/db-workflows/boot.ts` |
| `spawn` | leftover HTTP POST `/api/spawn` | `http.ts:2427`; `derive.ts:1258` |
| `armUnsupervised` | leftover HTTP POST `/api/spawn/arm-unsupervised` | `http.ts:2394–2400`; `spawns.ts:555` |
| `spawnKill` / `revive` / `enableRemote` / `adoptSession` | **HTTP already Effect** (control group) | `http.ts:2483–2705`; `http-workflows/control.ts:1–89` |

**Class:** MIXED. Root legs = **ROOT-CONVERTIBLE, DONE**. Leftover
spawn/arm = **P9.1 + P9.5**. Converted kill/revive/rc/adopt =
**HTTP-CAPABILITY** (do not yield Store). Atomicity: BUG-040
`claimPlanExecution` **before** clone/worktree/pane
(`spawns.ts:1420–1441`).

**Tests:** `spawn.test.ts`, `spawn-repo.test.ts`, `spawn-setup.test.ts`,
`spawn-unsupervised.test.ts`, `arm-gate.test.ts`, `revive.test.ts`,
`adopt.test.ts`, `p1-spawns-lifecycle.test.ts`,
`effect/http-workflow-control.test.ts`,
`effect/agents-poll-effect.test.ts`,
`effect/boot-reconciliation.test.ts`,
`effect/db-workflows-spawn-liveness.test.ts`,
`effect/db-workflows-boot.test.ts`.

---

### `questions.ts` — 42 / 42 (local `q` map, not `statements.ts`)

Local prepared map `questions.ts:327–346`. Line `questions.ts:1399` is
**two** calls: `q.pending.all()` + `q.resolved.all()` (`rg -c` reports
41).

| Surface | Context | Evidence |
|---|---|---|
| `answer` / `dismiss` | HTTP already Effect | `http.ts:2722–2759` |
| holds / rearm maps | in-memory, hook + watch | `questions.ts:348–358` |
| orphan sweep `setInterval` | **P1 timer, unref'd** | `questions.ts:1432–1440`; plan P5.5 “keep it on the explicit P1 handle until P10” |
| GET `/api/watch` | leftover held long-poll | `http.ts:2152–2154` |

**Class:** **P10-DEFERRED** (holds, watch, orphan sweep).
Answer/dismiss already **HTTP-CAPABILITY**. Do not P8.6-yield-Store;
do not start this in P9.

**Tests:** `questions-audit.test.ts`, `question-rearm.test.ts`,
`p1-question-retention-lifecycle.test.ts`, `choice-relay.test.ts`,
`board-hold-presence.test.ts`, `effect/http-workflow-control.test.ts`.

---

### `retention.ts` — 42 / 45 (+3 multiline)

| Surface | Context | Evidence |
|---|---|---|
| `pruneEvents` / `retentionSweep` | **root** retention schedule; **P8.6 DONE** | `db-workflows/retention.ts:66–88` `yield* Store`; `program.ts:1191` `STORE_BACKED_RETENTION=true` |
| `cleanup` | HTTP already Effect POST `/api/cleanup` | `http.ts:2328`; `derive.ts:1290` |
| `dismissSession` / `dismissRetry` | HTTP already Effect | `http.ts:2639–2679`; atomic no-await block `retention.ts:556–573` |

**Class:** root sweep = **ROOT-CONVERTIBLE, DONE**. cleanup/dismiss =
**HTTP-CAPABILITY** (already converted; must not yield Store). Dismiss
async window-kill after the atomic block is **P9.1** orchestration
behind an already-bridged route.

**Tests:** `effect/db-workflows-retention.test.ts`,
`effect/retention-schedule.test.ts`, `cleanup-api.test.ts`,
`dismiss.test.ts`, `p1-question-retention-lifecycle.test.ts`,
`audit-cleanup.test.ts`.

---

### `derive.ts` — 41 / 41

Not a leaf module: `createCore` composes the other 14
(`derive.ts:1186–1290`) and owns shared sync primitives.

| Surface | Context | Evidence |
|---|---|---|
| `card(sid, cwd)` get-or-create | hook + spawn + ingest + succession | `derive.ts:605–626`; **no await** in name+insert (`:602–604`) |
| `succeedSession` `BEGIN IMMEDIATE` | hook `/clear` succession | `derive.ts:953–967` |
| `tick` → `q.insertTicker` / `q.trimTicker` | **root** LAN onChange; **P8.6 DONE** | `derive.ts:1056–1058`; `app/db-workflows/lan-tick.ts`; `program.ts:1217` `STORE_BACKED_LAN_TICK` |
| `tombstoneCard` | spawns liveness / kill | `derive.ts:1164–1180` |
| `pruneEvents` | thin `q.pruneEvents.run` wrapper | `derive.ts:1299–1300` |

**Class:** MIXED. `tick` on LAN refresh = **ROOT-CONVERTIBLE, DONE**.
`card` / `succeedSession` are **sync leaves called from HTTP/hook
fibers** — converting them to `yield* Store` would violate the
convention. Treat as **P9-SEAM / shared sync kernel**, not a Store
slice.

**Tests:** `succession.test.ts`, `derive-audit-reliability.test.ts`,
`session-lifecycle.test.ts`, `ticket-callsign.test.ts`, `rename.test.ts`.

---

### `mail.ts` — 22 / 23 (+1 multiline `aliasesMatch`)

| Surface | Context | Evidence |
|---|---|---|
| `postMail` | HTTP already Effect POST `/mail` | `http.ts:2324`; `settings-command-mail-cleanup.ts` |
| `drainMail` | leftover GET `/mail` (mutating lease) | `http.ts:2135–2149` |
| `ackMail` | leftover POST `/mail/ack` | `http.ts:2319–2321` |
| `claimMail` / watch claim | leftover GET `/api/watch` | `http.ts:2152`; `derive.ts:1346` |
| `claimAllMail` `BEGIN IMMEDIATE` | **after** async tmux probes | `mail.ts:490–512` |

**Class:** POST `/mail` = **HTTP-CAPABILITY DONE**. GET `/mail` + GET
`/api/watch` = **P10-DEFERRED**. POST `/mail/ack` = leftover **P9.5**
(status matrix “P2 ack”). Pane-delivery Promise = **P9.4**.

**Tests:** `mail-and-blocking.test.ts`, `mail-delivery-lease.test.ts`,
`mail-frames.test.ts`, `p1-mail-lifecycle.test.ts`,
`watch-rewake.test.ts`,
`effect/http-workflow-settings-command-mail-cleanup.test.ts`.

---

### `events.ts` — 20 / 21 (+1 multiline `allSessions`)

| Surface | Context | Evidence |
|---|---|---|
| `applyEvent` + hook* | HTTP already Effect POST `/hook/:name` (fail-open) | `http.ts:1846–1868`; `derive.ts:1272–1282` |
| in-memory card mirror | same tick as `updateSession` | `events.ts:227` `let c = card(...)`; `:322–403` `c = { ...c, ...upd }` |
| plan-capture `BEGIN IMMEDIATE` | hook ExitPlanMode; applyEvent **after COMMIT** | `events.ts:916–931` (BUG-112) |

**Class:** **HTTP-CAPABILITY DONE** (hooks). Plan-capture is
P10-adjacent (question row + hold). **Do not yield Store.** Atomicity
forbids yield inside the txn and forbids splitting `c` vs
`updateSession` across fibers.

**Tests:** `effect/http-workflow-hooks.test.ts`,
`p6-hook-failopen-contract.test.ts`, `hook-*.test.ts`,
`needs-you.test.ts`, `plans.test.ts` (capture), `conflict.test.ts`.

---

### `settings.ts` — 16 / 16

| Surface | Context | Evidence |
|---|---|---|
| `setSettings` + `BEGIN IMMEDIATE` | HTTP already Effect POST `/api/settings` | `settings.ts:726`; `http.ts:2346` |
| `resolveSettings` | leftover GET `/api/settings` (matrix G3) **and** GET `/state` capability | `http.ts:2085–2087`; `derive.ts:1193` |

**Class:** POST = **HTTP-CAPABILITY DONE**. GET `/api/settings` =
leftover **P9.5**. Multi-key txn already runs inside `Effect.sync` on
the converted POST — do not yield inside txn.

**Tests:** `settings-transaction.test.ts`, `smoke-settings.test.ts`,
`effect/http-workflow-settings-command-mail-cleanup.test.ts`.

---

### `snapshot.ts` — 12 / 12

| Surface | Context | Evidence |
|---|---|---|
| `snapshot` / `fleetSize` | HTTP already Effect GET `/state`; `/ws` snapshot is converted-by-ownership (pure leaves in `http-policy.ts`, send loop still transport) | `http.ts:2081`; `derive.ts:1285–1286` |

**Class:** **HTTP-CAPABILITY DONE**. Not a P9 Store conversion.

**Tests:** `effect/http-workflow-health-state.test.ts`,
`effect/http-ws-snapshot-leaves.test.ts`, `p6-http-freeze.test.ts`.

---

### `worktrees.ts` — 10 / 11 (+1 multiline `worktreeSpawns`)

| Surface | Context | Evidence |
|---|---|---|
| `worktrees()` | leftover GET `/api/worktrees` (async git inspect) | `http.ts:2089–2101`; `derive.ts:1249` |
| `removeWorktree` | leftover POST `/api/worktrees/remove` | `http.ts:2332–2343`; `derive.ts:1249` |
| `purgeRows` `BEGIN IMMEDIATE` | after async `git worktree remove` | `worktrees.ts:696–711` |
| custody lease | in-memory, shared with revive | `derive.ts:1228–1247` |

**Class:** leftover HTTP = **P9.2 / P9.5**. Txn is sync-after-await.
Not root-context.

**Tests:** `worktrees.test.ts`, `worktree-chmod-symlink.test.ts`.

---

### `repos.ts` — 9 / 9

| Surface | Context | Evidence |
|---|---|---|
| `preflightRepo` | leftover HTTP POST `/api/repos/preflight` | `http.ts:2403–2424`; `derive.ts:1193` |
| catalog writes | spawn / hooks / ingest | `derive.ts:1186` must precede ingest/events/spawns |

**Class:** **P9.2** (git async) + leftover **P9.5**. Sync catalog
leaves stay `Effect.sync`/`try`.

**Tests:** `repos.test.ts`, `repo-identity.test.ts`,
`spawn-repo.test.ts`.

---

### `commands.ts` — 6 / 7 (+1 multiline `visibleSessions`)

POST `/command` already Effect (`http.ts:2377`). **HTTP-CAPABILITY
DONE.** Not a P9 Store conversion.

**Tests:** `fleet-command.test.ts`,
`effect/http-workflow-settings-command-mail-cleanup.test.ts`.

---

### `plans.ts` — 5 / 5

| Surface | Context | Evidence |
|---|---|---|
| `planMark` / `assignPlan` | HTTP already Effect | `http.ts:2771–2810`; `derive.ts:1207–1208` |
| `claimPlanExecution` | called from `spawns.spawn` **before async** | `spawns.ts:1441` |

**Class:** mark/assign = **HTTP-CAPABILITY DONE**. Claim is a **P9.1
constraint** (must stay sync-before-launch).

**Tests:** `plans.test.ts`, `accept-plan-*.test.ts`.

---

### `ledger.ts` — 4 / 4

Only reached from `events.applyEvent` → `recordFile` (hook path).
**HTTP-CAPABILITY** (hooks already converted). Not a root workflow.

**Tests:** `conflict.test.ts`, `filechanged-watch.test.ts`.

---

### `ingest.ts` — 3 / 3

`ingestAgentsPoll` only. Root agents-poll; **P8.6 DONE**
(`app/db-workflows/agents-ingest.ts`; `program.ts:1204`
`STORE_BACKED_AGENTS_INGEST`). Fail-open skip byte-identical.

**Tests:** `agents-ingest.test.ts`, `effect/agents-poll-effect.test.ts`,
`effect/db-workflows-agents-ingest.test.ts`.

---

### `files.ts` — 2 / 2 (`ctx.q`, not a local `q` map)

`files.ts:272` `ctx.q.getSession.get`, `:274`
`ctx.q.spawnBySession.get`. (Other `q` hits are search-string locals.)

Leftover HTTP: session FS `http.ts:2104–2118` + home FS `:2121–2132`.
**P9.3 / P9.5.** Bounded `runBounded` stays a named adapter.

**Tests:** `files-run-bounded.test.ts`, `session-fs.test.ts`.

---

### Non-DB (for P9/P7 completeness)

| Module | `q.*` | Class |
|---|---|---|
| `takeover.ts` | 0 | **P9.6** |
| `termbridge.ts` | 0 | **P7-DEFERRED** (`/ws/term` `http.ts:3209`) |
| `mdns.ts` | 0 DB | LAN child; `q.` are DNS question fields |
| `db.ts:497` `BEGIN` (deferred) | migrate | **P8.4 seam**, not an app workflow |

---

## Already-converted HTTP (do **not** retarget to `yield Store`)

From `http.ts` dispatch + P6 status:

GET `/health`, GET `/state`, POST `/api/paste-image`, POST
`/api/settings`, POST `/command`, POST `/mail`, POST `/api/cleanup`,
POST `/hook/:name`, POST `/api/spawn/:id/{kill,revive,rc}`, POST
`/api/sessions/:sid/{adopt,name,dismiss,dismiss/retry}`, POST
`/api/questions/:id/{answer,dismiss}`, POST
`/api/plans/:id/{mark,assign}`. `/ws` snapshot = converted-by-ownership
+ pure leaves.

Request-bridged Effects keep capabilities-as-params, `R=never` (P6.4).
Yielding Store on these fibers is a convention break, not a P9 win.

---

## Leftover HTTP (later packages, not P6 incompleteness)

Confirmed live in `http.ts` at `cd0470bb`:

| Route | `http.ts` | Core | Package |
|---|---|---|---|
| POST `/mail/ack` | 2319 | `ackMail` | P9.5 (matrix P2) / P10-adjacent lease |
| POST `/api/worktrees/remove` | 2332 | `removeWorktree` | P9.2 / P9.5 (P5) |
| POST `/api/spawn/arm-unsupervised` | 2394 | `armUnsupervised` | P9.1 / P9.5 (P9) |
| POST `/api/repos/preflight` | 2403 | `preflightRepo` | P9.2 / P9.5 (P10) |
| POST `/api/spawn` | 2427 | `spawn` | P9.1 / P9.5 (P11) |
| GET `/api/settings` | 2085 | `resolveSettings` | P9.5 (G3) |
| GET `/api/worktrees` | 2089 | `worktrees()` | P9.2 / P9.5 (G4) |
| GET `/api/sessions/:sid/fs/{list,read,search}` | 2104 | `fsList/Read/Search` | P9.3 (G5) |
| GET `/api/fs/{list,read,search}` | 2121 | `fs*Home` | P9.3 (G6) |
| GET `/mail` | 2135 | `drainMail` + optional `ackMail` | **P10** |
| GET `/api/watch` | 2152 | `watchHook` | **P10** |
| `/ws/term` | 3209 | termbridge | **P7** |
| static / favicon | 2156–2174 | — | P13 |

HTTP leftovers still dispatch with `.then` on the core Promise
(`http.ts:2093`, `:2335`, `:2411`). That dispatch is the P9.5
facade, not a reason to yield Store.

---

## 2. Atomicity hotspots (conversion-order constraints)

Five app `db.exec('BEGIN IMMEDIATE')` + migrate `BEGIN` + extra
no-await blocks. P9 must not yield inside any of these, and must not
split the paired in-memory mutation across a fiber boundary.

| # | Site | Constraint |
|---|---|---|
| 1 | **get-or-create `card()`** `derive.ts:605–626` | “ticketFromBranch → assignCallsign → insert chain has **NO await**” (`:602–604`). Hooks/events/ingest/spawn must not yield between name check and insert. Convert **after** callers, or keep as one coarse sync thunk. |
| 2 | **in-memory card mirror** `events.ts:227` + `:322–403` | `let c = card(...)` then same-tick `updateSession` + `c = { ...c, ...upd }`. Splitting `c` and the row across fibers races resurrection / col / source. `applyEvent` stays one non-interruptible thunk. |
| 3 | **`succeedSession` BEGIN IMMEDIATE** `derive.ts:967` | Archive predecessor + insert/rename heir + reassign spawns/mail/questions/touches/aliases. Half-done succession is worse than the bug (`:964–966`). Convert **after** `card()`/events, or one coarse sync thunk. Called from hook `/clear` path. |
| 4 | **plan-capture BEGIN IMMEDIATE** `events.ts:916` | Question insert + plan insert; `applyEvent` **after COMMIT** (BUG-112, `:903–911`). Hook path, P10-adjacent. No yield in txn. Fail-open on rollback. |
| 5 | **`claimAllMail` BEGIN IMMEDIATE** `mail.ts:512` | Lease batch **after** async tmux probes (`:490–498`). P9.4. No yield in txn. |
| 6 | **`setSettings` multi-key BEGIN IMMEDIATE** `settings.ts:726` | Already inside converted POST `Effect.sync`. Do not yield in txn. Half-applied gateway URL+token is the failure mode (`:715–725`). |
| + | **`purgeRows` BEGIN IMMEDIATE** `worktrees.ts:711` | After async git remove. P9.2. Expiry-before-delete ordering. |
| + | **dismiss atomic DB block** `retention.ts:556–573` | **NO awaits**; HTTP dismiss already Effect. Window-kill awaits **after** (`:575–578`) with re-read. |
| + | **BUG-040 `claimPlanExecution`** `spawns.ts:1420–1441` | Single guarded UPDATE **before** any clone/worktree/pane. P9.1 must keep claim sync-before-launch; loser 409s without launching. |
| + | **questions holds + DB same tick** `questions.ts:348` + create/answer | In-memory `holds`/`rearm*` maps. P10. Do not split map vs row across fibers. |
| + | **migrate `BEGIN`** `db.ts:497` | Deferred; P8.4 lifetime, not a P9 workflow. |
| + | **worktree custody lease** `derive.ts:1228–1247` | In-memory Map; no await between check and set. Shared by `removeWorktree` and `revive`. P9.1/P9.2 must preserve. |

Atomicity-heavy modules (`derive` / `events` / `mail` / `spawns`) stay
coarse thunks until P9 owns their async shells. Never convert a txn
site to yield-inside-txn.

---

## 3. P8.6-vs-P9 boundary (resolved)

### Boundary quotes (adjudicate scope on these)

**P8.6 checkbox** (plan P8):

> Convert application workflows to yield Store and translate failures
> once around a coarse synchronous DB operation with `Effect.try`; do
> not wrap each statement. […] Land one workflow/module per sub-slice,
> and forbid suspension/yielding inside a direct SQLite transaction
> callback […]

**P8 package exit gate** — **does not say “all workflows yield
Store”:**

> migration/restart/durability and query benchmarks pass; DB is
> acquired once, closed after all users, and cannot be accessed
> afterward; SQL candidate decision is recorded.

**P9 recommended order** (plan P9): P9.1 spawn/revive/dismiss
orchestration; P9.2 repo/worktree/git async; P9.3 files/search/cache;
P9.4 mail + provider async; P9.5 remaining HTTP-triggered application
services; P9.6 takeover/election.

**P9 exit:**

> all daemon asynchronous application workflows are Effects; remaining
> Promises are native callback/Response values inside named adapters;
> every compatibility bridge is inventoried.

**Convention that forbids interpreting P8.6 as “all 15 modules yield
Store”** (`db-workflows/retention.ts:10–15`): HTTP-bridged workflows
**do not** yield Store.

**Disposition at `cd0470bb`:** the five root-context conversions
complete the checkbox for root-owned workflows; HTTP-bridged workflows
keep capability parameters BY CONVENTION (P6.4 constraint),
holds/questions are P10, termbridge P7, async shells + leftover HTTP
are P9. This is the plan's own boundary, not incompleteness.

### P8.6 root-context slices — DONE

Whole-gen `provideService(Store)` already covers these. Each slice
copied the retention convention (`yield* Store` **before** the coarse
thunk; never inside a txn) and kept a `STORE_BACKED_*` source seam.

| # | Slice | Module | Entry | Commit | Notes |
|---|---|---|---|---|---|
| 0 | retention prune + sweep | `retention.ts` + `db-workflows/retention.ts` | root schedule | `145e9fbd` | adversarial SHIP-WITH-NITS; negative undischarged-die pin; `STORE_BACKED_RETENTION` `program.ts:1191` |
| 1 | **ingestAgentsPoll** | `ingest.ts` (3 `q.*`) + `db-workflows/agents-ingest.ts` | agents-poll | `570d8dae` | injectable 3rd param on `makeAgentsPollProgram`; default legacy keeps P5 Store-free |
| 2 | **boot clear-fork healing** | `spawns.reconcileClearForks` (sync) + `db-workflows/boot.ts` | boot, before async legs | `e0ab862a` | `Effect.try`; same `operationalError` both sides |
| 3 | **lan-refresh `core.tick`** | `derive.ts:1056` + `db-workflows/lan-tick.ts` | lan-refresh `onChange` | `570d8dae` | non-tick effects keep RefreshError→onError; tick swallowed via `catchTag('LanTickError')` |
| 4 | **boot `reconcileSpawns`** | `spawns.ts:3395` async + `db-workflows/boot.ts` | boot | `e0ab862a` | `ownedLegacyPromise`; yield Store **before** the Promise factory |
| 5 | **`spawnLivenessTick`** | `spawns.ts:3181` + `db-workflows/spawn-liveness.ts` | agents-poll | `cd0470bb` | **final** slice. `ownedLivenessTick` extracted verbatim (do **not** swap `ownedLegacyPromise` — sync-throw becomes `Effect.die`, not named `AgentsPollLivenessError`). Adversarial SHIP, zero findings |

Do **not** put HTTP-only modules on a further Store-yield list. Do
**not** convert txn sites to yield-inside-txn. The pilot still only
**declares** Store (`db-workflows/retention.ts:41–44`); later slices
that rethread statements bind `const { handle } = yield* Store`.

Type landmine (still live): annotate
`retention: RetentionSchedule<ProcessRunner | Store>`
(`program.ts:1241`) — do **not** add an explicit type arg on
`makeDaemonBackgroundProgram`.

### What P8.6 did **not** finish (plan assigns to P9 / P10 / P7)

| Plan item | Modules / routes | Why not P8.6 |
|---|---|---|
| **P9.1** | leftover POST `/api/spawn` + `armUnsupervised`; async guts of already-bridged kill/revive/rc/adopt/dismiss | async spawn/tmux orchestration; HTTP leftover = P9.5. Dismiss HTTP already Effect. |
| **P9.2** | `repos.ts`, `worktrees.ts` leftover GET/remove/preflight | async git; leftover HTTP |
| **P9.3** | `files.ts` session+home FS | leftover HTTP; `runBounded` adapter |
| **P9.4** | `mail.ts` pane delivery (`claimAllMail` after probes) | async tmux; GET `/mail`/watch stay P10 |
| **P9.5** | leftover HTTP table above | “remaining HTTP-triggered application services”; keep capabilities-as-params |
| **P9.6** | `takeover.ts` | 0 `q.*`; typed lifecycle, not Store |
| **P10** | questions holds, GET `/mail`, GET `/api/watch`, orphan sweep `questions.ts:1432` | plan P5.5 + P6 leftover + P10 purpose |
| **P7** | `/ws/term` | 0 `q.*` |

---

## 4. P9.1–P9.6 → module map

Resume in this order. P9 does not require P7's terminal ownership
(`/ws/term` stays P7; the termbridge facade is that seam). P7.0 remains
the standing §7 platform authorization checkpoint.

| P9 item | Modules | Async mechanism to remove | HTTP leftover? |
|---|---|---|---|
| **P9.1** | `spawns.ts` (+ `plans.ts` claim, `derive.ts` custody/`card`, dismiss window-kill in `retention.ts`) | spawn/revive/kill/rc Promise chains; liveness already P5-bridged **and** P8.6 Store-backed | POST `/api/spawn`, `/arm-unsupervised` |
| **P9.2** | `repos.ts`, `worktrees.ts` | git inspect/remove/preflight Promises | GET `/api/worktrees`, POST remove, POST preflight |
| **P9.3** | `files.ts` | `runBounded` child + FS Promises | session+home `/api/fs/*` |
| **P9.4** | `mail.ts` | tmux probe + pane paste; lease txn stays sync | GET `/mail`/`watch` wait for **P10**; POST `/mail/ack` is P9.5 |
| **P9.5** | leftover handlers in the table above + any still-Promise core behind already-bridged routes | HTTP dispatch still `.then` on leftovers (`http.ts:2093`, `:2335`, `:2411`) | yes — this **is** the leftover HTTP package |
| **P9.6** | `takeover.ts` | election/lifecycle Promises | none (`q.*=0`) |

P9.1 landmine (from the P8.6 spawn-liveness review): the join-owned
liveness bridge is `ownedLivenessTick` in `agents-poll.ts`.
`ownedLegacyPromise` maps a sync factory throw to `Effect.die`, which
is **not** the named `AgentsPollLivenessError` fail-open skip. Do not
“simplify” onto that helper. BUG-040 `claimPlanExecution`
(`spawns.ts:1420–1441`) must stay a single guarded UPDATE **before**
any clone/worktree/pane; a P9.1 spawn workflow that yields between
claim and launch races two winners.

For each module, keep the plan's five-step seam: identify the manual
Promise/cancellation/retry/resource/error mechanism → add a service
Effect without changing public policy → temporary facade while
unmigrated callers exist → convert callers and tests → delete the
facade. Intentional sync probes (`repo-identity`, small capability
checks) stay plain. Detached supervisor/CLI launches stay out of the
shared scoped process runner until survival and signal semantics have
dedicated tests.

---

## Evidence footnotes

- **303 same-line / +7 multiline / 310 inclusive** — python
  `\bq\.[A-Za-z_][\w]*\.(run\|get\|all)\(` and multiline variant over
  `src/daemon/*.ts` at `917c4dc8`; module surfaces re-verified at
  `cd0470bb`.
- **Per-module n:** spawns 69, questions 42, retention 42, derive 41,
  mail 22, events 20, settings 16, snapshot 12, worktrees 10, repos 9,
  commands 6, plans 5, ledger 4, ingest 3, files 2. Sum 303.
- **Multiline extras:** retention +3, mail +1, events +1, worktrees +1,
  commands +1.
- **`files.ts` `ctx.q`:** `:272`, `:274` (counted; `\bq\.` matches
  `ctx.q.`).
- **Questions local map:** `:327–346`; double call `:1399`.
- **Five IMMEDIATE + migrate BEGIN:** `derive.ts:967`, `events.ts:916`,
  `mail.ts:512`, `settings.ts:726`, `worktrees.ts:711`, `db.ts:497`.
- **Background children + Store provide (HEAD):** `program.ts:1234–1312`
  agents-poll / lan-refresh / retention / boot; Store provide `:1312`.
- **`STORE_BACKED_*` flags (HEAD):** `BOOT` `:1173`, `RETENTION`
  `:1191`, `AGENTS_INGEST` `:1204`, `LAN_TICK` `:1217`, `LIVENESS`
  `:1230`. Flip any one to `false` to restore that slice's legacy
  adapter.
- **Pilot does not bind handle:** `db-workflows/retention.ts:41–44`.
- **Type landmine:** `RetentionSchedule<ProcessRunner | Store>`
  annotation `program.ts:1241` — do not add an explicit type arg on
  `makeDaemonBackgroundProgram`.
- **P8 trial evidence:** [p8-strict-trial.md](./p8-strict-trial.md)
  (DO-NOT-ENABLE), [p8-stmt-cache-trial.md](./p8-stmt-cache-trial.md)
  (KEEP prepare-once),
  [p8-sql-client-trial.md](./p8-sql-client-trial.md) (KEEP direct
  bun:sqlite).
