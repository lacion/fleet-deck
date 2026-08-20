import assert from 'node:assert/strict';
import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Fiber from 'effect/Fiber';
import {
  BackgroundJoinTimeoutError,
  type BackgroundRegistration,
  makeBackgroundOwner,
  ownedLegacyPromise,
  prepareBackgroundOwner,
} from '../../src/daemon/app/background-owner.ts';
import {
  BackgroundOperationalError,
  BackgroundUnexpectedExitError,
} from '../../src/daemon/app/services/background.ts';
import { runEffectExit, TestClock } from './helpers.ts';

async function runEffectSuccess<A, E>(effect: Effect.Effect<A, E>): Promise<A> {
  const exit = await runEffectExit(effect);
  if (Exit.isFailure(exit)) assert.fail(Cause.pretty(exit.cause));
  return exit.value;
}

function operational(operation: string, cause: unknown = operation): BackgroundOperationalError {
  return new BackgroundOperationalError({
    operation,
    cause,
    message: `${operation} failed`,
  });
}

function firstReason<E>(exit: Exit.Exit<unknown, E>): Cause.Reason<E> {
  assert.ok(Exit.isFailure(exit));
  const reason = exit.cause.reasons[0];
  assert.ok(reason);
  return reason;
}

describe('P5 background service and owner', () => {
  test('preparation exposes readiness before a cold, concurrent-idempotent start', async () => {
    let runFactories = 0;
    let runEffects = 0;

    await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const prepared = yield* prepareBackgroundOwner({
            name: 'prepared',
            run: (controller) => {
              runFactories++;
              return Effect.sync(() => {
                runEffects++;
              }).pipe(
                Effect.andThen(controller.markReconciliationReady),
                Effect.andThen(Effect.never),
              );
            },
          });

          assert.equal(runFactories, 0, 'preparation constructed the background program');
          assert.equal(runEffects, 0, 'preparation started the background program');
          assert.equal(prepared.service.reconciliationStatus(), 'reconciling');

          const [first, second] = yield* Effect.all([prepared.start, prepared.start], {
            concurrency: 'unbounded',
          });
          assert.strictEqual(second, first);
          yield* prepared.service.awaitReady;
          assert.equal(runFactories, 1);
          assert.equal(runEffects, 1);
          assert.equal(prepared.service.reconciliationStatus(), 'settled');

          const third = yield* prepared.start;
          assert.strictEqual(third, first);
          assert.equal(runFactories, 1);
          assert.equal(runEffects, 1);
          assert.strictEqual(third.close(), first.close());
          yield* Effect.promise(() => first.close());
        }),
      ),
    );
  });

  test('the first start scope owns the fallback finalizer, not preparation', async () => {
    let interruptionFinalizers = 0;
    const owner = await runEffectSuccess(
      Effect.gen(function* () {
        const prepared = yield* prepareBackgroundOwner({
          name: 'start-scope',
          run: (controller) =>
            controller.markReconciliationReady.pipe(
              Effect.andThen(Effect.never),
              Effect.onInterrupt(() =>
                Effect.sync(() => {
                  interruptionFinalizers++;
                }),
              ),
            ),
        });

        assert.equal(prepared.service.reconciliationStatus(), 'reconciling');
        return yield* Effect.scoped(
          Effect.gen(function* () {
            const started = yield* prepared.start;
            yield* prepared.service.awaitReady;
            assert.equal(started.state, 'running');
            return started;
          }),
        );
      }),
    );

    assert.notEqual(owner.state, 'running');
    await owner.close();
    assert.equal(owner.state, 'closed');
    assert.equal(interruptionFinalizers, 1);
  });

  test('TestClock drives readiness while named degraded work still settles', async () => {
    await runEffectSuccess(
      Effect.provide(
        Effect.scoped(
          Effect.gen(function* () {
            const degraded = operational('boot-reconciliation');
            const registration = yield* makeBackgroundOwner({
              name: 'background',
              run: (controller) =>
                Effect.sleep(250).pipe(
                  Effect.andThen(Effect.fail(degraded)),
                  Effect.catchTag(
                    'BackgroundOperationalError',
                    () => controller.markReconciliationReady,
                  ),
                  Effect.andThen(Effect.never),
                ),
            });

            assert.equal(registration.service.reconciliationStatus(), 'reconciling');
            yield* TestClock.adjust(249);
            assert.equal(registration.service.reconciliationStatus(), 'reconciling');
            yield* TestClock.adjust(1);
            yield* registration.service.awaitReady;
            assert.equal(registration.service.reconciliationStatus(), 'settled');
            yield* Effect.promise(() => registration.owner.close());
            assert.equal(registration.owner.state, 'closed');
          }),
        ),
        TestClock.layer(),
      ),
    );
  });

  test('close publishes one Promise before interruption and is reentrant', async () => {
    let registration: BackgroundRegistration | null = null;
    let reentrantClose: Promise<void> | null = null;
    let interruptionFinalizers = 0;

    await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const created = yield* makeBackgroundOwner({
            name: 'reentrant',
            run: (controller) =>
              controller.markReconciliationReady.pipe(
                Effect.andThen(Effect.never),
                Effect.onInterrupt(() =>
                  Effect.sync(() => {
                    interruptionFinalizers += 1;
                    reentrantClose = registration?.owner.close() ?? null;
                  }),
                ),
              ),
          });
          registration = created;
          yield* created.service.awaitReady;

          const first = created.owner.close();
          const second = created.owner.close();
          assert.strictEqual(second, first);
          yield* Effect.promise(() => first);
          assert.strictEqual(reentrantClose, first);
          assert.equal(interruptionFinalizers, 1);
          assert.equal(created.owner.state, 'closed');
          assert.strictEqual(created.owner.close(), first);
        }),
      ),
    );
  });

  test('awaitFailure preserves named operational failure and makes readiness fail the same way', async () => {
    const expected = operational('retention');
    await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const registration = yield* makeBackgroundOwner({
            name: 'operational',
            run: () => Effect.fail(expected),
          });
          const [failureExit, readyExit] = yield* Effect.all([
            Effect.exit(registration.service.awaitFailure),
            Effect.exit(registration.service.awaitReady),
          ]);

          for (const exit of [failureExit, readyExit]) {
            const reason = firstReason(exit);
            assert.ok(Cause.isFailReason(reason));
            assert.strictEqual(reason.error, expected);
          }
        }),
      ),
    );
  });

  test('awaitFailure preserves defects and treats daemon-long success as a defect', async () => {
    const defect = new Error('background defect');
    const defectExit = await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const registration = yield* makeBackgroundOwner({
            name: 'defective',
            run: () => Effect.die(defect),
          });
          return yield* Effect.exit(registration.service.awaitFailure);
        }),
      ),
    );
    const defectReason = firstReason(defectExit);
    assert.ok(Cause.isDieReason(defectReason));
    assert.strictEqual(defectReason.defect, defect);

    const successExit = await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const registration = yield* makeBackgroundOwner({
            name: 'returned',
            run: () => Effect.succeed('not daemon-long'),
          });
          return yield* Effect.exit(registration.service.awaitFailure);
        }),
      ),
    );
    const successReason = firstReason(successExit);
    assert.ok(Cause.isDieReason(successReason));
    assert.ok(successReason.defect instanceof BackgroundUnexpectedExitError);
    assert.equal(successReason.defect.owner, 'returned');
  });

  test('owned legacy Promise interruption joins the same Promise before the fiber exits', async () => {
    let resolveSource: () => void = () => undefined;
    let resolveStarted: () => void = () => undefined;
    let mutated = false;
    const started = new Promise<void>((resolve) => {
      resolveStarted = resolve;
    });
    const source = new Promise<void>((resolve) => {
      resolveSource = () => {
        mutated = true;
        resolve();
      };
    });

    await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const fiber = yield* Effect.forkChild(
            ownedLegacyPromise({
              try: () => {
                resolveStarted();
                return source;
              },
              catch: (cause) => operational('legacy-promise', cause),
            }),
          );
          yield* Effect.promise(() => started);
          fiber.interruptUnsafe();
          yield* Effect.yieldNow;
          assert.equal(fiber.pollUnsafe(), undefined, 'interruption detached the owned Promise');
          assert.equal(mutated, false);

          resolveSource();
          const exit = yield* Fiber.await(fiber);
          assert.ok(Exit.isFailure(exit));
          assert.ok(Cause.hasInterruptsOnly(exit.cause));
          assert.equal(mutated, true);
        }),
      ),
    );
  });

  test('owned legacy Promise maps rejection but keeps a synchronous factory throw as a defect', async () => {
    const expected = operational('legacy-rejection');
    const rejectionExit = await runEffectSuccess(
      Effect.exit(
        ownedLegacyPromise({
          try: () => Promise.reject('expected rejection'),
          catch: () => expected,
        }),
      ),
    );
    const rejectionReason = firstReason(rejectionExit);
    assert.ok(Cause.isFailReason(rejectionReason));
    assert.strictEqual(rejectionReason.error, expected);

    const defect = new Error('factory defect');
    const defectExit = await runEffectSuccess(
      Effect.exit(
        ownedLegacyPromise({
          try: () => {
            throw defect;
          },
          catch: (cause) => operational('unreachable-mapper', cause),
        }),
      ),
    );
    const defectReason = firstReason(defectExit);
    assert.ok(Cause.isDieReason(defectReason));
    assert.strictEqual(defectReason.defect, defect);
  });

  test('bounded close and Scope fallback do not start another join', async () => {
    let resolveSource: () => void = () => undefined;
    let resolveStarted: () => void = () => undefined;
    let sourceSettled = false;
    const started = new Promise<void>((resolve) => {
      resolveStarted = resolve;
    });
    const source = new Promise<void>((resolve) => {
      resolveSource = () => {
        sourceSettled = true;
        resolve();
      };
    });
    let armedMilliseconds: number | null = null;
    let fireTimeout: () => void = () => undefined;

    const timeout = await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const registration = yield* makeBackgroundOwner({
            name: 'bounded',
            joinTimeoutMs: 250,
            armJoinTimeout: (callback, milliseconds) => {
              armedMilliseconds = milliseconds;
              let active = true;
              fireTimeout = () => {
                if (active) callback();
              };
              return () => {
                active = false;
              };
            },
            run: () =>
              ownedLegacyPromise({
                try: () => {
                  resolveStarted();
                  return source;
                },
                catch: (cause) => operational('bounded-source', cause),
              }).pipe(Effect.andThen(Effect.never)),
          });
          yield* Effect.promise(() => started);

          const first = registration.owner.close();
          assert.strictEqual(registration.owner.close(), first);
          assert.equal(armedMilliseconds, 250);
          const outcome = first.then(
            () => null,
            (error: unknown) => error,
          );
          fireTimeout();
          const error = yield* Effect.promise(() => outcome);
          assert.equal(sourceSettled, false);
          return error;
        }),
      ),
    );

    assert.ok(timeout instanceof BackgroundJoinTimeoutError);
    assert.equal(timeout.owner, 'bounded');
    assert.equal(timeout.timeoutMs, 250);
    assert.equal(sourceSettled, false, 'scope fallback waited for the legacy Promise');
    resolveSource();
    await Promise.resolve();
    assert.equal(sourceSettled, true);
  });
});
