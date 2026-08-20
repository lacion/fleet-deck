import assert from 'node:assert/strict';

import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Deferred from 'effect/Deferred';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Fiber from 'effect/Fiber';
import * as Layer from 'effect/Layer';

import { makeAgentsPollProgram } from '../../src/daemon/app/agents-poll.ts';
import {
  BackgroundProgramUnexpectedExitError,
  makeDaemonBackgroundProgram,
} from '../../src/daemon/app/background-program.ts';
import { lanRefresh } from '../../src/daemon/app/lan-refresh.ts';
import { makeRetentionSchedule } from '../../src/daemon/app/retention-schedule.ts';
import { makeFakeProcessRunner } from './fake-layers.ts';
import { runEffectExit, TestClock, TestServicesLayer } from './helpers.ts';

function assertSuccess<A, E>(exit: Exit.Exit<A, E>): A {
  if (Exit.isFailure(exit)) assert.fail(Cause.pretty(exit.cause));
  return exit.value;
}

describe('P5 aggregate Background program', () => {
  test('runs one ordered boot and all schedules, then root interruption stops every callback', async () => {
    const bootEvents: string[] = [];
    let agentsTicks = 0;
    let lanRefreshes = 0;
    let retentionSweeps = 0;
    let eventPrunes = 0;
    const processRunner = makeFakeProcessRunner();

    const scenario = Effect.gen(function* () {
      const ready = yield* Deferred.make<void>();
      const controller = {
        markReconciliationReady: Effect.sync(() => {
          bootEvents.push('ready');
        }).pipe(Effect.andThen(Deferred.succeed(ready, undefined)), Effect.asVoid),
      };
      const retention = yield* makeRetentionSchedule({
        interval: '100 millis',
        eventWindowMs: 1_000,
        pruneEvents: () =>
          Effect.sync(() => {
            eventPrunes++;
          }),
        retentionSweep: () =>
          Effect.sync(() => {
            retentionSweeps++;
            bootEvents.push(`retention-${String(retentionSweeps)}`);
          }),
      });
      const program = makeDaemonBackgroundProgram(controller, {
        agentsPoll: makeAgentsPollProgram(
          {
            ingestAgentsPoll() {
              assert.fail('disabled agents CLI cannot ingest');
            },
            spawnLivenessTick() {
              agentsTicks++;
            },
          },
          {
            argv: null,
            firstRunDelayMs: 10,
            idlePollIntervalMs: 20,
            pollIntervalMs: 20,
          },
        ),
        lanRefresh: lanRefresh({
          interval: '25 millis',
          readAddresses: () => Effect.succeed(['10.0.0.2']),
          previousAddresses: () => Effect.succeed(['10.0.0.1']),
          onChange: () =>
            Effect.sync(() => {
              lanRefreshes++;
            }),
        }),
        retention,
        boot: {
          clearForkHealing: Effect.sync(() => {
            bootEvents.push('clear-forks');
          }),
          reconcileSpawns: Effect.sync(() => {
            bootEvents.push('reconcile-spawns');
          }),
          awaitBroadcastIdle: Effect.sync(() => {
            bootEvents.push('broadcast-idle');
          }),
        },
      });

      const fiber = yield* Effect.forkChild(program);
      yield* Deferred.await(ready);

      const clearIndex = bootEvents.indexOf('clear-forks');
      const reconcileIndex = bootEvents.indexOf('reconcile-spawns');
      const retentionIndex = bootEvents.indexOf('retention-1');
      const broadcastIndex = bootEvents.indexOf('broadcast-idle');
      const readyIndex = bootEvents.indexOf('ready');
      assert.equal(clearIndex, 0);
      assert.ok(reconcileIndex > clearIndex);
      assert.ok(retentionIndex > clearIndex);
      assert.ok(broadcastIndex > reconcileIndex);
      assert.ok(broadcastIndex > retentionIndex);
      assert.ok(readyIndex > broadcastIndex);
      assert.equal(retentionSweeps, 1, "readiness used the schedule's one boot sweep");

      yield* TestClock.adjust('100 millis');
      assert.ok(agentsTicks >= 1, 'agents cadence advanced');
      assert.ok(lanRefreshes >= 1, 'LAN cadence advanced');
      assert.equal(eventPrunes, 1, 'retention periodic prune advanced on its grid');
      assert.equal(retentionSweeps, 2, 'retention periodic sweep did not duplicate boot');

      yield* Fiber.interrupt(fiber);
      const stopped = { agentsTicks, lanRefreshes, retentionSweeps, eventPrunes };
      yield* TestClock.adjust('1 second');
      assert.deepEqual(
        { agentsTicks, lanRefreshes, retentionSweeps, eventPrunes },
        stopped,
        'root interruption left no scheduled callback',
      );
    });

    assertSuccess(
      await runEffectExit(
        Effect.provide(scenario, Layer.merge(TestServicesLayer, processRunner.layer)),
      ),
    );
    assert.equal(processRunner.requests.length, 0);
  });

  test('turns an impossible daemon-long success into a defect and interrupts siblings', async () => {
    let siblingFinalized = false;
    let publishSiblingStarted: () => void = () => undefined;
    const siblingStarted = new Promise<void>((resolve) => {
      publishSiblingStarted = resolve;
    });
    const sibling = Effect.acquireRelease(Effect.sync(publishSiblingStarted), () =>
      Effect.sync(() => {
        siblingFinalized = true;
      }),
    ).pipe(Effect.andThen(Effect.never));
    const impossibleSuccess = Effect.promise(() => siblingStarted).pipe(
      Effect.asVoid,
    ) as unknown as Effect.Effect<never>;
    const neverRetention = {
      awaitFirstRun: Effect.never,
      program: Effect.never,
    };
    const exit = await runEffectExit(
      Effect.scoped(
        makeDaemonBackgroundProgram(
          { markReconciliationReady: Effect.void },
          {
            agentsPoll: impossibleSuccess,
            lanRefresh: sibling,
            retention: neverRetention,
            boot: {
              clearForkHealing: Effect.never,
              reconcileSpawns: Effect.never,
              awaitBroadcastIdle: Effect.never,
            },
          },
        ),
      ),
    );

    assert.ok(Exit.isFailure(exit));
    assert.equal(Exit.hasFails(exit), false);
    assert.equal(Exit.hasDies(exit), true);
    const defect = Cause.squash(exit.cause);
    assert.ok(defect instanceof BackgroundProgramUnexpectedExitError);
    assert.equal(defect.program, 'agents-poll');
    assert.equal(siblingFinalized, true);
  });
});
