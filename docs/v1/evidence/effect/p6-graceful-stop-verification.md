# P6.6 verification — graceful `Bun.serve` stop

**Plan item (docs/v1/effect-migration-plan.md, P6.6):**

> Implement graceful Bun.serve stop: initiate `server.stop(false)` once during
> quiesce, release holds, close clients, and race the graceful Promise with the
> absolute remaining deadline. If it loses, call and await `server.stop(true)`;
> do not await graceful stop serially before the force decision.

**Branch / HEAD:** `fd/v1-effect-feasibility` @ `b2d11d84`
**Method:** decompose the sentence into atomic clauses; for each, find the
implementing code at HEAD and the test that pins it; run every cited test.
This was a **verification** pass — no source was changed, so no clause needed a
new pinning test and no bundle rebuild was required.

---

## Clause decomposition

The plan sentence decomposes into seven atomic obligations:

1. **A —** `server.stop(false)` is initiated **exactly once** (idempotent).
2. **B —** initiated **during quiesce** (not deferred to a later phase).
3. **C —** **holds are released** as part of the sequence.
4. **D —** **clients are closed** as part of the sequence.
5. **E —** the graceful Promise races the **absolute remaining deadline** — the
   one whole-daemon budget, *not* a fresh per-phase relative timeout.
6. **F —** graceful stop is **not awaited serially** before the force decision.
7. **G —** if graceful loses the race, `server.stop(true)` is **called *and*
   awaited**.

---

## Clause table

| # | Clause | Code anchor (HEAD `b2d11d84`) | Pinning test | Status |
|---|--------|-------------------------------|--------------|--------|
| A | `stop(false)` once (idempotent) | `src/daemon/http.ts:3547` once-guard `if (gracefulStopPromise) return gracefulStopPromise`; single `await live.stop(false)` @ `http.ts:3576` | `tests/http-lifecycle.test.ts:755` (`graceful === beginGracefulStop()` @800; `closeClients()`/`forceStop()` identity @826/830; `close` reuses force @835); `:596` (shared completion @606 + **rebind proves `stop(false)`, not process teardown, freed the socket** @613-618); `tests/effect/daemon-resource-lifecycle.test.ts:170` (`http.beginGracefulStop` appears once in the phase order) | **PINNED** |
| B | Initiated during quiesce | `quiesceHttp()` calls `void startGracefulStop()` @ `http.ts:3663`; coordinator quiescing phase calls `beginGracefulStop()` @ `daemon-resource-lifecycle.ts:244` | `tests/effect/daemon-resource-lifecycle.test.ts:170` (`beginGracefulStop` fires **in the quiescing phase**, before releasing-holds/closing-clients/closing-http) | **PINNED** |
| C | Holds released | `releaseHeldResponses()` @ `http.ts:3666`; graceful gated on the `holdsReleased` barrier @ `http.ts:3559`; coordinator releasing-holds phase | `tests/http-lifecycle.test.ts:755` (@812 `gracefulSettled === false` — "native stop waits for the held-hook flush barrier"; @814-817 release → held hook returns `200 {}`); `tests/daemon-lifecycle-integration.test.ts:107` (real SIGTERM drains a held hook) | **PINNED** |
| D | Clients closed | `closeClientsOnce()` @ `http.ts:3715`; coordinator closing-clients phase | `tests/http-lifecycle.test.ts:755` (@825-827 `closeClients()` idempotent + awaited); `tests/effect/daemon-resource-lifecycle.test.ts:498` (SQLite not closed until client + ingress owners both join) | **PINNED** |
| E | Race the **absolute** remaining deadline (not a per-phase timeout) | One `ShutdownBudget` = absolute `deadlineMs = startedAtMs + timeoutMs` @ `lifecycle-coordinator.ts:139-166` (`remainingMs() = max(0, deadlineMs − now)`); created **once** in `runClose` @ `:454` and passed to every `runPhase(phase, budget)` @ `:464`; `deadlineWait` uses `budget.remainingMs()` @ `:326`; `runPhase` races `Promise.race([operationCompletion, deadline.completion])` @ `:600`. Doc contract @ `:257` "phases never receive a fresh relative timeout." | `tests/effect/lifecycle-coordinator.test.ts:183` ("one absolute deadline times out blocked and **later phases without resetting their waits**"); `:515` (`ShutdownBudget` one absolute deadline, `remainingMs()` monotone non-increasing); `:374` (forced owner that never settles is gated **until the absolute deadline**, `deadlineExpired === true`) | **PINNED** |
| F | Not awaited serially before the force decision | Coordinator `closeHttp` uses `await Promise.race([graceful ?? Promise.resolve(), this.forceRequested])` @ `daemon-resource-lifecycle.ts:470` (**race, not serial await**); `forceStopHttp` comment + code "Crucially, this does not await gracefulStopPromise" @ `http.ts:3602`, `live.stop(true)` @ `:3605` runs while `stop(false)` is still parked | `tests/http-lifecycle.test.ts:755` (@818-823 `gracefulSettled === false` because an open WebSocket keeps `stop(false)` pending, yet @829-831 `forceStop()` settles within 1 s — force did **not** wait on graceful); `:626` (force wins before the barrier → `order === ['force','graceful']`, graceful *joins* force); `tests/effect/daemon-resource-lifecycle.test.ts:261` (coordinator forces HTTP while the graceful deferred is **never resolved**; closing-http still completes as `Forced`) | **PINNED** |
| G | Loser → `stop(true)` called **and** awaited | `startForceStop()` @ `daemon-resource-lifecycle.ts:360` → `http.forceStop()` → `live.stop(true)` @ `http.ts:3605`; awaited in `closeHttp` @ `:467`/`:480`/`:491` and in `releaseProcess` @ `:526` | `tests/effect/daemon-resource-lifecycle.test.ts:349` (root force reserve **settles `stop(true)` before coordinator return**); `:428` (rejected graceful → `['http.forceStop','http.forceStop.settled']`); `:402` (typed force-stop rejection retained); `tests/http-lifecycle.test.ts:755` (@829-832 forced awaited, WebSocket then closed); `tests/effect/lifecycle-coordinator.test.ts:215` (force reserve opened before the absolute deadline **and still awaits owner settlement**) | **PINNED** |

