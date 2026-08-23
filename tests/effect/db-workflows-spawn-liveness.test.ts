import assert from 'node:assert/strict';

import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Duration from 'effect/Duration';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';

import {
  type LivenessCallbacks,
  type LivenessWork,
  legacyLivenessWork,
  makeAgentsPollOwner,
  makeAgentsPollProgram,
} from '../../src/daemon/app/agents-poll.ts';
import { makeStoreLivenessWork } from '../../src/daemon/app/db-workflows/spawn-liveness.ts';
import { Store } from '../../src/daemon/app/services/store.ts';
import { makeStoreOwner } from '../../src/daemon/app/store-owner.ts';
import type { SqliteHandle } from '../../src/daemon/sqlite.ts';
import { type FakeProcessRunner, makeFakeProcessRunner } from './fake-layers.ts';
import { runEffectExit, TestClock, TestServicesLayer } from './helpers.ts';

/**
 * A fake SQLite handle that throws on every query path. Like the retention pilot
 * and the earlier P8.6 slices, this slice only YIELDS Store to declare the
 * dependency; it must not touch the handle yet, so any accidental handle access
 * surfaces immediately as a test defect.
 */
function fakeHandle(): SqliteHandle {
  const forbid = (): never => {
    throw new Error('P8.6 spawn-liveness workflow must not touch the SQLite handle yet');
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

function testLayer(processRunner: FakeProcessRunner) {
  return Layer.merge(TestServicesLayer, processRunner.layer);
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

/**
 * Drive one wired liveness work through the REAL scheduler with the agents CLI
 * disabled, so the liveness tick is the only per-tick work. Every tick throws.
 * Returns the observable loop outcome: how many ticks ran and whether the fiber
 * closed on a clean interrupt (not a defect). Legacy work (Environment `never`) is
 * assignable to the `Store` parameter (R is covariant), so the same harness drives
 * both adapters and their fail-open outcomes compare byte-for-byte. Store is
 * discharged once at the fiber boundary, exactly as program.ts does.
 */
async function runThrowingLivenessLoop(
  makeWork: (callbacks: LivenessCallbacks) => LivenessWork<Store>,
): Promise<{ ticks: number; closedCleanly: boolean }> {
  let ticks = 0;
  const liveness = makeWork({
    spawnLivenessTick() {
      ticks++;
      throw new Error('named liveness skip');
    },
  });
  const processRunner = makeFakeProcessRunner();
  let closedCleanly = false;

  const scenario = Effect.gen(function* () {
    const fiber = yield* Effect.forkChild(
      // Mirror program.ts: makeAgentsPollProgram(core, options, wiredIngest, wiredLiveness).
      // The CLI is disabled (argv null), so no poll runs and the injected liveness
      // work owns the only per-tick effect; the poll core's own callbacks must
      // never fire.
      makeAgentsPollProgram(
        {
          ingestAgentsPoll() {
            assert.fail('a disabled agents CLI must not ingest');
          },
        },
        { argv: null, firstRunDelayMs: 0, idlePollIntervalMs: 10, pollIntervalMs: 10 },
        undefined,
        liveness,
      ),
    );
    const owner = makeAgentsPollOwner(fiber);
    yield* Effect.yieldNow;
    for (let index = 1; index < 4; index += 1) {
      yield* TestClock.adjust(Duration.millis(10));
    }
    yield* Effect.promise(() => owner.close());
    const fiberExit = yield* Effect.promise(() => owner.exit);
    closedCleanly =
      Exit.isFailure(fiberExit) && Exit.hasInterrupts(fiberExit) && !Exit.hasDies(fiberExit);
  });

  const exit = await runEffectExit(
    Effect.provide(scenario, testLayer(processRunner)).pipe(
      Effect.provideService(Store, boundService()),
    ),
  );
  assertSuccess(exit);
  assert.equal(processRunner.requests.length, 0, 'the disabled CLI must never spawn a poll');
  return { ticks, closedCleanly };
}

describe('P8.6 store-backed spawn-liveness work', () => {
  test('yields Store and threads success through the existing tick', async () => {
    let ticks = 0;
    const work = makeStoreLivenessWork({
      spawnLivenessTick() {
        ticks++;
      },
    });

    // The requirement is Store: the tick is only runnable once Store is
    // discharged — exactly the local provide program.ts performs on the fiber.
    const service = boundService();
    const exit = await runEffectExit(work.pipe(Effect.provideService(Store, service)));
    assert.ok(Exit.isSuccess(exit), Exit.isFailure(exit) ? Cause.pretty(exit.cause) : '');
    assert.equal(ticks, 1);

    // Negative pin for the family convention itself: without the discharge the
    // tick must DIE on the missing Store service. R is covariant, so deleting the
    // `yield* Store` line would still typecheck and every provide-wrapped
    // assertion above would still pass — this is the one case that fails if the
    // yield is removed.
    const undischargedExit = await runEffectExit(work as Effect.Effect<void, never, never>);
    assert.ok(Exit.isFailure(undischargedExit) && Cause.hasDies(undischargedExit.cause));
    assert.match(Cause.pretty(undischargedExit.cause), /Service not found.*Store/);
    assert.equal(ticks, 1, 'the undischarged tick must never reach the callback');
  });

  test('a synchronous tick throw becomes the identical AgentsPollLivenessError as the legacy adapter', async () => {
    const boom = new Error('tick blew up');
    const storeWork = makeStoreLivenessWork({
      spawnLivenessTick() {
        throw boom;
      },
    });
    const legacyWork = legacyLivenessWork({
      spawnLivenessTick() {
        throw boom;
      },
    });

    const storeErr = soleFailure(
      await runEffectExit(storeWork.pipe(Effect.provideService(Store, boundService()))),
    );
    const legacyErr = soleFailure(await runEffectExit(legacyWork));

    assert.equal(storeErr._tag, 'AgentsPollLivenessError');
    assert.equal(storeErr.cause, boom);
    // Byte-parity with the legacy capability-parameterized adapter: same tag and
    // preserved cause, so the scheduler's fail-open runLiveness boundary catches
    // that tag identically either way.
    assert.deepEqual([storeErr._tag, storeErr.cause], [legacyErr._tag, legacyErr.cause]);
  });

  test('an async tick rejection becomes the identical AgentsPollLivenessError as the legacy adapter', async () => {
    const boom = new Error('async tick rejected');
    const storeWork = makeStoreLivenessWork({
      spawnLivenessTick() {
        return Promise.reject(boom);
      },
    });
    const legacyWork = legacyLivenessWork({
      spawnLivenessTick() {
        return Promise.reject(boom);
      },
    });

    const storeErr = soleFailure(
      await runEffectExit(storeWork.pipe(Effect.provideService(Store, boundService()))),
    );
    const legacyErr = soleFailure(await runEffectExit(legacyWork));

    // The liveness callback is genuinely async: an already-owned Promise that
    // rejects settles through the shared ownedLivenessTick bridge into the same
    // named failure the sync path produces, byte-identical across both adapters.
    assert.equal(storeErr._tag, 'AgentsPollLivenessError');
    assert.equal(storeErr.cause, boom);
    assert.deepEqual([storeErr._tag, storeErr.cause], [legacyErr._tag, legacyErr.cause]);
  });

  test('a throwing store-backed tick is swallowed by the scheduler fail-open skip; the loop keeps ticking', async () => {
    const { ticks, closedCleanly } = await runThrowingLivenessLoop(makeStoreLivenessWork);

    // firstRunDelayMs 0 fires the tick at t0, then three 10 ms grid ticks fire
    // three more: the tick throws on every one, the named fail-open skip swallows
    // each, and the loop is still alive to be interrupted cleanly.
    assert.equal(ticks, 4);
    assert.equal(closedCleanly, true, 'the fail-open loop closes on interrupt, not a defect');
  });

  test('a missing-Store defect is not laundered by the fail-open skip and terminates the scheduler', async () => {
    const processRunner = makeFakeProcessRunner();
    const storeWork = makeStoreLivenessWork({
      spawnLivenessTick() {
        assert.fail('a missing-Store tick must die before it reaches the callback');
      },
    });

    const scenario = Effect.gen(function* () {
      const fiber = yield* Effect.forkChild(
        // Cast drops the Store requirement so the program is runnable under a
        // ProcessRunner-only layer: the missing service surfaces at runtime as a
        // defect inside the tick, exactly the case the fail-open catchTag must NOT
        // launder. The CLI is disabled, so the liveness tick is the first effect.
        makeAgentsPollProgram(
          {
            ingestAgentsPoll() {
              assert.fail('unused poll-core ingest');
            },
          },
          { argv: null, firstRunDelayMs: 0, pollIntervalMs: 10 },
          undefined,
          storeWork,
        ) as Effect.Effect<never, never, never>,
      );
      const owner = makeAgentsPollOwner(fiber);
      const fiberExit = yield* Effect.promise(() => owner.exit);
      const closing = owner.close();
      yield* Effect.promise(() => closing);
      return fiberExit;
    });

    const exit = await runEffectExit(Effect.provide(scenario, testLayer(processRunner)));
    const fiberExit = assertSuccess(exit);
    assert.ok(Exit.isFailure(fiberExit));
    assert.equal(Exit.hasFails(fiberExit), false);
    assert.equal(Exit.hasDies(fiberExit), true);
    assert.match(Cause.pretty(fiberExit.cause), /Service not found.*Store/);
  });

  test('the store-backed fail-open loop is byte-identical to the legacy adapter', async () => {
    const storeOutcome = await runThrowingLivenessLoop(makeStoreLivenessWork);
    const legacyOutcome = await runThrowingLivenessLoop(legacyLivenessWork);

    // The yield* Store prefix changes nothing about the fail-open loop: same tick
    // count, same clean interrupt on close.
    assert.deepEqual(storeOutcome, legacyOutcome);
  });

  test('owner close joins a store-backed in-flight tick and suppresses later ticks', async () => {
    const tickStarted = latch();
    const releaseTick = latch();
    let ticks = 0;
    let tickCompleted = false;
    const work = makeStoreLivenessWork({
      spawnLivenessTick() {
        ticks++;
        tickStarted.release();
        return releaseTick.promise.then(() => {
          tickCompleted = true;
        });
      },
    });
    const processRunner = makeFakeProcessRunner();

    let closeSettled = false;
    const scenario = Effect.gen(function* () {
      const fiber = yield* Effect.forkChild(
        makeAgentsPollProgram(
          {
            ingestAgentsPoll() {
              assert.fail('a disabled agents CLI must not ingest');
            },
          },
          { argv: null, firstRunDelayMs: 0, idlePollIntervalMs: 10, pollIntervalMs: 10 },
          undefined,
          work,
        ),
      );
      const owner = makeAgentsPollOwner(fiber);
      // The first tick returns a still-pending Promise, so the scheduler is parked
      // inside the shared ownedLivenessTick bridge with the callback in flight.
      yield* Effect.promise(() => tickStarted.promise);

      const closing = owner.close();
      void closing.then(() => {
        closeSettled = true;
      });
      yield* TestClock.adjust(Duration.seconds(1));
      assert.equal(ticks, 1, 'a blocked tick cannot overlap with another');
      assert.equal(closeSettled, false, 'close joins the in-flight tick before settling');
      assert.equal(tickCompleted, false);

      releaseTick.release();
      yield* Effect.promise(() => closing);
      assert.equal(tickCompleted, true, 'the interruption finalizer joined the admitted callback');
      const fiberExit = yield* Effect.promise(() => owner.exit);
      assert.ok(
        Exit.isFailure(fiberExit) && Exit.hasInterrupts(fiberExit) && !Exit.hasDies(fiberExit),
        'the joined close is a clean interrupt, not a defect',
      );

      yield* TestClock.adjust(Duration.seconds(1));
      assert.equal(ticks, 1, 'no later tick runs after the interrupt');
    });

    const exit = await runEffectExit(
      Effect.provide(scenario, testLayer(processRunner)).pipe(
        Effect.provideService(Store, boundService()),
      ),
    );
    assertSuccess(exit);
    assert.equal(processRunner.requests.length, 0, 'the disabled CLI must never spawn a poll');
  });
});
