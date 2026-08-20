import * as Effect from 'effect/Effect';

import { type BackgroundController, ownedLegacyPromise } from './background-owner.ts';
import { BackgroundOperationalError } from './services/background.ts';

export type BootReconciliationOperation =
  | 'clear-fork-healing'
  | 'spawn-reconciliation'
  | 'first-retention'
  | 'broadcast-idle';

export interface BootReconciliationOperationalFailure {
  readonly operation: BootReconciliationOperation;
  readonly error: BackgroundOperationalError;
}

export interface BootReconciliationResult {
  readonly degraded: boolean;
  readonly failures: readonly BootReconciliationOperationalFailure[];
}

export interface BootReconciliationWork<Environment> {
  /** Synchronous clear-fork healing must complete before either asynchronous leg starts. */
  readonly clearForkHealing: Effect.Effect<void, BackgroundOperationalError, Environment>;
  readonly reconcileSpawns: Effect.Effect<unknown, BackgroundOperationalError, Environment>;
  readonly firstRetention: Effect.Effect<unknown, BackgroundOperationalError, Environment>;
  /** Drains the coalesced mutation flush after both middle legs have settled. */
  readonly awaitBroadcastIdle: Effect.Effect<void, BackgroundOperationalError, Environment>;
}

export interface BootReconciliationOptions<Environment>
  extends BootReconciliationWork<Environment> {
  /** Integration logs/degrades named failures; defects from the observer remain defects. */
  readonly onOperationalFailure?: (
    failure: BootReconciliationOperationalFailure,
  ) => Effect.Effect<void, never, Environment>;
}

export interface LegacyBootReconciliationCallbacks {
  readonly clearForkHealing: () => void;
  readonly reconcileSpawns: () => PromiseLike<unknown>;
  readonly firstRetention: () => PromiseLike<unknown>;
  readonly awaitBroadcastIdle: () => PromiseLike<void>;
}

export type LegacyBootReconciliationWithoutRetentionCallbacks = Omit<
  LegacyBootReconciliationCallbacks,
  'firstRetention'
>;

function operationalError(
  operation: BootReconciliationOperation,
  cause: unknown,
): BackgroundOperationalError {
  return new BackgroundOperationalError({
    operation: `boot-${operation}`,
    cause,
    message: `fleetd boot ${operation} failed`,
  });
}

function legacyPromise<A>(
  operation: BootReconciliationOperation,
  factory: () => PromiseLike<A>,
): Effect.Effect<A, BackgroundOperationalError> {
  return ownedLegacyPromise({
    try: factory,
    catch: (cause) => operationalError(operation, cause),
  });
}

/**
 * Temporary adapter for the existing core/HTTP boot seams.
 *
 * The synchronous clear-fork callback keeps its historical fail-open mapping.
 * Promise rejection is operational; a synchronous throw while constructing an
 * asynchronous Promise remains a defect, matching ownedLegacyPromise.
 */
export function legacyBootReconciliationWork(
  callbacks: LegacyBootReconciliationCallbacks,
): BootReconciliationWork<never> {
  return {
    ...legacyBootReconciliationWithoutRetentionWork(callbacks),
    firstRetention: legacyPromise('first-retention', callbacks.firstRetention.bind(callbacks)),
  };
}

/**
 * P5 production adapter. Retention is supplied by the one Effect-owned schedule,
 * so the boot workflow cannot accidentally retain a second callback for it.
 */
export function legacyBootReconciliationWithoutRetentionWork(
  callbacks: LegacyBootReconciliationWithoutRetentionCallbacks,
): Omit<BootReconciliationWork<never>, 'firstRetention'> {
  const clearForkHealing = callbacks.clearForkHealing.bind(callbacks);
  const reconcileSpawns = callbacks.reconcileSpawns.bind(callbacks);
  const awaitBroadcastIdle = callbacks.awaitBroadcastIdle.bind(callbacks);

  return {
    clearForkHealing: Effect.try({
      try: clearForkHealing,
      catch: (cause) => operationalError('clear-fork-healing', cause),
    }),
    reconcileSpawns: legacyPromise('spawn-reconciliation', reconcileSpawns),
    awaitBroadcastIdle: legacyPromise('broadcast-idle', awaitBroadcastIdle),
  };
}

function attempt<Environment>(
  operation: BootReconciliationOperation,
  work: Effect.Effect<unknown, BackgroundOperationalError, Environment>,
): Effect.Effect<BootReconciliationOperationalFailure | null, never, Environment> {
  return Effect.suspend(() => work).pipe(
    Effect.as(null as BootReconciliationOperationalFailure | null),
    Effect.catchTag('BackgroundOperationalError', (error) => Effect.succeed({ operation, error })),
  );
}

function recordFailure<Environment>(
  failures: BootReconciliationOperationalFailure[],
  failure: BootReconciliationOperationalFailure | null,
  observer:
    | ((failure: BootReconciliationOperationalFailure) => Effect.Effect<void, never, Environment>)
    | undefined,
): Effect.Effect<void, never, Environment> {
  if (!failure) return Effect.void;
  return Effect.sync(() => {
    failures.push(failure);
  }).pipe(Effect.andThen(observer ? Effect.suspend(() => observer(failure)) : Effect.void));
}

/**
 * One-shot structured boot workflow for the shared Background owner.
 *
 * Only named BackgroundOperationalError failures degrade. Unknown defects are
 * deliberately not caught, so the Background owner's awaitFailure observes the
 * original defect. The controller publishes `settled` synchronously before its
 * Deferred wakes readiness waiters.
 */
export function makeBootReconciliationProgram<Environment>(
  controller: BackgroundController,
  options: BootReconciliationOptions<Environment>,
): Effect.Effect<BootReconciliationResult, never, Environment> {
  const resolved = {
    clearForkHealing: options.clearForkHealing,
    reconcileSpawns: options.reconcileSpawns,
    firstRetention: options.firstRetention,
    awaitBroadcastIdle: options.awaitBroadcastIdle,
    onOperationalFailure: options.onOperationalFailure,
  };

  return Effect.gen(function* () {
    const failures: BootReconciliationOperationalFailure[] = [];

    const clearForkFailure = yield* attempt('clear-fork-healing', resolved.clearForkHealing);
    yield* recordFailure(failures, clearForkFailure, resolved.onOperationalFailure);

    const middleFailures = yield* Effect.all(
      [
        attempt('spawn-reconciliation', resolved.reconcileSpawns),
        attempt('first-retention', resolved.firstRetention),
      ],
      { concurrency: 'unbounded' },
    );
    for (const failure of middleFailures) {
      yield* recordFailure(failures, failure, resolved.onOperationalFailure);
    }

    const broadcastFailure = yield* attempt('broadcast-idle', resolved.awaitBroadcastIdle);
    yield* recordFailure(failures, broadcastFailure, resolved.onOperationalFailure);

    yield* controller.markReconciliationReady;
    return {
      degraded: failures.length > 0,
      failures: Object.freeze([...failures]),
    };
  });
}
