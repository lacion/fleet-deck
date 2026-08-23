// db-workflows/retention.ts — P8.6 pilot: root-context DB work that yields Store.
//
// This is the FIRST module of the P8.6 db-workflows family. It converts the
// retention schedule's DB seams from capability parameters into workflows that
// resolve the SQLite handle from the root Context by yielding the Store service.
//
// The convention this module establishes, stated once here for every later slice
// to copy:
//
//   * ROOT-CONTEXT ONLY yields Store. Retention already runs as a P5 Effect
//     schedule forked under the root runtime, where the root-published Store
//     service is reachable. Workflows reached through the HTTP bridge do NOT
//     yield Store — they keep their capabilities as parameters, per the P6.4
//     constraint (capabilities-as-params, R=never), because the request path is
//     not the root fiber. Yield Store only where the root owns the fiber.
//
//   * ONE COARSE operation per tick, never per statement. A synchronous sweep is
//     lifted with a SINGLE Effect.try around the whole existing function; an
//     already-owned Promise sweep is lifted with a SINGLE ownedLegacyPromise.
//     We do not wrap each `q.<stmt>` call. The sweep's SQL constants, row
//     mapping, and pure derivation do not move here and do not change — this
//     module only chooses where the handle comes from and where a failure is
//     translated.
//
//   * SYNC STAYS SYNC / NON-INTERRUPTIBLE. A synchronous query or transaction
//     blocks Bun's event loop; Effect cannot cancel it, so we never pretend it
//     can. The sync sweep runs to completion inside its one Effect.try thunk.
//     NO suspension or yielding is permitted inside a direct SQLite transaction
//     callback: `yield* Store` happens BEFORE the coarse operation, never within
//     the synchronous work.
//
//   * FAILURE TRANSLATED ONCE, at the P5 boundary's shape. Both operations reuse
//     retention-schedule.ts's `operationalError`, producing the exact same
//     BackgroundOperationalError (same `operation` tag, `message`, and `cause`)
//     the legacy adapter produces. The schedule's existing
//     catchTag('BackgroundOperationalError') boundary therefore observes a
//     byte-identical failure: fail-open skip for periodic, logged for boot. The
//     `yield* Store` prefix is a pure, infallible Context read that changes no
//     failure, defect, or interruption-join semantics.
//
// The handle is not bound yet (`yield* Store` declares the dependency but the
// pilot's sweeps still reach SQLite through the existing synchronous seams). Later
// slices that convert a sweep's internals will bind `const { handle } = yield* Store`
// and thread that handle instead of a captured closure.

import * as Effect from 'effect/Effect';

import { ownedLegacyPromise } from '../background-owner.ts';
import {
  type LegacyRetentionCallbacks,
  operationalError,
  type RetentionWork,
} from '../retention-schedule.ts';
import { Store } from '../services/store.ts';

/**
 * Store-backed retention work. Structurally identical to legacyRetentionWork,
 * except each operation first yields the root-owned Store service, so the work's
 * requirement is `Store` rather than `never`. The failure translation, the
 * coarse operation boundary, and the interruption-join behaviour are the same:
 * a synchronous prune throw becomes an operational failure via Effect.try, and
 * an already-owned sweep Promise joins its settlement before interruption
 * completes via ownedLegacyPromise. A synchronous Promise-factory throw remains
 * a defect, exactly as in the legacy adapter.
 */
export function makeStoreRetentionWork(callbacks: LegacyRetentionCallbacks): RetentionWork<Store> {
  const pruneEvents = callbacks.pruneEvents.bind(callbacks);
  const retentionSweep = callbacks.retentionSweep.bind(callbacks);
  return {
    pruneEvents: (cutoffMs) =>
      Effect.gen(function* () {
        // Declare the root-owned Store dependency. The synchronous prune runs
        // whole inside the one Effect.try below — no yielding inside the work.
        yield* Store;
        return yield* Effect.try({
          try: () => pruneEvents(cutoffMs),
          catch: (cause) => operationalError('prune-events', cause),
        });
      }),
    retentionSweep: (nowMs) =>
      Effect.gen(function* () {
        yield* Store;
        return yield* ownedLegacyPromise({
          try: () => retentionSweep(nowMs),
          catch: (cause) => operationalError('retention-sweep', cause),
        });
      }),
  };
}
