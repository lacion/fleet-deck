import assert from 'node:assert/strict';

import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Fiber from 'effect/Fiber';

import { makeStoreRetentionWork } from '../../src/daemon/app/db-workflows/retention.ts';
import {
  legacyRetentionWork,
  makeRetentionSchedule,
  RETENTION_EVENT_WINDOW_MS,
  type RetentionOperationalFailure,
} from '../../src/daemon/app/retention-schedule.ts';
import { Store } from '../../src/daemon/app/services/store.ts';
import { makeStoreOwner } from '../../src/daemon/app/store-owner.ts';
import type { SqliteHandle } from '../../src/daemon/sqlite.ts';
import { runEffectExit, TestClock, TestServicesLayer } from './helpers.ts';

/**
 * A fake SQLite handle that throws on every query path. The P8.6 pilot only
 * YIELDS Store to declare the dependency; it must not touch the handle yet, so
 * any accidental handle access surfaces immediately as a test defect. Later
 * slices that thread `handle` will replace this with a real scratch connection.
 */
function fakeHandle(): SqliteHandle {
  const forbid = (): never => {
    throw new Error('P8.6 pilot retention workflow must not touch the SQLite handle yet');
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

describe('P8.6 store-backed retention work', () => {
  test('yields Store and threads success through the existing sync prune and owned sweep', async () => {
    const cutoffs: number[] = [];
    const sweepArgs: number[] = [];
    const work = makeStoreRetentionWork({
      pruneEvents(cutoffMs) {
        cutoffs.push(cutoffMs);
      },
      retentionSweep(nowMs) {
        sweepArgs.push(nowMs);
        return Promise.resolve('swept');
      },
    });

    // The requirement is Store: each operation is only runnable once Store is
    // discharged — exactly the local provide program.ts performs on the fiber.
    const service = boundService();
    const pruneExit = await runEffectExit(
      work.pruneEvents(111).pipe(Effect.provideService(Store, service)),
    );
    assert.ok(
      Exit.isSuccess(pruneExit),
      Exit.isFailure(pruneExit) ? Cause.pretty(pruneExit.cause) : '',
    );
    assert.deepEqual(cutoffs, [111]);

    const sweepExit = await runEffectExit(
      work.retentionSweep(222).pipe(Effect.provideService(Store, service)),
    );
    assert.ok(Exit.isSuccess(sweepExit) && sweepExit.value === 'swept');
    assert.deepEqual(sweepArgs, [222]);

    // Negative pin for the family convention itself: without the discharge the
    // operation must DIE on the missing Store service. R is covariant, so
    // deleting the `yield* Store` lines would still typecheck and every
    // provide-wrapped assertion above would still pass — this is the one case
    // that fails if the yield is removed.
    const undischargedExit = await runEffectExit(
      work.pruneEvents(333) as Effect.Effect<void, never, never>,
    );
    assert.ok(Exit.isFailure(undischargedExit) && Cause.hasDies(undischargedExit.cause));
    assert.match(Cause.pretty(undischargedExit.cause), /Service not found.*Store/);
    assert.deepEqual(cutoffs, [111], 'the undischarged prune must never reach the callback');
  });

  test('a synchronous prune throw becomes the identical operational failure as the legacy adapter', async () => {
    const boom = new Error('prune blew up');
    const storeWork = makeStoreRetentionWork({
      pruneEvents() {
        throw boom;
      },
      retentionSweep() {
        return Promise.resolve();
      },
    });
    const legacyWork = legacyRetentionWork({
      pruneEvents() {
        throw boom;
      },
      retentionSweep() {
        return Promise.resolve();
      },
    });

    const storeErr = soleFailure(
      await runEffectExit(
        storeWork.pruneEvents(9).pipe(Effect.provideService(Store, boundService())),
      ),
    );
    const legacyErr = soleFailure(await runEffectExit(legacyWork.pruneEvents(9)));

    assert.equal(storeErr._tag, 'BackgroundOperationalError');
    assert.equal(storeErr.operation, 'retention-prune-events');
    assert.equal(storeErr.message, 'fleetd retention prune-events failed');
    assert.equal(storeErr.cause, boom);
    // Byte-parity with the legacy capability-parameterized adapter: same tag,
    // operation, message, and preserved cause.
    assert.deepEqual(
      [storeErr._tag, storeErr.operation, storeErr.message, storeErr.cause],
      [legacyErr._tag, legacyErr.operation, legacyErr.message, legacyErr.cause],
    );
  });

  test('a sweep rejection becomes the identical operational failure as the legacy adapter', async () => {
    const rejection = new Error('sweep rejected');
    const storeWork = makeStoreRetentionWork({
      pruneEvents() {},
      retentionSweep() {
        return Promise.reject(rejection);
      },
    });
    const legacyWork = legacyRetentionWork({
      pruneEvents() {},
      retentionSweep() {
        return Promise.reject(rejection);
      },
    });

    const storeErr = soleFailure(
      await runEffectExit(
        storeWork.retentionSweep(7).pipe(Effect.provideService(Store, boundService())),
      ),
    );
    const legacyErr = soleFailure(await runEffectExit(legacyWork.retentionSweep(7)));

    assert.equal(storeErr._tag, 'BackgroundOperationalError');
    assert.equal(storeErr.operation, 'retention-retention-sweep');
    assert.equal(storeErr.message, 'fleetd retention retention-sweep failed');
    assert.equal(storeErr.cause, rejection);
    assert.deepEqual(
      [storeErr._tag, storeErr.operation, storeErr.message, storeErr.cause],
      [legacyErr._tag, legacyErr.operation, legacyErr.message, legacyErr.cause],
    );
  });

  test('a synchronous sweep-factory throw is a defect in both the store-backed and legacy work', async () => {
    const boom = new Error('sweep factory threw');
    const storeWork = makeStoreRetentionWork({
      pruneEvents() {},
      retentionSweep() {
        throw boom;
      },
    });
    const legacyWork = legacyRetentionWork({
      pruneEvents() {},
      retentionSweep() {
        throw boom;
      },
    });

    const storeExit = await runEffectExit(
      storeWork.retentionSweep(1).pipe(Effect.provideService(Store, boundService())),
    );
    const legacyExit = await runEffectExit(legacyWork.retentionSweep(1));

    for (const exit of [storeExit, legacyExit]) {
      assert.ok(Exit.isFailure(exit));
      assert.equal(Exit.hasFails(exit), false);
      assert.equal(Exit.hasDies(exit), true);
      assert.equal(Cause.squash(exit.cause), boom);
    }
  });

  test('the program.ts wiring shape runs boot immediately and keeps the 10-minute prune/sweep order (Store provided once)', async () => {
    const events: string[] = [];
    const work = makeStoreRetentionWork({
      pruneEvents(cutoffMs) {
        events.push(`prune:${String(cutoffMs)}`);
      },
      retentionSweep(nowMs) {
        events.push(`sweep:${String(nowMs)}`);
        return Promise.resolve();
      },
    });
    const service = boundService();

    const scenario = Effect.gen(function* () {
      // Mirror program.ts: spread the work into makeRetentionSchedule with the
      // exact boot-logs/periodic-silent onOperationalFailure, and discharge Store
      // once at the fiber boundary.
      const schedule = yield* makeRetentionSchedule({
        ...work,
        onOperationalFailure: ({ phase, error }) =>
          phase === 'boot'
            ? Effect.sync(() => {
                console.error('fleetd retention sweep error:', error.cause);
              })
            : Effect.void,
      });
      const fiber = yield* Effect.forkChild(schedule.program);
      yield* schedule.awaitFirstRun;
      assert.deepEqual(events, ['sweep:0']);

      yield* TestClock.adjust('10 minutes');
      assert.deepEqual(events, [
        'sweep:0',
        `prune:${String(10 * 60_000 - RETENTION_EVENT_WINDOW_MS)}`,
        `sweep:${String(10 * 60_000)}`,
      ]);
      yield* Fiber.interrupt(fiber);
    });

    const exit = await runEffectExit(
      Effect.provide(scenario, TestServicesLayer).pipe(Effect.provideService(Store, service)),
    );
    assert.ok(Exit.isSuccess(exit), Exit.isFailure(exit) ? Cause.pretty(exit.cause) : '');
  });

  test('store-backed boot sweep rejection settles readiness fail-open, identical to the legacy schedule path', async () => {
    const expected = new Error('legacy rejection');
    const failures: RetentionOperationalFailure[] = [];
    const cutoffs: number[] = [];
    let sweeps = 0;
    const work = makeStoreRetentionWork({
      pruneEvents(cutoffMs) {
        cutoffs.push(cutoffMs);
      },
      retentionSweep() {
        sweeps++;
        return sweeps === 1 ? Promise.reject(expected) : Promise.resolve();
      },
    });
    const service = boundService();

    const scenario = Effect.gen(function* () {
      const schedule = yield* makeRetentionSchedule({
        ...work,
        interval: '10 minutes',
        onOperationalFailure: (failure) =>
          Effect.sync(() => {
            failures.push(failure);
          }),
      });
      const fiber = yield* Effect.forkChild(schedule.program);
      yield* schedule.awaitFirstRun;
      // Fail-open boot: readiness settles, the boot failure is observed (not a
      // defect that kills the fiber), and the schedule continues on the grid.
      assert.equal(failures.length, 1);
      assert.equal(failures[0]?.phase, 'boot');
      assert.equal(failures[0]?.operation, 'retention-sweep');
      assert.equal(failures[0]?.error.cause, expected);

      yield* TestClock.adjust('10 minutes');
      assert.equal(sweeps, 2);
      assert.deepEqual(cutoffs, [10 * 60_000 - RETENTION_EVENT_WINDOW_MS]);
      yield* Fiber.interrupt(fiber);
    });

    const exit = await runEffectExit(
      Effect.provide(scenario, TestServicesLayer).pipe(Effect.provideService(Store, service)),
    );
    assert.ok(Exit.isSuccess(exit), Exit.isFailure(exit) ? Cause.pretty(exit.cause) : '');
  });
});
