import assert from 'node:assert/strict';
import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import {
  BackgroundJoinTimeoutError,
  ownedLegacyPromise,
  prepareBackgroundOwner,
} from '../../src/daemon/app/background-owner.ts';
import { makeDaemonBackgroundProgram } from '../../src/daemon/app/background-program.ts';
import { BackgroundOperationalError } from '../../src/daemon/app/services/background.ts';
import { scaleMs } from '../helpers/wait.ts';
import { runEffectExit } from './helpers.ts';

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

describe('P5 prompt top-level background failure', () => {
  test('a defecting child fails the root before a stuck sibling can join, within a bounded close', async () => {
    const defect = new Error('agents-poll defect');
    let resolveSource: () => void = () => undefined;
    let markSiblingStarted: () => void = () => undefined;
    let sourceSettled = false;
    const siblingStarted = new Promise<void>((resolve) => {
      markSiblingStarted = resolve;
    });
    const source = new Promise<void>((resolve) => {
      resolveSource = () => {
        sourceSettled = true;
        resolve();
      };
    });
    let armedMilliseconds: number | null = null;
    let fireTimeout: () => void = () => undefined;

    // A sibling that has entered an owned legacy Promise: interrupting it joins
    // the same never-settling completion, so the aggregate can never exit.
    const stuckSibling = ownedLegacyPromise({
      try: () => {
        markSiblingStarted();
        return source;
      },
      catch: (cause) => operational('stuck-sibling', cause),
    }).pipe(
      Effect.andThen(Effect.never),
      Effect.catch(() => Effect.never),
    ) as Effect.Effect<never, never>;

    // The defecting child dies only once the sibling is confirmed running, so the
    // failure is observed while that sibling is mid-flight and destined to stick.
    const defectingChild = Effect.promise(() => siblingStarted).pipe(
      Effect.andThen(Effect.die(defect)),
    ) as Effect.Effect<never, never>;

    const result = await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const prepared = yield* prepareBackgroundOwner({
            name: 'prompt-failure',
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
            run: (controller) =>
              makeDaemonBackgroundProgram(controller, {
                agentsPoll: defectingChild,
                lanRefresh: stuckSibling,
                retention: { awaitFirstRun: Effect.never, program: Effect.never },
                boot: {
                  clearForkHealing: Effect.never,
                  reconcileSpawns: Effect.never,
                  awaitBroadcastIdle: Effect.never,
                },
              }),
          });
          const owner = yield* prepared.start;

          // (a) The root observes the defect promptly: awaitFailure resolves with
          // no bounded join even armed, and while the stuck sibling has not joined.
          const failureExit = yield* Effect.exit(
            prepared.service.awaitFailure.pipe(Effect.timeout(scaleMs(5_000))),
          );
          assert.equal(armedMilliseconds, null, 'awaitFailure resolved before any close/join');
          assert.equal(sourceSettled, false, 'the stuck sibling never joined');

          // (b) Shutdown is still bounded: close interrupts and, because the stuck
          // sibling cannot join, the configured join timeout resolves close().
          const close = owner.close();
          assert.equal(armedMilliseconds, 250, 'close armed exactly one bounded join');
          const outcome = close.then(
            () => null,
            (error: unknown) => error,
          );
          fireTimeout();
          const closeError = yield* Effect.promise(() => outcome);
          return { failureExit, closeError };
        }),
      ),
    );

    const reason = firstReason(result.failureExit);
    assert.ok(Cause.isDieReason(reason), 'the defecting child surfaced as a defect');
    assert.strictEqual(reason.defect, defect);
    assert.ok(result.closeError instanceof BackgroundJoinTimeoutError);
    assert.equal((result.closeError as BackgroundJoinTimeoutError).owner, 'prompt-failure');
    assert.equal((result.closeError as BackgroundJoinTimeoutError).timeoutMs, 250);

    // Release the never-settling sibling so no owned Promise is left pending.
    resolveSource();
    await Promise.resolve();
    assert.equal(sourceSettled, true);
  });

  test('a requested shutdown interruption is suppressed and does not trip the failure latch', async () => {
    const failureExit = await runEffectSuccess(
      Effect.scoped(
        Effect.gen(function* () {
          const prepared = yield* prepareBackgroundOwner({
            name: 'requested-shutdown',
            joinTimeoutMs: 1_000,
            run: (controller) =>
              makeDaemonBackgroundProgram(controller, {
                agentsPoll: Effect.never,
                lanRefresh: Effect.never,
                retention: { awaitFirstRun: Effect.void, program: Effect.never },
                boot: {
                  clearForkHealing: Effect.void,
                  reconcileSpawns: Effect.void,
                  awaitBroadcastIdle: Effect.void,
                },
              }),
          });
          const owner = yield* prepared.start;

          // A healthy daemon reaches readiness and stays running: no child exit
          // has published a failure yet.
          yield* prepared.service.awaitReady;

          // Requesting shutdown interrupts every running child. Each such
          // interruption must be suppressed, not published as a failure: the
          // latch may only ever carry the aggregate's own interrupts-only Cause.
          yield* Effect.promise(() => owner.close());
          assert.equal(owner.state, 'closed');
          return yield* Effect.exit(prepared.service.awaitFailure);
        }),
      ),
    );

    assert.ok(Exit.isFailure(failureExit));
    assert.ok(
      Cause.hasInterruptsOnly(failureExit.cause),
      'requested-shutdown interruptions stayed interruptions and did not become a published failure',
    );
  });
});
