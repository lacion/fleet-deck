import assert from 'node:assert/strict';

import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Deferred from 'effect/Deferred';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';

import { makeBackgroundOwner } from '../../src/daemon/app/background-owner.ts';
import {
  type BootReconciliationOperation,
  type BootReconciliationResult,
  type BootReconciliationWork,
  legacyBootReconciliationWithoutRetentionWork,
  makeBootReconciliationProgram,
} from '../../src/daemon/app/boot-reconciliation.ts';
import { makeStoreBootReconciliationWork } from '../../src/daemon/app/db-workflows/boot.ts';
import { Store } from '../../src/daemon/app/services/store.ts';
import { makeStoreOwner } from '../../src/daemon/app/store-owner.ts';
import type { SqliteHandle } from '../../src/daemon/sqlite.ts';
import { runEffectExit } from './helpers.ts';

/**
 * A fake SQLite handle that throws on every query path. Like the retention
 * pilot, this slice only YIELDS Store to declare the dependency; it must not
 * touch the handle yet, so any accidental handle access surfaces immediately as
 * a test defect. Later slices that thread `handle` will replace this with a real
 * scratch connection.
 */
function fakeHandle(): SqliteHandle {
  const forbid = (): never => {
    throw new Error('P8.6 boot workflow must not touch the SQLite handle yet');
  };
  return {
    exec: forbid,
    prepare: forbid,
    finalizeStatements: () => {},
    close: () => {},
  };
}

/** The real root-owned Store service shape, over the untouched fake handle. */
function boundService() {
  return makeStoreOwner({ name: 'sqlite', handle: fakeHandle() }).service;
}

/** Extract the single typed failure from an Exit, using the codebase's Cause idiom. */
function soleFailure<E>(exit: Exit.Exit<unknown, E>): E {
  assert.ok(Exit.isFailure(exit), 'expected a typed operational failure');
  const errors = exit.cause.reasons.filter(Cause.isFailReason).map((reason) => reason.error);
  const [error, ...rest] = errors;
  assert.ok(error, 'expected exactly one typed failure');
  assert.equal(rest.length, 0, 'expected exactly one typed failure');
  return error;
}

interface BootObservation {
  readonly status: string;
  readonly degraded: boolean;
  readonly failures: readonly {
    readonly operation: BootReconciliationOperation;
    readonly cause: unknown;
  }[];
  readonly observed: readonly BootReconciliationOperation[];
}

/**
 * Run one boot work object through the real makeBootReconciliationProgram under a
 * Background owner, discharging Store once at the fiber boundary exactly as
 * program.ts does. Legacy work (Environment `never`) is assignable to the
 * `Store` parameter (R is covariant), so the same helper drives both adapters
 * and their fail-open outcomes can be compared byte-for-byte.
 */
async function runBoot(
  work: Omit<BootReconciliationWork<Store>, 'firstRetention'>,
): Promise<BootObservation> {
  const observed: BootReconciliationOperation[] = [];
  let result: BootReconciliationResult | null = null;

  const scenario = Effect.scoped(
    Effect.gen(function* () {
      const resultPublished = yield* Deferred.make<void>();
      const registration = yield* makeBackgroundOwner({
        name: 'boot-store-failopen',
        run: (controller) =>
          makeBootReconciliationProgram(controller, {
            ...work,
            // The retention gate supplies firstRetention in production; here a
            // benign success stands in for it so the store-backed legs are the
            // only variable under test.
            firstRetention: Effect.void,
            onOperationalFailure: (failure) =>
              Effect.sync(() => {
                observed.push(failure.operation);
              }),
          }).pipe(
            Effect.tap((value) =>
              Effect.sync(() => {
                result = value;
              }).pipe(Effect.andThen(Deferred.succeed(resultPublished, undefined))),
            ),
            Effect.andThen(Effect.never),
          ),
      });

      yield* registration.service.awaitReady;
      const status = registration.service.reconciliationStatus();
      yield* Deferred.await(resultPublished);
      yield* Effect.promise(() => registration.owner.close());
      // Read `result` inside the generator, where the captured let keeps its
      // declared BootReconciliationResult | null type. A synchronous read at the
      // outer scope would be control-flow-narrowed to its `null` initializer,
      // because every write happens inside the tap closure.
      const value = result;
      assert.ok(value, 'boot result was not published');
      return {
        status,
        degraded: value.degraded,
        failures: value.failures.map(({ operation, error }) => ({
          operation,
          cause: error.cause,
        })),
        observed: [...observed],
      };
    }),
  );

  const exit = await runEffectExit(scenario.pipe(Effect.provideService(Store, boundService())));
  assert.ok(Exit.isSuccess(exit), Exit.isFailure(exit) ? Cause.pretty(exit.cause) : '');
  return exit.value;
}

