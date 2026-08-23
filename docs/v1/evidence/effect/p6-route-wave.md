# P6.4 route-conversion wave

The [P6.4 work package](../../effect-migration-plan.md) converts HTTP
application handlers to Effect workflows in repeatable route-group sub-slices.
This file is the wave record at HEAD `e2518a63` on `fd/v1-effect-feasibility`.
The P6.1 freeze remains [p6-http-matrix.md](./p6-http-matrix.md); this overlay
does not renumber G1–G11 / P1–P23.

**Section inventory:** §0 identity · §1 slices and review verdicts · §2 defect
family · §3 killing schedule · §4 closure · §5 Exit × witness settle table ·
§6 bundle identity · §7 suite counts · §8 remaining / resume · §9 WS-snapshot
ingress slice (uncommitted, on top of `c03c406d`).

Adversarial reviews lived in scratch (`/tmp/fd-effect/{paste,settings,control,join-fix}-review.md`)
and are transcribed here. Do not treat those paths as durable evidence.

---

## 0. Identity

| Field | Value |
| --- | --- |
| Branch | `fd/v1-effect-feasibility` |
| Implementation HEAD **and** `origin/fd/v1-effect-feasibility` | `e2518a63dbf7f198cd8ef01624d70e3fc0e300ee` (`fix(effect): join started mutations under interruption`) |
| Wave parent | `ac438c21` (`docs(effect): capture the pre-P6.4 performance baseline`) |
| Date | 2026-08-23 |
| Package | `fleetdeck@0.23.6` (standing landmine: daemon + bundle changed; version bump ×4 is required before a PR to main / publish — `hook-integrity` keys on `semverGt`) |
| Bun | 1.3.14 (`0d9b296a`) |
| Effect | `4.0.0-rc.110` |
| Host (quiet suites) | WSL2, otherwise idle |
| This documentation | uncommitted. Implementation is pushed. |

Five wave commits, oldest first:

| Commit | Subject | Review at landing |
| --- | --- | --- |
| `56a15e8a` | convert the health/state route group to Effect workflows | SHIP-WITH-NITS; three template SHOULD-FIXes applied in the same commit |
| `cffb9dea` | convert the paste-image route group to Effect workflows | SHIP-WITH-NITS; static recorded legacy-until-P13 |
| `c7eeb641` | convert the settings/command/mail/cleanup route group to Effect workflows | initially **DO-NOT-SHIP** (defect family, §2) |
| `62ef3c0f` | convert the control route group to Effect workflows | initially **DO-NOT-SHIP** (same family) |
| `e2518a63` | join started mutations under interruption | re-review **CLOSED-SHIP-WITH-NITS**; also the bundle line-comment strip |

Rollback of any converted group remains `effectRoutes=null` (unset
`installEffectRoutes` in `program.ts`). Whole-wave revert is the five commits
above, in reverse.

---

## 1. Slices and review verdicts

Convention every later worker copies (pilot, `src/daemon/app/http-workflows/health-state.ts`):

- one module per route group under `src/daemon/app/http-workflows/`;
- capabilities-as-parameters, **R=never** (no Context lookup; Core is not an
  Effect service until P8);
- **E=never** (expected 400/409/503-from-core are DATA, not typed errors);
- structural `HttpEffectRoutes` port in `http.ts`; `program.ts` fills it via
  `installEffectRoutes` after the owner exists and before bind;
- `mapEffectRouteExit` in `http-policy.ts` is the shared Exit classifier;
  interpretation is **per settler**;
- rollback = `effectRoutes=null`.

Pilot template SHOULD-FIXes applied in `56a15e8a`:

1. `Cause.hasInterruptsOnly` → `'quiesce'`, not defect.
2. Pin `HttpWorkflowEffect` E=`never` and `HttpQuiescingFailure` on `runRequest`'s Exit.
3. Convention header: every later worker copies the convention; PER-GROUP
   policies are marked as such (snapshot fallback is not universal).

### Converted routes (18)

