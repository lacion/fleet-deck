// db-workflows/lan-tick.ts — P8.6 slice 4: the LAN-refresh feed tick yields Store.
//
// Fourth module of the P8.6 db-workflows family. The canonical convention — the
// four rules (root-context-only yields Store; ONE coarse operation per tick,
// never per statement; sync-stays-sync with `yield* Store` BEFORE the coarse op,
// never inside a transaction callback; failure translated ONCE so the boundary
// sees a byte-identical failure) — lives verbatim at db-workflows/retention.ts.
// Read it there; this header only records what is lan-tick-specific.
//
// Lan-tick-specific notes:
//
//   * ONE root-context leg. The LAN refresh loop runs as a P5 daemon-long
//     schedule forked under the shared Background owner — the root fiber — so its
//     feed tick yields Store, exactly as the retention pilot yields it. The two
//     `q.<stmt>` calls inside core.tick (insertTicker + trimTicker) are the SQLite
//     seam; they do not move here and do not change.
//
//   * COARSE boundary mirrors the pre-existing inline swallow one-for-one. The
//     whole synchronous core.tick runs inside the single Effect.try, and a throw
//     becomes a LanTickError. The wiring site (program.ts) catches that tag and
//     drops it — byte-identical to the prior `try { core.tick(...) } catch {}`
//     silent swallow: the feed line is non-essential and its failure never
//     load-bears LAN discovery, never logs, and never propagates.
//
//   * Unlike retention/boot, the LAN tick has no pre-existing capability-home
//     module, so this family module defines the whole seam — the error, the work
//     type, and BOTH the legacy (R = never) and store-backed (R = Store) adapters.
//     The legacy adapter is the P8.6 rollback path; both translate a tick throw
//     through the identical LanTickError, so the swallow boundary is byte-identical
//     either way.
//
//   * The `yield* Store` prefix is a pure, infallible Context read: it changes no
//     failure, defect, or swallow semantics. It only lifts the work's requirement
//     from `never` to `Store`, discharged once by the whole-gen provideService(Store)
//     the background program already performs.

import * as Data from 'effect/Data';
import * as Effect from 'effect/Effect';

import { Store } from '../services/store.ts';

export class LanTickError extends Data.TaggedError('LanTickError')<{
  readonly cause: unknown;
}> {}

/** The single DB-touching leg of a LAN-address-change feed tick. */
export interface LanTickCallbacks {
  readonly tick: (message: string) => void;
}

/**
 * The coarse LAN feed-tick operation, parameterized over its Effect environment.
 * The legacy adapter requires nothing (`never`); the store-backed adapter yields
 * the root-owned Store first, so its requirement is `Store`. Both translate a
 * synchronous tick throw into the identical LanTickError; the wiring site's
 * catchTag swallow drops that tag either way.
 */
export type LanTickWork<Environment> = (
  message: string,
) => Effect.Effect<void, LanTickError, Environment>;

/**
 * Capability-parameterized LAN feed tick (R = never): the existing synchronous
 * core.tick wrapped in the one coarse Effect.try. This is the P8.6 rollback path
 * for the store-backed adapter.
 */
export function legacyLanTickWork(callbacks: LanTickCallbacks): LanTickWork<never> {
  const tick = callbacks.tick.bind(callbacks);
  return (message) =>
    Effect.try({
      try: () => tick(message),
      catch: (cause) => new LanTickError({ cause }),
    });
}

/**
 * Store-backed LAN feed tick. Structurally identical to legacyLanTickWork, except
 * it first yields the root-owned Store service, so the work's requirement is
 * `Store` rather than `never`. The coarse operation boundary and the failure
 * translation are the same: a synchronous tick throw becomes the identical
 * LanTickError via the one Effect.try, which the wiring site's catchTag swallow
 * drops by tag either way.
 */
export function makeStoreLanTickWork(callbacks: LanTickCallbacks): LanTickWork<Store> {
  const tick = callbacks.tick.bind(callbacks);
  return (message) =>
    Effect.gen(function* () {
      // Declare the root-owned Store dependency. The synchronous tick runs whole
      // inside the one Effect.try below — no yielding inside the work.
      yield* Store;
      return yield* Effect.try({
        try: () => tick(message),
        catch: (cause) => new LanTickError({ cause }),
      });
    });
}