---

## Honesty checks the task flagged

**"absolute remaining deadline" vs a per-phase deadline.** Verified by reading
the deadline arithmetic. There is exactly one `ShutdownBudget`
(`lifecycle-coordinator.ts:139`) whose `deadlineMs` is fixed at construction as
`startedAtMs + timeoutMs`. `runClose` constructs it **once** (`:454`) and threads
the *same* instance into every `runPhase` (`:464`), which computes its wait from
`budget.remainingMs()` (`:326`, `:552`, `:569`) — a value that only decreases.
No phase is handed a fresh `timeoutMs`; the doc comment at `:257` states this as
the contract and `lifecycle-coordinator.test.ts:183`/`:515` pin it
(later phases do not reset their waits; remaining never increases). At the HTTP
owner the "race against the deadline" is realized as `closeHttp`'s
`Promise.race([graceful, forceRequested])` (`daemon-resource-lifecycle.ts:470`),
where `forceRequested` is opened by the force latch — which the absolute deadline
(and the pre-deadline reserve / second signal) trips. **Confirmed: absolute, not
per-phase.**

**"do not await graceful serially" — the exact race construction.** Verified.
`closeHttp` never writes `await graceful; …then force`; it writes
`await Promise.race([graceful ?? Promise.resolve(), this.forceRequested])`
(`:470`). `forceStopHttp` explicitly does **not** await `gracefulStopPromise`
before issuing `stop(true)` (`http.ts:3602-3605`). Both directions are pinned
behaviorally: `http-lifecycle.test.ts:755` proves a still-pending `stop(false)`
(held open by a live WebSocket) does not block `forceStop()` from settling, and
`daemon-resource-lifecycle.test.ts:261` proves the coordinator completes
closing-http as `Forced` while the graceful deferred is left unresolved.
**Confirmed: raced, not serial.**

**Nuance recorded (not a gap).** Clause G's "awaited" holds fully on the
graceful-*loses-the-race* paths the plan sentence describes — second signal or
pre-deadline reserve — where `closeHttp`/`releaseProcess` `await this.forceStop`
before the coordinator returns (`:480`/`:491`/`:526`), pinned by
`daemon-resource-lifecycle.test.ts:349`/`:428`. On a **hard absolute-deadline
exhaustion**, `runPhase` returns `TimedOut` once `budget.remainingMs() <= 0`
(`lifecycle-coordinator.ts:552`) without blocking on the still-pending
`stop(true)`; ownership is deliberately retained for the **synchronous host-exit
fallback** rather than an unbounded async await. That is the documented design,
and it is itself pinned by `daemon-resource-lifecycle.test.ts:375` ("hard
deadline retains process ownership for the synchronous host-exit fallback") and
`lifecycle-coordinator.test.ts:374`. So `stop(true)` is always **called**, and
**awaited** on every path except the one where the whole-daemon deadline has
already fired — by design, not omission.

---

## Gate results

Environment: `export PATH="$HOME/.bun/bin:$PATH"`; tests run as
`timeout 300 bun test <file> < /dev/null`.

| Gate | Command | Result |
|------|---------|--------|
| Cited tests (aggregate) | `bun test` of the four suites below | **41 pass / 0 fail** across 4 files |
| `tests/http-lifecycle.test.ts` | — | **11 pass / 0 fail** |
| `tests/effect/daemon-resource-lifecycle.test.ts` | — | **13 pass / 0 fail** |
| `tests/effect/lifecycle-coordinator.test.ts` | — | **16 pass / 0 fail** |
| `tests/daemon-lifecycle-integration.test.ts` | — | **1 pass / 0 fail** |
| Typecheck | `bun run typecheck` (`tsc --noEmit` root + board) | **clean** |
| CI | `bun run ci` (`biome ci`) | **408 files checked, no fixes, exit 0** |
| Bundle | not rebuilt | **N/A — no source changed** |

No new tests were added: every clause is already pinned, so a new
`tests/effect/http-graceful-stop.test.ts` would have been redundant.

---

## Verdict

All seven atomic clauses of P6.6 are **implemented at HEAD and pinned by existing
tests**, including the two the task singled out for scrutiny (absolute-vs-per-phase
deadline, and the non-serial race construction). **P6.6's box is honestly
checkable.** No source change, no new test, no bundle rebuild. One design nuance
is recorded above (async-await of `stop(true)` is intentionally skipped only on
hard absolute-deadline exhaustion, where the synchronous host-exit fallback owns
teardown); it is documented and pinned, and does not constitute an unmet clause.