| ID | Path | Settler class | Workflow module | Covering workflow tests |
| --- | --- | --- | --- | --- |
| G1 | GET `/health` | snapshot / legacy fallback | `health-state.ts` | `tests/effect/http-workflow-health-state.test.ts` |
| G2 | GET `/state` | snapshot / legacy fallback | `health-state.ts` | same |
| P8 | POST `/api/paste-image` | sync mutating (503, no replay) | `paste.ts` | `tests/effect/http-workflow-paste.test.ts` |
| P6 | POST `/api/settings` | sync mutating | `settings-command-mail-cleanup.ts` | `tests/effect/http-workflow-settings-command-mail-cleanup.test.ts` |
| P7 | POST `/command` | sync mutating | same | same |
| P3 | POST `/mail` | async mutating, join-on-interrupt | same | same + `tests/effect/ingress-supervisor.test.ts` (live join) |
| P4 | POST `/api/cleanup` | async mutating, join-on-interrupt | same | same + live join |
| P12 | POST `/api/spawn/:id/kill` | async mutating, join-on-interrupt | `control.ts` | `tests/effect/http-workflow-control.test.ts` + live join |
| P13 | POST `/api/spawn/:id/revive` | async mutating, join-on-interrupt | `control.ts` | same |
| P14 | POST `/api/sessions/:sid/adopt` | async mutating, join-on-interrupt | `control.ts` | same |
| P16 | POST `/api/sessions/:sid/dismiss` | async mutating, join-on-interrupt | `control.ts` | same |
| P17 | POST `/api/sessions/:sid/dismiss/retry` | async mutating, join-on-interrupt | `control.ts` | same |
| P18 | POST `/api/spawn/:id/rc` | async mutating, join-on-interrupt | `control.ts` | same |
| P15 | POST `/api/sessions/:sid/name` | sync mutating | `control.ts` | `tests/effect/http-workflow-control.test.ts` |
| P19 | POST `/api/questions/:n/answer` | sync mutating | `control.ts` | same |
| P20 | POST `/api/questions/:n/dismiss` | sync mutating | `control.ts` | same |
| P21 | POST `/api/plans/:n/mark` | sync mutating | `control.ts` | same |
| P22 | POST `/api/plans/:n/assign` | sync mutating | `control.ts` | same |

`mapEffectRouteExit` classification is shared:

| Exit | kind |
| --- | --- |
| Success | `'success'` |
| `ApplicationQuiescingError` (structural `_tag`) | `'quiesce'` |
| `Cause.hasInterruptsOnly` | `'quiesce'` |
| die / unexpected fail | `'defect'` |

Interpretation is per settler:

- **Snapshot** (`settleEffectSnapshotRoute`): quiesce/interrupt → **legacy sync
  handler** (always-200). G1, G2.
- **Sync mutating** (`settleEffectMutatingRoute` / `settleEffectPasteImageRoute`):
  quiesce → frozen `503 {"ok":false,"reason":"shutting-down"}`, **never replay
  the write**. `Effect.sync` completes on the admitting turn; interrupt-after-start
  is a no-op. P8, P6, P7, P15, P19–P22.
- **Async mutating** (`settleEffectAsyncMutatingRoute` / `settleControlAsyncRoute`
  + `startOnce`): **join-on-interrupt**. 503 ONLY when `started()===null`; if the
  native Promise started, JOIN it and emit the true legacy `.then/.catch` bytes
  so `res.done` still ties `closeClients` to the write. P3, P4, P12–P14, P16–P18.

### Still legacy (not in the 18)

- G3 GET `/api/settings`, G4–G6 worktrees/fs, G7 GET `/mail`, G8 GET `/api/watch`,
  G9–G11 shell/static/404.
- P1 `/hook/:Name` (LAST; behind an exhaustive fail-open contract test).
- P2 `/mail/ack`, P5 `/api/worktrees/remove`, P9 arm-unsupervised, P10 preflight,
  P11 spawn, P23 POST 404.
- Static assets: recorded legacy-until-P13 (paste review; wrapping
  `readFileSync` would invent an intra-quiesce window the sync path does not
  have).
- WS-snapshot ingress: **converted-by-ownership + pure-leaves-only** — see §9
  (uncommitted, on top of `c03c406d`). No Effect workflow was added: the /ws
  surface's lifecycle already runs under the P6.3 owner, and its three inline
  decisions are lifted to pure `http-policy.ts` leaves. Terminal WS (`/ws/term`)
  handlers stay untouched behind the termbridge facade until P7.
