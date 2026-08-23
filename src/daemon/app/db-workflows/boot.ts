// db-workflows/boot.ts — P8.6 slice 2: boot reconciliation yields Store.
//
// Second module of the P8.6 db-workflows family. The canonical convention — the
// four rules (root-context-only yields Store; ONE coarse operation per leg, never
// per statement; sync-stays-sync with `yield* Store` BEFORE the coarse op, never
// inside a transaction callback; failure translated ONCE through the shared
// `operationalError` so the workflow's BackgroundOperationalError boundary sees a
// byte-identical failure) — lives verbatim at db-workflows/retention.ts. Read it
// there; this header only records what is boot-specific.
//
// Boot-specific notes:
//
//   * THREE legs, all root-context. Boot reconciliation runs as the P5 one-shot
//     workflow forked under the shared Background owner — the root fiber, where
//     the root-published Store service is reachable — so every leg yields Store,
//     exactly as the retention pilot yields it in each of its two ops. This is
//     the convention applied, not a per-leg judgement about SQLite: clear-fork
//     healing and spawn reconciliation are SQLite seams, while broadcast-idle
//     drains the coalesced mutation flush (BUG-066) and touches no SQLite, but it
//     is the same root-owned boot workflow, so it declares the dependency
//     uniformly rather than special-casing one leg.
//
//   * COARSE boundary per leg mirrors the legacy adapter one-for-one: clear-fork
//     healing is the single synchronous Effect.try; spawn reconciliation and
//     broadcast-idle are each a single ownedLegacyPromise (interruption joins the
//     admitted Promise; a synchronous Promise-factory throw stays a defect).
//
//   * firstRetention is NOT a leg here. The one Effect-owned retention schedule
//     supplies it through makeDaemonBackgroundProgram's private gate, so the work
//     this module builds is `Omit<BootReconciliationWork<Store>, 'firstRetention'>`
//     — the store-backed twin of legacyBootReconciliationWithoutRetentionWork.
//     Fail-open readiness is unchanged: the schedule still catches every named
//     BackgroundOperationalError and settles readiness regardless (P5).

import * as Effect from 'effect/Effect';

import { ownedLegacyPromise } from '../background-owner.ts';
import {
  type BootReconciliationWork,
  type LegacyBootReconciliationWithoutRetentionCallbacks,
  operationalError,
} from '../boot-reconciliation.ts';
import { Store } from '../services/store.ts';

/**
 * Store-backed boot reconciliation work. Structurally identical to
 * legacyBootReconciliationWithoutRetentionWork, except each leg first yields the
 * root-owned Store service, so the work's requirement is `Store` rather than
 * `never`. The coarse operation boundary, the failure translation, and the
 * interruption-join behaviour are the same: a synchronous clear-fork throw
 * becomes an operational failure via Effect.try, and each already-owned Promise
 * joins its settlement before interruption completes via ownedLegacyPromise. A
 * synchronous Promise-factory throw remains a defect, exactly as in the legacy
 * adapter.
 */
export function makeStoreBootReconciliationWork(
  callbacks: LegacyBootReconciliationWithoutRetentionCallbacks,
): Omit<BootReconciliationWork<Store>, 'firstRetention'> {
  const clearForkHealing = callbacks.clearForkHealing.bind(callbacks);
  const reconcileSpawns = callbacks.reconcileSpawns.bind(callbacks);
  const awaitBroadcastIdle = callbacks.awaitBroadcastIdle.bind(callbacks);
  return {
    clearForkHealing: Effect.gen(function* () {
      // Declare the root-owned Store dependency. The synchronous heal runs whole
      // inside the one Effect.try below — no yielding inside the work.
      yield* Store;
      return yield* Effect.try({
        try: clearForkHealing,
        catch: (cause) => operationalError('clear-fork-healing', cause),
      });
    }),
    reconcileSpawns: Effect.gen(function* () {
      yield* Store;
      return yield* ownedLegacyPromise({
        try: reconcileSpawns,
        catch: (cause) => operationalError('spawn-reconciliation', cause),
      });
    }),
    awaitBroadcastIdle: Effect.gen(function* () {
      yield* Store;
      return yield* ownedLegacyPromise({
        try: awaitBroadcastIdle,
        catch: (cause) => operationalError('broadcast-idle', cause),
      });
    }),
  };
}
