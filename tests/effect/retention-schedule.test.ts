import assert from 'node:assert/strict';

import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Deferred from 'effect/Deferred';
import type * as Duration from 'effect/Duration';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Fiber from 'effect/Fiber';

import { ownedLegacyPromise } from '../../src/daemon/app/background-owner.ts';
import {
  legacyRetentionWork,
  makeRetentionSchedule,
  RETENTION_EVENT_WINDOW_MS,
  type RetentionOperationalFailure,
} from '../../src/daemon/app/retention-schedule.ts';
import { BackgroundOperationalError } from '../../src/daemon/app/services/background.ts';
import { runEffectExit, TestClock, TestServicesLayer } from './helpers.ts';

function operational(operation: string): BackgroundOperationalError {
  return new BackgroundOperationalError({
    operation,
    cause: operation,
    message: `${operation} failed`,
  });
}

function assertSuccess<A, E>(exit: Exit.Exit<A, E>): A {
  if (Exit.isFailure(exit)) assert.fail(Cause.pretty(exit.cause));
  return exit.value;
}

function latch(): { readonly promise: Promise<void>; readonly release: () => void } {
  let release: () => void = () => undefined;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}

describe('P5 Effect retention schedule', () => {
  test('runs boot immediately, captures options, and preserves exact 10-minute prune/sweep order', async () => {
    const events: string[] = [];
    const options = {
      interval: '10 minutes' as Duration.Input,
      eventWindowMs: RETENTION_EVENT_WINDOW_MS,
      pruneEvents: (cutoffMs: number) =>
        Effect.sync(() => {
          events.push(`prune:${String(cutoffMs)}`);
        }),
      retentionSweep: (nowMs: number) =>
        Effect.sync(() => {
          events.push(`sweep:${String(nowMs)}`);
        }),
    };
    const allocation = makeRetentionSchedule(options);
    options.interval = '1 minute';
    options.eventWindowMs = 1;
    options.pruneEvents = () => Effect.die('late option mutation');

    const scenario = Effect.gen(function* () {
      const schedule = yield* allocation;
      const fiber = yield* Effect.forkChild(schedule.program);
      yield* schedule.awaitFirstRun;
      assert.deepEqual(events, ['sweep:0']);

      yield* TestClock.adjust('1 minute');
      assert.deepEqual(events, ['sweep:0'], 'the mutated one-minute option was not observed');
      yield* TestClock.adjust('9 minutes');
      assert.deepEqual(events, [
        'sweep:0',
        `prune:${String(10 * 60_000 - RETENTION_EVENT_WINDOW_MS)}`,
        `sweep:${String(10 * 60_000)}`,
      ]);
      yield* TestClock.adjust('10 minutes');
      assert.deepEqual(events.slice(-2), [
        `prune:${String(20 * 60_000 - RETENTION_EVENT_WINDOW_MS)}`,
        `sweep:${String(20 * 60_000)}`,
      ]);
      yield* Fiber.interrupt(fiber);
    });

    assertSuccess(await runEffectExit(Effect.provide(scenario, TestServicesLayer)));
  });

  test('settles first-run readiness after a named boot failure and continues periodically', async () => {
    const expected = operational('boot-retention');
    const failures: RetentionOperationalFailure[] = [];
    let sweeps = 0;
    let prunes = 0;

    const scenario = Effect.gen(function* () {
      const schedule = yield* makeRetentionSchedule({
        interval: '10 minutes',
        pruneEvents: () =>
          Effect.sync(() => {
            prunes++;
          }),
        retentionSweep: () => {
          sweeps++;
          return sweeps === 1 ? Effect.fail(expected) : Effect.void;
        },
        onOperationalFailure: (failure) =>
          Effect.sync(() => {
            failures.push(failure);
          }),
      });
      const fiber = yield* Effect.forkChild(schedule.program);

      yield* schedule.awaitFirstRun;
      yield* schedule.awaitFirstRun;
      assert.equal(fiber.pollUnsafe(), undefined, 'named boot failure terminated the scheduler');
      assert.equal(sweeps, 1);
      assert.deepEqual(failures, [
        { phase: 'boot', operation: 'retention-sweep', error: expected },
      ]);

      yield* TestClock.adjust('10 minutes');
      assert.equal(prunes, 1);
      assert.equal(sweeps, 2);
      yield* Fiber.interrupt(fiber);
    });

    assertSuccess(await runEffectExit(Effect.provide(scenario, TestServicesLayer)));
  });

  test('observes each periodic operational failure, preserves order, and retries next grid tick', async () => {
    const pruneFailure = operational('event-prune');
    const sweepFailure = operational('periodic-retention');
    const failures: RetentionOperationalFailure[] = [];
    const order: string[] = [];
    let pruneAttempts = 0;
    let sweepAttempts = 0;

    const scenario = Effect.gen(function* () {
      const schedule = yield* makeRetentionSchedule({
        interval: '10 minutes',
        pruneEvents: () => {
          pruneAttempts++;
          order.push(`prune-${String(pruneAttempts)}`);
          return pruneAttempts === 1 ? Effect.fail(pruneFailure) : Effect.void;
        },
        retentionSweep: () => {
          sweepAttempts++;
          order.push(`sweep-${String(sweepAttempts)}`);
          return sweepAttempts === 2 ? Effect.fail(sweepFailure) : Effect.void;
        },
        onOperationalFailure: (failure) =>
          Effect.sync(() => {
            failures.push(failure);
          }),
      });
      const fiber = yield* Effect.forkChild(schedule.program);
      yield* schedule.awaitFirstRun;
      assert.deepEqual(order, ['sweep-1']);

      yield* TestClock.adjust('10 minutes');
      assert.deepEqual(order, ['sweep-1', 'prune-1', 'sweep-2']);
      assert.deepEqual(
        failures.map(({ phase, operation, error }) => ({ phase, operation, error })),
        [
          { phase: 'periodic', operation: 'prune-events', error: pruneFailure },
          { phase: 'periodic', operation: 'retention-sweep', error: sweepFailure },
        ],
      );

      yield* TestClock.adjust('10 minutes');
      assert.deepEqual(order.slice(-2), ['prune-2', 'sweep-3']);
      assert.equal(fiber.pollUnsafe(), undefined);
      yield* Fiber.interrupt(fiber);
    });

    assertSuccess(await runEffectExit(Effect.provide(scenario, TestServicesLayer)));
  });

  test('keeps work single-flight and skips fixed-grid ticks missed by an overrun', async () => {
    const starts: number[] = [];
    let inFlight = 0;
    let maximumInFlight = 0;

    const scenario = Effect.gen(function* () {
      const periodicStarted = yield* Deferred.make<void>();
      const releasePeriodic = yield* Deferred.make<void>();
      let sweeps = 0;
      const schedule = yield* makeRetentionSchedule({
        interval: '10 minutes',
        pruneEvents: () => Effect.void,
        retentionSweep: (nowMs) =>
          Effect.gen(function* () {
            sweeps++;
            inFlight++;
            maximumInFlight = Math.max(maximumInFlight, inFlight);
            starts.push(nowMs);
            if (sweeps === 2) {
              yield* Deferred.succeed(periodicStarted, undefined);
              yield* Deferred.await(releasePeriodic);
            }
            inFlight--;
          }),
      });
      const fiber = yield* Effect.forkChild(schedule.program);
      yield* schedule.awaitFirstRun;

      yield* TestClock.adjust('10 minutes');
      yield* Deferred.await(periodicStarted);
      yield* TestClock.adjust('25 minutes');
      assert.deepEqual(starts, [0, 10 * 60_000]);
      assert.equal(inFlight, 1);

      yield* Deferred.succeed(releasePeriodic, undefined);
      yield* Effect.yieldNow;
      yield* TestClock.adjust(4 * 60_000 + 59_000);
      assert.deepEqual(starts, [0, 10 * 60_000]);
      yield* TestClock.adjust('1 second');
      assert.deepEqual(starts, [0, 10 * 60_000, 40 * 60_000]);
      assert.equal(maximumInFlight, 1);
      assert.equal(inFlight, 0);
      yield* Fiber.interrupt(fiber);
    });

    assertSuccess(await runEffectExit(Effect.provide(scenario, TestServicesLayer)));
  });

  test('interruption joins active legacy work and suppresses its late Effect continuation', async () => {
    const sourceStarted = latch();
    const releaseSource = latch();
    let sweeps = 0;
    let prunes = 0;
    let lateMutations = 0;

    const scenario = Effect.gen(function* () {
      const schedule = yield* makeRetentionSchedule({
        interval: '10 minutes',
        pruneEvents: () =>
          Effect.sync(() => {
            prunes++;
          }),
        retentionSweep: () => {
          sweeps++;
          if (sweeps === 1) return Effect.void;
          return ownedLegacyPromise({
            try: () => {
              sourceStarted.release();
              return releaseSource.promise;
            },
            catch: () => operational('legacy-retention'),
          }).pipe(
            Effect.andThen(
              Effect.sync(() => {
                lateMutations++;
              }),
            ),
          );
        },
      });
      const fiber = yield* Effect.forkChild(schedule.program);
      yield* schedule.awaitFirstRun;
      yield* TestClock.adjust('10 minutes');
      yield* Effect.promise(() => sourceStarted.promise);

      fiber.interruptUnsafe();
      yield* Effect.yieldNow;
      assert.equal(fiber.pollUnsafe(), undefined, 'interruption detached active retention work');
      assert.equal(lateMutations, 0);

      releaseSource.release();
      const interrupted = yield* Fiber.await(fiber);
      assert.ok(Exit.isFailure(interrupted));
      assert.equal(Cause.hasInterruptsOnly(interrupted.cause), true);
      assert.equal(lateMutations, 0, 'cancelled work resumed an Effect continuation');

      yield* TestClock.adjust('1 hour');
      assert.equal(sweeps, 2);
      assert.equal(prunes, 1);
      assert.equal(lateMutations, 0);
    });

    assertSuccess(await runEffectExit(Effect.provide(scenario, TestServicesLayer)));
  });

  test('legacy adapter maps rejections to named failures without losing boot readiness', async () => {
    const expected = new Error('legacy rejection');
    const failures: RetentionOperationalFailure[] = [];
    const cutoffs: number[] = [];
    let sweeps = 0;
    const work = legacyRetentionWork({
      pruneEvents(cutoffMs) {
        cutoffs.push(cutoffMs);
      },
      retentionSweep() {
        sweeps++;
        return sweeps === 1 ? Promise.reject(expected) : Promise.resolve();
      },
    });

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
      assert.equal(failures.length, 1);
      assert.equal(failures[0]?.phase, 'boot');
      assert.equal(failures[0]?.error.cause, expected);

      yield* TestClock.adjust('10 minutes');
      assert.equal(sweeps, 2);
      assert.deepEqual(cutoffs, [10 * 60_000 - RETENTION_EVENT_WINDOW_MS]);
      yield* Fiber.interrupt(fiber);
    });

    assertSuccess(await runEffectExit(Effect.provide(scenario, TestServicesLayer)));
  });

  test('defects terminate both boot readiness and the scheduler fiber', async () => {
    const defect = new Error('unexpected retention defect');
    const scenario = Effect.gen(function* () {
      const schedule = yield* makeRetentionSchedule({
        pruneEvents: () => Effect.void,
        retentionSweep: () => Effect.die(defect),
      });
      const fiber = yield* Effect.forkChild(schedule.program);
      const readinessExit = yield* Effect.exit(schedule.awaitFirstRun);
      const fiberExit = yield* Fiber.await(fiber);
      return { readinessExit, fiberExit };
    });

    const { readinessExit, fiberExit } = assertSuccess(
      await runEffectExit(Effect.provide(scenario, TestServicesLayer)),
    );
    for (const exit of [readinessExit, fiberExit]) {
      assert.ok(Exit.isFailure(exit));
      assert.equal(Exit.hasFails(exit), false);
      assert.equal(Exit.hasDies(exit), true);
      assert.equal(Cause.squash(exit.cause), defect);
    }
  });
});