- GET `/mail` and GET `/api/watch` stay under their P1 owners until P10.

---

## 2. Defect family

Headline of the wave, recorded honestly. All 190+ green tests at
`c7eeb641` / `62ef3c0f` missed it. Two independent adversarial reviews
caught it on both slices (`settings-review.md` DO-NOT-SHIP; `control-review.md`
DO-NOT-SHIP). Closed by `e2518a63`; re-review `join-fix-review.md`
CLOSED-SHIP-WITH-NITS.

Two cooperating holes:

### 2a. Bridge: `runPromise(Effect.exit(runnable))` rejected under external interrupt

Production `IngressSupervisor.runPromiseExit` was
`runPromiseWith(Effect.exit(runnable))`. rc.110 `failCause` is already an Exit
(`exitFailCause`). `interruptUnsafe` on a yielded fiber does
`evaluate(failCause(interrupt))`. `runLoop` sees `ExitTypeId` in `_yielded` and
**returns that Exit**, skipping `Effect.exit`'s `contE`. `runPromiseWith` then
`throw causeSquash` → `"All fibers interrupted without error"`. The settler
`.catch` wrote a defect 500 while the native Promise kept running.

Empirical probe (project `effect`, this tree; transcribed from
`join-fix-review.md`):

| path | `interruptUnsafe` of arity-0 `Effect.promise` |
| --- | --- |
| OLD `runPromise(Effect.exit(inner))` | **rejected** `"All fibers interrupted without error"`; native still fulfilled `"landed"` |
| `runPromiseExit(inner)` / `runPromiseExitWith(inner)` | **resolved** `Failure`, `hasInterruptsOnly=true`; native still fulfilled |
| LiveIngressSupervisor `runPromiseExit` + `interrupt()` | same resolve; `activeCount` 0; native still joinable |
| `runPromiseExitWith(Effect.die)` | **resolved** `Failure(Die)`, does not reject |
| `runPromiseExitWith(Effect.exit(succeed))` | nested `Success(Success)` — production does **not** wrap |
| `runPromiseExitWith(Effect.exit(inner))` + interrupt | still interrupts-only Failure (short-circuit skips `contE`) |

Control review's live probe of the old path: in-flight interrupt of the six
async control POSTs answered `500 {"err":"internal"}` (settler defect arm), not
the claimed 503, and the core Promise still committed.

### 2b. Async mutating settlers 503'd a started native Promise

`Effect.promise` arity-0 thunks (`evaluate.length !== 0` is `withSignal`) start
a native Promise with no `AbortController`. Interrupt sets `resumed=true` so a
later `resume` is a no-op; the native Promise continues. Mutating settlers
treated interrupts-only as the same 503 as a true admission refusal, unsticking
`res.done` while SQLite/tmux were still owned.

Settings-review empirical probe (repo cwd, `interruptUnsafe` + `Fiber.await`,
matching production):

```json
{"interrupted":true,"promiseFinished":true,"promiseValue":"landed","exitTag":"Failure","causeReasons":["Interrupt"]}
```

The focused workflow tests stubbed `runRequest` to inject an interrupts-only
Exit and never POSTed `/mail` or `/api/cleanup` (settings/command are
`Effect.sync`; interrupt-after-start is a no-op on a completed sync leaf). The
control interrupt test likewise stubbed the Exit and never ran the Effect.

`Effect.tryPromise` was not the bug: rejection mapping via `Effect.promise` →
`resume(die(e))` is the parent `.catch` dialect. Passing an `AbortSignal`
without making `cleanup`/`postMail` abort-aware would not have fixed it either
— length-1 only aborts the controller.

---

## 3. Killing schedule

The schedule that matters (settings review F1; join-fix review restated the
closed form). Admitted POST `/api/cleanup` → `evaluate()` starts the native
Promise → shutdown `closing-clients` `interruptUnsafe`:

**Before `e2518a63`:**

1. POST admitted while ingress still `open`. Workflow runs. `Effect.promise`
   `evaluate()` fires. Native `cleanup()` is in flight.
