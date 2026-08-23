// db-workflows/spawn-liveness.ts — P8.6 slice 5: the spawn-liveness tick yields Store.
//
// Fifth and final module of the P8.6 db-workflows family. The canonical
// convention — the four rules (root-context-only yields Store; ONE coarse
// operation per tick, never per statement; sync-stays-sync with `yield* Store`
// BEFORE the coarse op, never inside a transaction callback; failure translated
// ONCE so the boundary sees a byte-identical failure) — lives verbatim at
// db-workflows/retention.ts. Read it there; this header only records what is
// spawn-liveness-specific.
//
// Spawn-liveness-specific notes:
//
//   * ONE root-context leg, and the largest. The agents-poll scheduler runs as a
//     P5 daemon-long schedule forked under the shared Background owner — the root
//     fiber — so its liveness tick yields Store, exactly as the retention pilot
//     yields it. The spawn-row and tombstoning `q.<stmt>` seams that spawns.ts's
//     spawnLivenessTick reaches (and the adjacent BUG-040 claimPlanExecution
//     atomicity constraint) do not move here and do not change: this module only
//     chooses where the handle comes from and where a fault is translated.
//
//   * The join/cancel machinery is load-bearing and is NOT reimplemented here.
//     Unlike the retention/boot/lan-tick sweeps, the liveness callback is
//     genuinely async and join-owned: `spawnLivenessTick` may return a Promise,
//     and close must not retire downstream resources while a tick is in flight.
//     Both adapters share the SINGLE `ownedLivenessTick` helper in agents-poll.ts
//     (the Promise callback bridge whose interruption finalizer joins the admitted
//     callback), so the no-overlap / join-on-interrupt semantics are byte-identical
//     to the legacy path. This module adds ONLY the `yield* Store` prefix.
//
//   * FAILURE POLICY is a named fail-open skip, byte-identical either side. A
//     synchronous throw AND an async rejection both become the identical
//     AgentsPollLivenessError via ownedLivenessTick; the scheduler's runLiveness
//     boundary catches that tag and drops it, so the tick failure never logs,
//     never propagates, and never stops the loop. A missing-Store defect is NOT
//     that tag and deliberately survives — the family's negative pin.
//
//   * The `yield* Store` prefix is a pure, infallible Context read: it changes no
//     failure, defect, join, or swallow semantics. It only lifts the work's
//     requirement from `never` to `Store`, discharged once by the whole-gen
//     provideService(Store) the background program already performs.

import * as Effect from 'effect/Effect';

import { type LivenessCallbacks, type LivenessWork, ownedLivenessTick } from '../agents-poll.ts';
import { Store } from '../services/store.ts';

/**
 * Store-backed spawn-liveness tick. Structurally identical to legacyLivenessWork,
 * except it first yields the root-owned Store service, so the work's requirement
 * is `Store` rather than `never`. When no callback is present it short-circuits to
 * Effect.void (R = never), byte-identical to the legacy adapter. The active path
 * reuses the shared ownedLivenessTick machinery, so the join/cancel behaviour and
 * the AgentsPollLivenessError translation are the same; the scheduler's fail-open
 * runLiveness boundary drops that tag either way.
 */
export function makeStoreLivenessWork(callbacks: LivenessCallbacks): LivenessWork<Store> {
  const callback = callbacks.spawnLivenessTick?.bind(callbacks);
  if (callback === undefined) return Effect.void;
  return Effect.gen(function* () {
    // Declare the root-owned Store dependency. The whole liveness tick runs under
    // the shared ownedLivenessTick join machinery below — the `yield* Store` read
    // happens BEFORE that coarse operation, never inside it.
    yield* Store;
    return yield* ownedLivenessTick(callback);
  });
}
