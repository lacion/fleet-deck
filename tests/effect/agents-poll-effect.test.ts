import assert from 'node:assert/strict';

import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Clock from 'effect/Clock';
import * as Duration from 'effect/Duration';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';

import {
  AGENTS_POLL_EXEC_TIMEOUT_MS,
  makeAgentsPollOwner,
  makeAgentsPollProgram,
  resolveAgentsPollOptions,
} from '../../src/daemon/app/agents-poll.ts';
import { ProcessNonZeroExitError, ProcessTimeoutError } from '../../src/daemon/app/errors.ts';
import type { ProcessSuccess } from '../../src/daemon/app/services/process-runner.ts';
import { makeFakeProcessRunner, type FakeProcessRunner } from './fake-layers.ts';
import { runEffectExit, TestClock, TestServicesLayer } from './helpers.ts';

function latch(): { readonly promise: Promise<void>; readonly release: () => void } {
  let release: () => void = () => undefined;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}

function testLayer(processRunner: FakeProcessRunner) {
  return Layer.merge(TestServicesLayer, processRunner.layer);
}

function assertSuccess<A, E>(exit: Exit.Exit<A, E>): A {
  if (Exit.isFailure(exit)) assert.fail(Cause.pretty(exit.cause));
  return exit.value;
}

describe('Effect agents polling', () => {
  test('captures environment and explicit argv/options at construction', () => {
    const env: Record<string, string | undefined> = {
      FLEETDECK_AGENTS_CMD: 'fixture --literal | $(not-a-shell)',
      FLEETDECK_AGENTS_IDLE_POLL_MS: '250',
      FLEETDECK_AGENTS_POLL_MS: '10',
    };
    const fromEnv = resolveAgentsPollOptions({ env });
    env['FLEETDECK_AGENTS_CMD'] = 'false';
    env['FLEETDECK_AGENTS_IDLE_POLL_MS'] = '999';

    assert.deepEqual(fromEnv.argv, ['fixture', '--literal', '|', '$(not-a-shell)']);
    assert.equal(fromEnv.pollIntervalMs, 100, 'environment cadence retains the 100 ms floor');
    assert.equal(fromEnv.idlePollIntervalMs, 250);
    assert.equal(fromEnv.firstRunDelayMs, 100);

    const argv = ['fixture', 'before-mutation'];
    const ownedBy = () => true;
    const explicit = resolveAgentsPollOptions({
      argv,
      firstRunDelayMs: 3,
      idlePollIntervalMs: 19,
      pollIntervalMs: 7,
      processOwnedBy: ownedBy,
    });
    argv[1] = 'after-mutation';

    assert.deepEqual(explicit.argv, ['fixture', 'before-mutation']);
    assert.equal(explicit.firstRunDelayMs, 3);
    assert.equal(explicit.idlePollIntervalMs, 19);
    assert.equal(explicit.pollIntervalMs, 7);
    assert.equal(explicit.processOwnedBy, ownedBy);
    assert.equal(resolveAgentsPollOptions({ env: { FLEETDECK_AGENTS_CMD: 'false' } }).argv, null);
    assert.equal(resolveAgentsPollOptions({ env: { FLEETDECK_AGENTS_CMD: '  ' } }).argv, null);
  });

  test('keeps liveness on the active completion-spaced cadence when the CLI is disabled', async () => {
    const processRunner = makeFakeProcessRunner();
    let livenessTicks = 0;
    const scenario = Effect.gen(function* () {
      const fiber = yield* Effect.forkChild(
        makeAgentsPollProgram(
          {
            ingestAgentsPoll() {
              assert.fail('disabled agents CLI must not ingest');
            },
            spawnLivenessTick() {
              livenessTicks++;
            },
          },
          {
            argv: null,
            firstRunDelayMs: 5,
            idlePollIntervalMs: 100,
            pollIntervalMs: 10,
          },
        ),
      );
      const owner = makeAgentsPollOwner(fiber);
      yield* Effect.yieldNow;

      yield* TestClock.adjust(Duration.millis(4));
      assert.equal(livenessTicks, 0);
      yield* TestClock.adjust(Duration.millis(1));
      assert.equal(livenessTicks, 1);
      yield* TestClock.adjust(Duration.millis(9));
      assert.equal(livenessTicks, 1);
      yield* TestClock.adjust(Duration.millis(1));
      assert.equal(livenessTicks, 2);
      yield* TestClock.adjust(Duration.millis(10));
      assert.equal(livenessTicks, 3);

      yield* Effect.promise(() => owner.close());
      yield* TestClock.adjust(Duration.seconds(1));
      assert.equal(livenessTicks, 3);
    });

    const exit = await runEffectExit(Effect.provide(scenario, testLayer(processRunner)));
    assertSuccess(exit);
    assert.equal(processRunner.requests.length, 0);
  });

  test('preserves completion spacing, active/idle cadence, ownership eligibility, and no overlap', async () => {
    const runStarts: number[] = [];
    const ownershipChecks: Array<readonly [number, number]> = [];
    let inFlight = 0;
    let maximumInFlight = 0;
    let livenessTicks = 0;
    const activeRecords = [
      { kind: 'background', pid: 11, startedAt: 111 },
      { kind: 'interactive', pid: 12, startedAt: 112 },
      { kind: 'interactive', pid: 13, startedAt: 113 },
    ];
    const outputs = [JSON.stringify(activeRecords), '[]', '[]'];
    const processRunner = makeFakeProcessRunner({
      execute: () =>
        Effect.gen(function* () {
          inFlight++;
          maximumInFlight = Math.max(maximumInFlight, inFlight);
          runStarts.push(yield* Clock.currentTimeMillis);
          yield* Effect.sleep(Duration.millis(4));
          inFlight--;
          return { ok: true as const, out: outputs[runStarts.length - 1] ?? '[]' };
        }),
    });
    const ingests: unknown[] = [];

    const scenario = Effect.gen(function* () {
      const fiber = yield* Effect.forkChild(
        makeAgentsPollProgram(
          {
            ingestAgentsPoll(records) {
              ingests.push(records);
            },
            spawnLivenessTick() {
              livenessTicks++;
            },
          },
          {
            argv: ['fixture', '--json'],
            firstRunDelayMs: 5,
            idlePollIntervalMs: 30,
            pollIntervalMs: 10,
            processOwnedBy(pid, startedAt) {
              ownershipChecks.push([pid, startedAt]);
              return pid === 13;
            },
          },
        ),
      );
      const owner = makeAgentsPollOwner(fiber);
      yield* Effect.yieldNow;

      yield* TestClock.adjust(Duration.millis(5));
      assert.deepEqual(runStarts, [5]);
      yield* TestClock.adjust(Duration.millis(4));
      assert.equal(livenessTicks, 1);
      yield* TestClock.adjust(Duration.millis(10));
      assert.deepEqual(runStarts, [5, 19], 'next active run is spaced from prior completion');
      yield* TestClock.adjust(Duration.millis(4));
      assert.equal(livenessTicks, 2);
      yield* TestClock.adjust(Duration.millis(10));
      yield* TestClock.adjust(Duration.millis(10));
      assert.deepEqual(runStarts, [5, 19], 'idle CLI skips retain the liveness cadence');
      yield* TestClock.adjust(Duration.millis(10));
      assert.deepEqual(runStarts, [5, 19, 53], 'empty registry backs the CLI off from t23 to t53');
      yield* TestClock.adjust(Duration.millis(4));

      yield* Effect.promise(() => owner.close());
    });

    const exit = await runEffectExit(Effect.provide(scenario, testLayer(processRunner)));
    assertSuccess(exit);
    assert.equal(maximumInFlight, 1);
    assert.equal(inFlight, 0);
    assert.equal(livenessTicks, 5);
    assert.deepEqual(ownershipChecks, [
      [12, 112],
      [13, 113],
    ]);
    assert.deepEqual(ingests, [activeRecords, [], []]);
    assert.deepEqual(
      processRunner.requests.map(({ argv, timeoutMs }) => ({ argv, timeoutMs })),
      [
        { argv: ['fixture', '--json'], timeoutMs: AGENTS_POLL_EXEC_TIMEOUT_MS },
        { argv: ['fixture', '--json'], timeoutMs: AGENTS_POLL_EXEC_TIMEOUT_MS },
        { argv: ['fixture', '--json'], timeoutMs: AGENTS_POLL_EXEC_TIMEOUT_MS },
      ],
    );
  });

  test('skips invalid JSON, process failures, and named callback errors without stopping', async () => {
    let invocation = 0;
    const failureResult = { ok: false as const, code: 1, err: 'fixture failure' };
    const processRunner = makeFakeProcessRunner({
      execute: () => {
        invocation++;
        if (invocation === 1) return Effect.succeed({ ok: true as const, out: '{bad json' });
        if (invocation === 2) {
          return Effect.fail(
            new ProcessNonZeroExitError({
              exitCode: 1,
              message: failureResult.err,
              result: failureResult,
            }),
          );
        }
        if (invocation === 3) {
          return Effect.fail(
            new ProcessTimeoutError({
              message: 'fixture timed out',
              result: { ...failureResult, code: 'ETIMEDOUT' },
              timeoutMs: AGENTS_POLL_EXEC_TIMEOUT_MS,
            }),
          );
        }
        return Effect.succeed({ ok: true as const, out: invocation === 4 ? '[]' : 'null' });
      },
    });
    const ingests: unknown[] = [];
    let livenessTicks = 0;

    const scenario = Effect.gen(function* () {
      const fiber = yield* Effect.forkChild(
        makeAgentsPollProgram(
          {
            ingestAgentsPoll(records) {
              ingests.push(records);
              throw new Error('named ingest skip');
            },
            spawnLivenessTick() {
              livenessTicks++;
              if (livenessTicks === 1) throw new Error('named sync liveness skip');
              if (livenessTicks === 2) return Promise.reject(new Error('named async skip'));
              return undefined;
            },
          },
          {
            argv: ['fixture'],
            firstRunDelayMs: 0,
            idlePollIntervalMs: 10,
            pollIntervalMs: 10,
          },
        ),
      );
      const owner = makeAgentsPollOwner(fiber);
      yield* Effect.yieldNow;
      for (let index = 1; index < 5; index += 1) {
        yield* TestClock.adjust(Duration.millis(10));
      }
      yield* Effect.promise(() => owner.close());
    });

    const exit = await runEffectExit(Effect.provide(scenario, testLayer(processRunner)));
    assertSuccess(exit);
    assert.equal(invocation, 5);
    assert.equal(livenessTicks, 5);
    assert.deepEqual(ingests, [[], null]);
  });

  test('owner close interrupts and joins process cleanup, suppresses late callbacks, and is idempotent', async () => {
    const commandStarted = latch();
    const releaseResult = latch();
    const cleanupStarted = latch();
    const releaseCleanup = latch();
    let runs = 0;
    let ingests = 0;
    let livenessTicks = 0;
    const processRunner = makeFakeProcessRunner({
      execute: () =>
        Effect.callback<ProcessSuccess>((resume) => {
          runs++;
          commandStarted.release();
          void releaseResult.promise.then(() => {
            resume(Effect.succeed({ ok: true, out: '[]' }));
          });
          return Effect.promise(async () => {
            cleanupStarted.release();
            await releaseCleanup.promise;
          });
        }),
    });

    const scenario = Effect.gen(function* () {
      const fiber = yield* Effect.forkChild(
        makeAgentsPollProgram(
          {
            ingestAgentsPoll() {
              ingests++;
            },
            spawnLivenessTick() {
              livenessTicks++;
            },
          },
          {
            argv: ['fixture'],
            firstRunDelayMs: 0,
            idlePollIntervalMs: 10,
            pollIntervalMs: 10,
          },
        ),
      );
      const owner = makeAgentsPollOwner(fiber);
      yield* Effect.promise(() => commandStarted.promise);

      const firstClose = owner.close();
      const secondClose = owner.close();
      assert.equal(secondClose, firstClose);
      yield* Effect.promise(() => cleanupStarted.promise);
      let closeSettled = false;
      void firstClose.then(() => {
        closeSettled = true;
      });

      yield* TestClock.adjust(Duration.seconds(1));
      assert.equal(runs, 1, 'a blocked command cannot overlap with another poll');
      assert.equal(closeSettled, false, 'close joins the ProcessRunner interruption finalizer');

      releaseResult.release();
      yield* Effect.promise(() => Promise.resolve());
      assert.equal(ingests, 0, 'a late process success cannot mutate the core');
      assert.equal(livenessTicks, 0, 'later callbacks in the cancelled tick remain suppressed');

      releaseCleanup.release();
      yield* Effect.promise(() => firstClose);
      const fiberExit = yield* Effect.promise(() => owner.exit);
      assert.ok(Exit.isFailure(fiberExit));
      assert.equal(Exit.hasInterrupts(fiberExit), true);
      assert.equal(owner.close(), firstClose, 'idempotence survives settlement');

      yield* TestClock.adjust(Duration.seconds(1));
      assert.equal(runs, 1);
      assert.equal(ingests, 0);
      assert.equal(livenessTicks, 0);
    });

    const exit = await runEffectExit(Effect.provide(scenario, testLayer(processRunner)));
    assertSuccess(exit);
  });

  test('unexpected defects terminate the scheduler and remain defects on the owner exit', async () => {
    const defect = new Error('unexpected ProcessRunner defect');
    let livenessTicks = 0;
    const processRunner = makeFakeProcessRunner({ execute: () => Effect.die(defect) });
    const scenario = Effect.gen(function* () {
      const fiber = yield* Effect.forkChild(
        makeAgentsPollProgram(
          {
            ingestAgentsPoll() {
              assert.fail('a defect cannot produce a poll result');
            },
            spawnLivenessTick() {
              livenessTicks++;
            },
          },
          { argv: ['fixture'], firstRunDelayMs: 0, pollIntervalMs: 10 },
        ),
      );
      const owner = makeAgentsPollOwner(fiber);
      const fiberExit = yield* Effect.promise(() => owner.exit);
      const closing = owner.close();
      assert.equal(owner.close(), closing);
      yield* Effect.promise(() => closing);
      return fiberExit;
    });

    const exit = await runEffectExit(Effect.provide(scenario, testLayer(processRunner)));
    const fiberExit = assertSuccess(exit);
    assert.ok(Exit.isFailure(fiberExit));
    assert.equal(Exit.hasFails(fiberExit), false);
    assert.equal(Exit.hasDies(fiberExit), true);
    assert.equal(Cause.squash(fiberExit.cause), defect);
    assert.equal(livenessTicks, 0);
  });
});