2. Shutdown `quiescing`: `ingress.quiesce` + `http.quiesce` + `core.quiesce`.
   New admissions 503. This request is already inside.
3. `closing-clients`: `interruptUnsafe` → (2a) `runPromise` **rejects** →
   settler `.catch` defect 500, **or** (if the Exit had resolved) mapper
   `'quiesce'` → settler **503 `shutting-down`**. Either way `res.done`
   resolves → `closeClients` proceeds.
4. Native Promise is still running. `cleanup()` is not in any inFlight set and
   is not joined by `closeCore`. After tmux awaits it **always** writes SQLite.
   `closing-store` can `db.close()` under that continuation → `SQLITE_MISUSE`,
   or a **partial Clear** (windows killed, cards not archived), or a **full
   Clear after the client already got 503/500** (silent success-after-refusal;
   operator retries). Frozen: `res.done` joined the `.then(json)` chain, so
   store close waited for 200/409/500.

`POST /mail` is milder (`mail.close()` still joins `inFlight`) but still a
dialect regression: client could see ingress `503 shutting-down` instead of the
joined mail-lifecycle 503, or (control) a defect 500 while `spawnKill` /
`revive` / `adoptSession` / … continued. A board that retries revive after
restart can double-pane.

**After `e2518a63` (join-fix review, re-derived):**

1. `closing-clients` `interruptRuntime()` → `interruptUnsafe` →
   `runPromiseExit` **resolves** interrupts-only.
2. Settler sees `started()!==null` → `joinNative`; **does not `json()`**.
3. `json()` is `writeHead`+`end` → `_resolve`s `res.done`.
4. `drainThenRespond` `resolve(res.done)` (flattening); `active.promise` **is**
   that chain.
5. `forceFaultedResponseDuringShutdown` returns without `forceEnd` when
   `drained && !drainFaulted && !destroyed` — a fully-read POST.
6. `closeClientsOnce` `await Promise.allSettled(active.promise)` therefore
   waits for the native op.
7. `closing-store` / `db.close()` is after that await.

---

## 4. Closure

`e2518a63` is two cooperating fixes plus the bundle line-comment strip.

### 4a. Bridge

`runPromiseExit` now calls `Effect.runPromiseExitWith(context)(runnable)` with
the runnable **unwrapped**. rc.110 `runPromiseExitWith` is `runFork` +
`fiber.addObserver(exit => resolve(exit))` — always resolves. Do **not**
double-wrap in `Effect.exit` (nests the Exit, breaks the mapper). Die still
resolves; quiesce-refusal early path (`Promise.resolve(Exit.fail(refusal))`)
is unchanged. P4 supervisor contracts re-verified: `runPromise` still
`runPromiseWith` (still rejects on interrupt; stuck-cleanup still asserts
`workSettled → true`); readiness `Effect.void` still resolves as an Exit.

### 4b. startOnce + join-on-interrupt

`startOnce(op)` memoizes the native Promise (`invoke: () => native ??= op()`,
`started: () => native`). The workflow calls `invoke`; the recorder is the
only witness that can tell a true `ApplicationQuiescingError` refusal from
interrupt-after-start (the mapper collapses both to `'quiesce'`).

Wired to POST `/mail`, POST `/api/cleanup`, and the six controlAsync POSTs
(`settleControlAsyncRoute`). The five SYNC control POSTs + settings/command +
paste-image stay on the sync-mutating settler.

### 4c. Live-bridge tests

They construct a real `makeIngressSupervisor(Context.empty(), rootScope)` and
install `runRequest: supervisor.runPromiseExit`. `gateAsyncMethod` wraps the
real core method; the returned Promise settles only on `release()`.

They would fail:

- **(a)** regression to `runPromise(Effect.exit)` — Exit promise rejects →
  settler `.catch` 500 → `responded===true` before `release()`.
- **(b)** settler that 503s a started op — same, `responded===true` after
  `activeCount===0`.
- **(c)** quiesce path invoking the op — `invocations()===0` on the second
  board.

They pin the client response, which is the same `res.done` chain `closeClients`
waits on. They do not themselves call `closeClients`.

### 4d. Residual nit (accepted)