describe('P8.6 store-backed boot reconciliation work', () => {
  test('yields Store and threads success through the sync heal and both owned promises', async () => {
    const calls: string[] = [];
    const work = makeStoreBootReconciliationWork({
      clearForkHealing() {
        calls.push('clear');
      },
      reconcileSpawns() {
        calls.push('spawn');
        return Promise.resolve('reconciled');
      },
      awaitBroadcastIdle() {
        calls.push('broadcast');
        return Promise.resolve();
      },
    });
    const service = boundService();

    const clearExit = await runEffectExit(
      work.clearForkHealing.pipe(Effect.provideService(Store, service)),
    );
    assert.ok(
      Exit.isSuccess(clearExit),
      Exit.isFailure(clearExit) ? Cause.pretty(clearExit.cause) : '',
    );

    const spawnExit = await runEffectExit(
      work.reconcileSpawns.pipe(Effect.provideService(Store, service)),
    );
    assert.ok(Exit.isSuccess(spawnExit) && spawnExit.value === 'reconciled');

    const broadcastExit = await runEffectExit(
      work.awaitBroadcastIdle.pipe(Effect.provideService(Store, service)),
    );
    assert.ok(Exit.isSuccess(broadcastExit));
    assert.deepEqual(calls, ['clear', 'spawn', 'broadcast']);

    // Negative pin for the family convention itself: without the discharge the
    // sync clear-fork leg must DIE on the missing Store service. R is covariant,
    // so deleting the `yield* Store` line would still typecheck and every
    // provide-wrapped assertion above would still pass — this is the one case
    // that fails if the yield is removed.
    const undischargedExit = await runEffectExit(
      work.clearForkHealing as Effect.Effect<void, never, never>,
    );
    assert.ok(Exit.isFailure(undischargedExit) && Cause.hasDies(undischargedExit.cause));
    assert.match(Cause.pretty(undischargedExit.cause), /Service not found.*Store/);
    assert.deepEqual(
      calls,
      ['clear', 'spawn', 'broadcast'],
      'the undischarged heal must never reach the callback',
    );
  });

  test('a synchronous clear-fork throw becomes the identical operational failure as the legacy adapter', async () => {
    const boom = new Error('clear-fork blew up');
    const storeWork = makeStoreBootReconciliationWork({
      clearForkHealing() {
        throw boom;
      },
      reconcileSpawns: () => Promise.resolve(),
      awaitBroadcastIdle: () => Promise.resolve(),
    });
    const legacyWork = legacyBootReconciliationWithoutRetentionWork({
      clearForkHealing() {
        throw boom;
      },
      reconcileSpawns: () => Promise.resolve(),
      awaitBroadcastIdle: () => Promise.resolve(),
    });

    const storeErr = soleFailure(
      await runEffectExit(
        storeWork.clearForkHealing.pipe(Effect.provideService(Store, boundService())),
      ),
    );
    const legacyErr = soleFailure(await runEffectExit(legacyWork.clearForkHealing));

    assert.equal(storeErr._tag, 'BackgroundOperationalError');
    assert.equal(storeErr.operation, 'boot-clear-fork-healing');
    assert.equal(storeErr.message, 'fleetd boot clear-fork-healing failed');
    assert.equal(storeErr.cause, boom);
    // Byte-parity with the legacy capability-parameterized adapter: same tag,
    // operation, message, and preserved cause.
    assert.deepEqual(
      [storeErr._tag, storeErr.operation, storeErr.message, storeErr.cause],
      [legacyErr._tag, legacyErr.operation, legacyErr.message, legacyErr.cause],
    );
  });

  test('a spawn or broadcast rejection becomes the identical operational failure as the legacy adapter', async () => {
    const spawnRejection = new Error('spawn rejected');
    const broadcastRejection = new Error('broadcast rejected');
    const callbacks = {
      clearForkHealing: () => undefined,
      reconcileSpawns: () => Promise.reject(spawnRejection),
      awaitBroadcastIdle: () => Promise.reject(broadcastRejection),
    };
    const storeWork = makeStoreBootReconciliationWork(callbacks);
    const legacyWork = legacyBootReconciliationWithoutRetentionWork(callbacks);

    const storeSpawnErr = soleFailure(
      await runEffectExit(
        storeWork.reconcileSpawns.pipe(Effect.provideService(Store, boundService())),
      ),
    );
    const legacySpawnErr = soleFailure(await runEffectExit(legacyWork.reconcileSpawns));
    assert.equal(storeSpawnErr.operation, 'boot-spawn-reconciliation');
    assert.equal(storeSpawnErr.message, 'fleetd boot spawn-reconciliation failed');
    assert.equal(storeSpawnErr.cause, spawnRejection);
    assert.deepEqual(
      [storeSpawnErr._tag, storeSpawnErr.operation, storeSpawnErr.message, storeSpawnErr.cause],
      [legacySpawnErr._tag, legacySpawnErr.operation, legacySpawnErr.message, legacySpawnErr.cause],
    );

    const storeBroadcastErr = soleFailure(
      await runEffectExit(
        storeWork.awaitBroadcastIdle.pipe(Effect.provideService(Store, boundService())),
      ),
    );
    const legacyBroadcastErr = soleFailure(await runEffectExit(legacyWork.awaitBroadcastIdle));
    assert.equal(storeBroadcastErr.operation, 'boot-broadcast-idle');
    assert.equal(storeBroadcastErr.message, 'fleetd boot broadcast-idle failed');
    assert.equal(storeBroadcastErr.cause, broadcastRejection);
    assert.deepEqual(
      [
        storeBroadcastErr._tag,
        storeBroadcastErr.operation,
        storeBroadcastErr.message,
        storeBroadcastErr.cause,
      ],
      [
        legacyBroadcastErr._tag,
        legacyBroadcastErr.operation,
        legacyBroadcastErr.message,
        legacyBroadcastErr.cause,
      ],
    );
  });

  test('a synchronous spawn-factory throw is a defect in both the store-backed and legacy work', async () => {
    const boom = new Error('spawn factory threw');
    const callbacks = {
      clearForkHealing: () => undefined,
      reconcileSpawns() {
        throw boom;
      },
      awaitBroadcastIdle: () => Promise.resolve(),
    };

    const storeExit = await runEffectExit(
      makeStoreBootReconciliationWork(callbacks).reconcileSpawns.pipe(
        Effect.provideService(Store, boundService()),
      ),
    );
    const legacyExit = await runEffectExit(
      legacyBootReconciliationWithoutRetentionWork(callbacks).reconcileSpawns,
    );

    for (const exit of [storeExit, legacyExit]) {
      assert.ok(Exit.isFailure(exit));
      assert.equal(Exit.hasFails(exit), false);
      assert.equal(Exit.hasDies(exit), true);
      assert.equal(Cause.squash(exit.cause), boom);
    }
  });

  test('the program.ts wiring runs the store-backed legs in order and settles readiness (Store provided once)', async () => {
    const events: string[] = [];
    const work = makeStoreBootReconciliationWork({
      clearForkHealing() {
        events.push('clear');
      },
      reconcileSpawns() {
        events.push('spawn');
        return Promise.resolve();
      },
      awaitBroadcastIdle() {
        events.push('broadcast');
        return Promise.resolve();
      },
    });
    let result: BootReconciliationResult | null = null;

    const scenario = Effect.scoped(
      Effect.gen(function* () {
        const resultPublished = yield* Deferred.make<void>();
        const registration = yield* makeBackgroundOwner({
          name: 'boot-store-wiring',
          run: (controller) =>
            makeBootReconciliationProgram(controller, {
              ...work,
              firstRetention: Effect.sync(() => {
                events.push('retention');
              }),
            }).pipe(
              Effect.tap((value) =>
                Effect.sync(() => {
                  result = value;
                }).pipe(Effect.andThen(Deferred.succeed(resultPublished, undefined))),
              ),
              Effect.andThen(Effect.never),
            ),
        });

        assert.equal(registration.service.reconciliationStatus(), 'reconciling');
        yield* registration.service.awaitReady;
        yield* Deferred.await(resultPublished);
        assert.equal(registration.service.reconciliationStatus(), 'settled');
        // Read `result` inside the generator, where the captured let keeps its
        // declared type instead of being narrowed to its null initializer.
        assert.equal(result?.degraded, false);
        assert.deepEqual(result?.failures, []);
        yield* Effect.promise(() => registration.owner.close());
      }),
    );

    const exit = await runEffectExit(scenario.pipe(Effect.provideService(Store, boundService())));
    assert.ok(Exit.isSuccess(exit), Exit.isFailure(exit) ? Cause.pretty(exit.cause) : '');

    // Clear-fork healing runs first; the broadcast flush drains last; spawn and
    // the retention gate run concurrently between them.
    assert.equal(events[0], 'clear');
    assert.equal(events.at(-1), 'broadcast');
    assert.ok(events.includes('spawn') && events.includes('retention'));
  });

  test('a store-backed boot leg rejection settles readiness fail-open, identical to the legacy adapter', async () => {
    const spawnRejection = new Error('spawn rejected');
    const callbacks = {
      clearForkHealing: () => undefined,
      reconcileSpawns: () => Promise.reject(spawnRejection),
      awaitBroadcastIdle: () => Promise.resolve(),
    };

    const storeObservation = await runBoot(makeStoreBootReconciliationWork(callbacks));
    const legacyObservation = await runBoot(
      legacyBootReconciliationWithoutRetentionWork(callbacks),
    );

    // Fail-open boot: readiness settles, the boot failure is observed (not a
    // defect that kills the fiber), and the workflow completes degraded.
    assert.equal(storeObservation.status, 'settled');
    assert.equal(storeObservation.degraded, true);
    assert.deepEqual(storeObservation.observed, ['spawn-reconciliation']);
    assert.deepEqual(storeObservation.failures, [
      { operation: 'spawn-reconciliation', cause: spawnRejection },
    ]);
    // Byte-parity with the legacy capability-parameterized adapter: the yield*
    // Store prefix changes nothing about the fail-open readiness boundary.
    assert.deepEqual(storeObservation, legacyObservation);
  });
});
