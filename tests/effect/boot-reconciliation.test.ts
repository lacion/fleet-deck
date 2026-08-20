import assert from 'node:assert/strict';

import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Deferred from 'effect/Deferred';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';

import { makeBackgroundOwner } from '../../src/daemon/app/background-owner.ts';
import {
  type BootReconciliationOperation,
  type BootReconciliationOptions,
  type BootReconciliationResult,
  legacyBootReconciliationWork,
  makeBootReconciliationProgram,
} from '../../src/daemon/app/boot-reconciliation.ts';
import { runEffectExit } from './helpers.ts';

async function runEffectSuccess<A, E>(effect: Effect.Effect<A, E>): Promise<A> {
  const exit = await runEffectExit(effect);
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

describe('P5 boot reconciliation workflow', () => {
  test('orders clear-fork healing, concurrent middle legs, broadcast flush, then readiness', async () => {
    const events: string[] = [];
    let inFlight = 0;
    let maximumInFlight = 0;

    await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const spawnStarted = yield* Deferred.make<void>();
          const retentionStarted = yield* Deferred.make<void>();

          const middle = (
            name: string,
            mine: Deferred.Deferred<void>,
            other: Deferred.Deferred<void>,
          ) =>
            Effect.gen(function* () {
              events.push(`${name}:start`);
              inFlight++;
              maximumInFlight = Math.max(maximumInFlight, inFlight);
              yield* Deferred.succeed(mine, undefined);
              yield* Deferred.await(other);
              events.push(`${name}:end`);
              inFlight--;
            });

          const registration = yield* makeBackgroundOwner({
            name: 'boot-order',
            run: (controller) =>
              makeBootReconciliationProgram(controller, {
                clearForkHealing: Effect.sync(() => {
                  assert.equal(inFlight, 0);
                  events.push('clear:start', 'clear:end');
                }),
                reconcileSpawns: middle('spawn', spawnStarted, retentionStarted),
                firstRetention: middle('retention', retentionStarted, spawnStarted),
                awaitBroadcastIdle: Effect.sync(() => {
                  assert.equal(inFlight, 0);
                  events.push('broadcast');
                }),
              }).pipe(Effect.andThen(Effect.never)),
          });

          assert.equal(registration.service.reconciliationStatus(), 'reconciling');
          yield* registration.service.awaitReady;
          events.push(`ready:${registration.service.reconciliationStatus()}`);

          assert.deepEqual(events.slice(0, 2), ['clear:start', 'clear:end']);
          const spawnStart = events.indexOf('spawn:start');
          const retentionStart = events.indexOf('retention:start');
          const spawnEnd = events.indexOf('spawn:end');
          const retentionEnd = events.indexOf('retention:end');
          const broadcast = events.indexOf('broadcast');
          assert.ok(spawnStart > 1 && retentionStart > 1);
          assert.ok(spawnStart < retentionEnd && retentionStart < spawnEnd);
          assert.ok(spawnEnd < broadcast && retentionEnd < broadcast);
          assert.deepEqual(events.slice(-2), ['broadcast', 'ready:settled']);
          assert.equal(maximumInFlight, 2, 'spawn and retention did not start concurrently');

          yield* Effect.promise(() => registration.owner.close());
        }),
      ),
    );
  });

  test('records every legacy operational failure and still completes degraded readiness', async () => {
    const clearFailure = new Error('clear failure');
    const spawnFailure = new Error('spawn failure');
    const retentionFailure = new Error('retention failure');
    const broadcastFailure = new Error('broadcast failure');
    const observed: BootReconciliationOperation[] = [];
    let result: BootReconciliationResult | null = null;
    let broadcastAttempts = 0;

    await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const resultPublished = yield* Deferred.make<void>();
          const work = legacyBootReconciliationWork({
            clearForkHealing() {
              throw clearFailure;
            },
            reconcileSpawns: () => Promise.reject(spawnFailure),
            firstRetention: () => Promise.reject(retentionFailure),
            awaitBroadcastIdle() {
              broadcastAttempts++;
              return Promise.reject(broadcastFailure);
            },
          });
          const registration = yield* makeBackgroundOwner({
            name: 'boot-degraded',
            run: (controller) =>
              makeBootReconciliationProgram(controller, {
                ...work,
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
          assert.equal(registration.service.reconciliationStatus(), 'settled');
          yield* Deferred.await(resultPublished);
          assert.equal(broadcastAttempts, 1);
          assert.deepEqual(observed, [
            'clear-fork-healing',
            'spawn-reconciliation',
            'first-retention',
            'broadcast-idle',
          ]);
          assert.equal(result?.degraded, true);
          assert.deepEqual(
            result?.failures.map(({ operation, error }) => ({ operation, cause: error.cause })),
            [
              { operation: 'clear-fork-healing', cause: clearFailure },
              { operation: 'spawn-reconciliation', cause: spawnFailure },
              { operation: 'first-retention', cause: retentionFailure },
              { operation: 'broadcast-idle', cause: broadcastFailure },
            ],
          );

          yield* Effect.promise(() => registration.owner.close());
        }),
      ),
    );
  });

  for (const operation of [
    'clear-fork-healing',
    'spawn-reconciliation',
    'first-retention',
    'broadcast-idle',
  ] as const) {
    test(`preserves a ${operation} defect for awaitFailure`, async () => {
      const defect = new Error(`${operation} defect`);
      const defective = Effect.die(defect);
      const options: BootReconciliationOptions<never> = {
        clearForkHealing: operation === 'clear-fork-healing' ? defective : Effect.void,
        reconcileSpawns: operation === 'spawn-reconciliation' ? defective : Effect.void,
        firstRetention: operation === 'first-retention' ? defective : Effect.void,
        awaitBroadcastIdle: operation === 'broadcast-idle' ? defective : Effect.void,
      };

      await runEffectSuccess(
        Effect.scoped(
          Effect.gen(function* () {
            const registration = yield* makeBackgroundOwner({
              name: `boot-defect-${operation}`,
              run: (controller) => makeBootReconciliationProgram(controller, options),
            });
            const [failureExit, readyExit] = yield* Effect.all([
              Effect.exit(registration.service.awaitFailure),
              Effect.exit(registration.service.awaitReady),
            ]);

            for (const exit of [failureExit, readyExit]) {
              assert.ok(Exit.isFailure(exit));
              assert.equal(Exit.hasFails(exit), false);
              assert.equal(Exit.hasDies(exit), true);
              assert.strictEqual(Cause.squash(exit.cause), defect);
            }
            assert.equal(registration.service.reconciliationStatus(), 'reconciling');
          }),
        ),
      );
    });
  }

  test('interrupts and joins both admitted legacy promises without a late continuation', async () => {
    const spawnStarted = latch();
    const retentionStarted = latch();
    const releaseSpawn = latch();
    const releaseRetention = latch();
    let active = 0;
    let maximumActive = 0;
    let admittedMutations = 0;
    let postCloseMutations = 0;
    let broadcastAttempts = 0;
    let storeClosed = false;

    const admitted = (started: ReturnType<typeof latch>, release: ReturnType<typeof latch>) => {
      active++;
      maximumActive = Math.max(maximumActive, active);
      started.release();
      return release.promise.then(() => {
        if (storeClosed) postCloseMutations++;
        admittedMutations++;
        active--;
      });
    };

    await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const work = legacyBootReconciliationWork({
            clearForkHealing: () => undefined,
            reconcileSpawns: () => admitted(spawnStarted, releaseSpawn),
            firstRetention: () => admitted(retentionStarted, releaseRetention),
            awaitBroadcastIdle() {
              broadcastAttempts++;
              return Promise.resolve();
            },
          });
          const registration = yield* makeBackgroundOwner({
            name: 'boot-interruption',
            run: (controller) =>
              makeBootReconciliationProgram(controller, work).pipe(Effect.andThen(Effect.never)),
          });

          yield* Effect.promise(() =>
            Promise.all([spawnStarted.promise, retentionStarted.promise]),
          );
          assert.equal(maximumActive, 2);
          registration.owner.interrupt();
          assert.equal(registration.owner.state, 'interrupting');
          const closing = registration.owner.close();
          let closed = false;
          void closing.then(() => {
            closed = true;
          });
          yield* Effect.yieldNow;
          assert.equal(closed, false, 'owner detached both admitted Promises');

          releaseSpawn.release();
          yield* Effect.promise(() => Promise.resolve());
          assert.equal(closed, false, 'owner detached the still-active retention Promise');
          releaseRetention.release();
          yield* Effect.promise(() => closing);
          storeClosed = true;
          yield* Effect.promise(() => Promise.resolve());

          assert.equal(admittedMutations, 2, 'close returned before admitted work settled');
          assert.equal(postCloseMutations, 0);
          assert.equal(active, 0);
          assert.equal(broadcastAttempts, 0, 'interrupted workflow reached its trailing flush');
          assert.equal(registration.service.reconciliationStatus(), 'reconciling');
          const readyExit = yield* Effect.exit(registration.service.awaitReady);
          assert.ok(Exit.isFailure(readyExit));
          assert.equal(Cause.hasInterruptsOnly(readyExit.cause), true);
        }),
      ),
    );
  });

  test('a synchronous legacy Promise factory throw remains a defect', async () => {
    const defect = new Error('legacy spawn factory defect');
    const work = legacyBootReconciliationWork({
      clearForkHealing: () => undefined,
      reconcileSpawns() {
        throw defect;
      },
      firstRetention: () => Promise.resolve(),
      awaitBroadcastIdle: () => Promise.resolve(),
    });

    await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const registration = yield* makeBackgroundOwner({
            name: 'boot-legacy-defect',
            run: (controller) => makeBootReconciliationProgram(controller, work),
          });
          const failureExit = yield* Effect.exit(registration.service.awaitFailure);
          assert.ok(Exit.isFailure(failureExit));
          assert.equal(Exit.hasFails(failureExit), false);
          assert.strictEqual(Cause.squash(failureExit.cause), defect);
        }),
      ),
    );
  });
});