Shutdown-only duplicate `errorPrefix` log on controlAsync interrupt × native
**rejection**: the fold still calls `caps.onError` and JOIN `onRejected` logs
the same prefix. Response bytes stay **single** (`json()` only from JOIN).
Live tests use spawnKill 404 **fulfill**, so they do not pin this race. Two
identical `fleetd <route> error:` lines on one shutdown request could look like
two faults in a BUG log; they are one. Not a byte defect.
(`join-fix-review.md` Finding 1 — NIT.)

Paste review N1 (mapper interrupt comment still snapshot-only) is the comment
fix in `src/daemon/http-policy.ts` accompanying this documentation. Paste
review N2 (stale P8 anchors) is fixed in the matrix overlay. Control review's
`settleEffectControlRoute` leftover name was already renamed in `e2518a63`.

---

## 5. Exit × witness settle table

`startOnce` is the only witness. **503 only when `started()===null`.**

| outcome | `started()` | bytes | native |
| --- | --- | --- | --- |
| success | yes (by construction) | workflow `{status,body}` | already settled (`Effect.promise` awaited it) |
| quiesce | `null` | `503 {"ok":false,"reason":"shutting-down"}` | never invoked |
| quiesce | Promise | JOIN → mail `status??200, body??out`; cleanup `!ok?409:200`; control `json(out.status,out.body)` / reject `{ok:false,reason:'internal'}` | waited |
| defect | `null` | per-route defect 500 | never invoked |
| defect | Promise | JOIN first | waited |

A sync throw inside `operation()` before the Promise is returned does not
assign (`??=` aborts); that is the CONTROL_DEFECT `{err:'internal'}` dialect,
matching the legacy outer catch.

No Exit/witness cell writes bytes and then lets the native complete unjoined.
The residual `.catch` on a **rejected** `runRequest` still `emitDefect`s
without checking `started()`; that is the old 2a path, and the bridge no
longer rejects on interrupt (probe + live tests).

---

## 6. Bundle identity

`e2518a63` also extended the bundle post-step (`scripts/strip-bundle-jsdoc.ts`)
to strip `//` line comments (plus the existing JSDoc + esbuild-header strip).
Preserves shebang, GENERATED banner, non-JSDoc blocks, line count, kept names.
Minify-equivalence proof byte-identical. 18 lexer self-checks.

The strip retired the recurring gzip-ceiling squeeze. Mid-wave the join fix
alone had been +720 B gzip over the 189,440 ceiling (worker left that budget
decision unmade). The line-comment strip in the same commit recovered it.

Final identity at `e2518a63` (policy uses `Bun.gzipSync(bytes, {level:9, library:'zlib'})`;
do not substitute Python `zlib.compress`):

| Field | Value |
| --- | --- |
| Raw | 601,875 B (under the 768,000 B ceiling) |
| gzip-9 zlib | 164,469 B |
| Headroom vs 189,440 B ceiling | 24,971 B |
| SHA-256 | `b9c02c5f00abe54edb516e2859b5de61badca18472220c5ac774b8bcf145f771` |
| Lines | 19,841 (`split('\n').length`; `wc -l` is 19,840 newlines) |
| Deterministic | two rebuilds byte-identical |

Trade-off: the generated bundle carries no comments now. Line numbers, kept
names, shebang, and the GENERATED banner are preserved.

Pilot identity at `56a15e8a` (historical, not the accepted HEAD artifact):
648,468 B raw / 187,643 B gzip-9 / SHA `4b54e416` / 1,797 B headroom.

Do not tune, raise, or revert the gzip budget. The standing version-manifest
landmine (`0.23.6` ×4) is not a reason to revert conversions.

---

## 7. Suite counts

Quiet WSL2 host, Bun 1.3.14, 2026-08-23, HEAD `e2518a63`. Logs:
`/tmp/fd-effect/quiet-test-9.log`, `/tmp/fd-effect/quiet-test-bundle-9.log`.

| Lane | Result |
| --- | --- |
| `bun run test` (source) | **1,554 pass / 0 fail**, 192 files, 530.42 s |
| `bun run test:bundle` | **1,545 pass / 9 skip / 0 fail**, 192 files, 519.53 s |

Join-fix review focused runs (pre-global, still green at HEAD):
`ingress-supervisor` 13/0, `http-server-owner` 7/0, `root-exit-paths` 5/0,
`lifecycle-coordinator` 16/0, settings-group workflow 16/0, control workflow
18/0, `mail-delivery-lease` + `cleanup-api` 8/0.

Expected stderr in the quiet source log (defect-path tests, paste symlink
refusal, `hook-integrity` version-landmine `::error::` lines) is not a
failure. The landmine fires because the behavior-bearing plugin payload
changed while all four version manifests stayed `0.23.6`; it does not fail
the suite, it will fail a PR-to-main / publish gate.

---

## 8. Remaining / resume

P6.4's plan box stays **unchecked**. The wave converted 18 application
routes; it did not finish the package.

Resume order (do not start P7–P14; do not mark P3's quiet-host item closed):

1. **WS-snapshot ingress slice** — done as converted-by-ownership +
   pure-leaves-only (§9), uncommitted on top of `c03c406d`.
2. **Exhaustive fail-open contract test**, then the **hooks** slice (LAST,
   per the full-spine guardrail).
3. **P6.6 / P6.8 closure.** P6.6 still owns the
   `stop(false)`-once-race-deadline-`stop(true)` machine; the `res.done` join
   invariant that `closeClients` depends on is restored for async-mutating
   Effect routes (§4–§5). P6.8 harness landed at `d5404aac`; the quiet-host
   baseline was captured at `ac438c21`
   (`docs/v1/evidence/effect/p6-baseline.json`, taken at `51d39ddd`). Do not
   check P6.8 until the post-conversion comparison is in.

P6.5 is preserve-as-implemented (frozen in the matrix §3 / `p6-ws-send-probe.md`).

---

## 9. WS-snapshot ingress slice

**Uncommitted**, on top of `c03c406d` (the wave-doc commit; implementation base
`e2518a63`). Outcome: **converted-by-ownership (P6.3) + pure-leaves-only**. No
Effect workflow, no new `HttpEffectRoutes` port surface, no `runRequest` bridge
call. The reasoning, argued with anchors, is that the /ws snapshot surface has no
application handler left to bridge — only transport machinery and pure decisions.

### 9a. What was converted vs recorded-as-is

| Surface | Disposition | Anchor |
| --- | --- | --- |
| /ws lifecycle (open/close/terminate at shutdown) | **converted-by-ownership** — already runs under the P6.3 `HttpServer` owner; the never-close-before-`stop(true)` invariant and the quiescing terminate path are untouched | `handleUpgrade` 503-on-quiescing (`http.ts:3046`); shared keepalive `if (quiescing) return` (`http.ts:3012`) |
| Buffered-byte eviction decision | **pure leaf** `wsBufferEviction` | `http-policy.ts`; wired `broadcast()` `http.ts:2790` |
| Keepalive liveness decision | **pure leaf** `wsKeepaliveAction` | `http-policy.ts`; wired shared keepalive `http.ts:3015` |
| Snapshot-frame assembly | **pure leaf** `assembleSnapshotFrame` | `http-policy.ts`; wired `wsSnapshot()` `http.ts:2772` |
| Upgrade admission | **already decomposed** — composes the existing `authorized`/`hostHeaderOk`/`crossSiteReason` policy leaves; no inline pure decision remained | `http.ts:3069` |
| Broadcast TRIGGER (coalescing `setTimeout`) | **left as-is** — transport machinery, not a handler; bridging a 60 ms flush timer changes only its timing (direction (b)) | `scheduleBroadcast` `http.ts` |
| Per-frame send loop | **left synchronous** — only the pure decisions inside it were lifted | `broadcast()` `http.ts` |
| `/ws/term` handlers | **untouched** behind the termbridge facade until P7 (the eviction there is `sendTermFrame` + `MAX_TERM_WS_BUFFER` + `close(1009)`, a term handler, not this slice) | `http.ts` |

The keepalive is a SHARED lifecycle timer over `[snapshotClients, termClients]`,
not a term handler, so wiring `wsKeepaliveAction` there does not touch `/ws/term`
handling — it replaces the identical inline `!isAlive` decision both servers ran.

### 9b. Why pure leaves, not Effect workflows

The leaves live in `src/daemon/http-policy.ts` (DOMAIN zone), not under
`app/http-workflows/`, decided by `tests/import-boundaries.ts`: `http.ts` is a
domain module (`isDomainSource`) barred by `domainImportForbidden` from importing
`app/**`, and the workflow modules live under `app/`. `http-policy.ts` is the
pre-existing pure-policy home `http.ts` already imports, and its header already
permits bare `effect/*` in the domain zone. The three decisions run inside the
synchronous broadcast loop and the keepalive timer — transport machinery — so
wrapping them in an Effect through the ingress bridge would manufacture Effect for
its own sake and buy nothing (the pre-authorized outcome).

### 9c. Extracted leaves inventory

All in `src/daemon/http-policy.ts`, byte/behaviour-identical to the inline code:

- `wsBufferEviction(bufferedAmount, cap): 'evict' | 'send'` — R1-2 backpressure;
  strictly-greater-than the cap evicts (matches the `FLEETDECK_WS_BUFFER_MAX=-1`
  test lever: `0 > -1` evicts; idle `0 > 0` sends).
- `wsKeepaliveAction(isAlive): 'ping' | 'terminate'` — H-R3 liveness.
- `assembleSnapshotFrame<S, L>(snapshot, legacyUpgrade)` — the frozen frame
  `{ type:'snapshot', ...snapshot, legacy_upgrade }` in exact key order; generic so
  it preserves the concrete snapshot type (no `tsc` change). H-S1 stays a call-site
  choice (`core.snapshot()`, not `snapshotWithLan()`); BUG-031 legacy_upgrade rides.

Isolation coverage: `tests/effect/http-ws-snapshot-leaves.test.ts` (9 tests) —
the boundaries, the `-1` lever, ping/terminate, frozen key order, legacy_upgrade
present when null, no-injection (H-S1), and byte-identity with the pre-extraction
inline expression. Real-daemon behaviour is unchanged and stays green in
`tests/ws-hardening.test.ts` (coalescing / eviction / keepalive / H-S1 / BUG-031)
and `tests/terminal-ws.test.ts` (untouched).

### 9d. Gate counts (quiet WSL2 host, Bun 1.3.14, 2026-08-23)

| Gate | Result |
| --- | --- |
| `bun run typecheck` (root + board) | exit 0 |
| `bun run ci` (`biome ci`) | 404 files, no fixes applied |
| `tests/effect/http-ws-snapshot-leaves.test.ts` (new isolation) | in the batch below |
| `tests/effect/daemon-bundle-policy.test.ts` + `daemon-app.test.ts` + isolation | **13 pass / 0 fail**, 3 files |
| `tests/import-boundaries.test.ts` + `p6-http-freeze.test.ts` | **13 pass / 0 fail**, 2 files |
| `tests/ws-hardening.test.ts` | **5 pass / 0 fail** |
| `tests/terminal-ws.test.ts` (untouched-green) | **21 pass / 0 fail** |
| `tests/effect/` (full P6.4 wave regression) | **253 pass / 0 fail**, 39 files |

### 9e. Bundle identity (this slice)

Rebuilt twice, byte-identical (SHA `cd0ca27a2428266849888ae9d7876816b5ec0519f9200b87c94e38f4b92e6298`).
Policy method `Bun.gzipSync(bytes, {level:9, library:'zlib'})`.

| Field | e2518a63 (recorded) | this slice | Δ |
| --- | --- | --- | --- |
| Raw | 601,875 B | 602,148 B | +273 B (ceiling 768,000 B) |
| gzip-9 zlib | 164,469 B | 164,593 B | +124 B |
| Headroom vs 189,440 B | 24,971 B | **24,847 B** | −124 B |
| Lines (`split('\n')`) | 19,841 | 19,853 | +12 |

Well inside the budget; no tuning, raising, or reverting of the gzip ceiling. The
standing `0.23.6` ×4 version-manifest landmine is unchanged by this slice.

*End of WS-snapshot slice record. Anchors valid on top of `c03c406d`.*

*End of wave record. Anchors and SHAs valid at HEAD `e2518a63`.*
