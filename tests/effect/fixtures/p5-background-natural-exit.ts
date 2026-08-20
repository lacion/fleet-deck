import { writeSync } from 'node:fs';
import * as BunRuntime from '@effect/platform-bun/BunRuntime';
import * as Effect from 'effect/Effect';
import * as Runtime from 'effect/Runtime';
import * as Scope from 'effect/Scope';

import { makeAgentsPollProgram } from '../../../src/daemon/app/agents-poll.ts';
import { prepareBackgroundOwner } from '../../../src/daemon/app/background-owner.ts';
import { makeDaemonBackgroundProgram } from '../../../src/daemon/app/background-program.ts';
import { lanRefresh } from '../../../src/daemon/app/lan-refresh.ts';
import { makeRetentionSchedule } from '../../../src/daemon/app/retention-schedule.ts';
import { ProcessRunnerLive } from '../../../src/daemon/platform/bun/process-runner-live.ts';

type SchedulerName = 'agents' | 'lan' | 'retention';

interface Counters {
  agents: number;
  lan: number;
  retentionSweeps: number;
  retentionPrunes: number;
}

interface Observation {
  readonly event: string;
  readonly [key: string]: string | number | boolean;
}

const counters: Counters = {
  agents: 0,
  lan: 0,
  retentionSweeps: 0,
  retentionPrunes: 0,
};
const finalized = new Set<SchedulerName>();
let backgroundClosed = false;
let joinTimeoutFired = false;
let postCloseMutations = 0;

function observe(event: string, fields: Record<string, string | number | boolean> = {}): void {
  const observation: Observation = { event, ...fields };
  writeSync(process.stdout.fd, `${JSON.stringify(observation)}\n`);
}

function count(name: keyof Counters, firstEvent: string): void {
  if (backgroundClosed) postCloseMutations += 1;
  counters[name] += 1;
  if (counters[name] === 1) observe(firstEvent);
}

function withFinalizer<Environment>(
  name: SchedulerName,
  program: Effect.Effect<never, never, Environment>,
): Effect.Effect<never, never, Environment> {
  return program.pipe(
    Effect.ensuring(
      Effect.sleep('12 millis').pipe(
        Effect.andThen(
          Effect.sync(() => {
            finalized.add(name);
            observe('scheduler-finalized', { scheduler: name });
          }),
        ),
      ),
    ),
  );
}

function armReferencedJoinTimeout(callback: () => void, milliseconds: number): () => void {
  const timer = setTimeout(() => {
    joinTimeoutFired = true;
    callback();
  }, milliseconds);
  return () => clearTimeout(timer);
}

process.once('beforeExit', (code) => {
  observe('before-exit', { code, joinTimeoutFired, postCloseMutations });
});

process.once('exit', (code) => {
  observe('exit', { code, joinTimeoutFired, postCloseMutations });
});

const program = Effect.scoped(
  Effect.gen(function* () {
    const rootScope = yield* Effect.scope;
    yield* Scope.addFinalizer(
      rootScope,
      Effect.sync(() => observe('root-scope-closed')),
    );

    const baseRetention = yield* makeRetentionSchedule({
      interval: '50 millis',
      eventWindowMs: 1_000,
      pruneEvents: () =>
        Effect.sync(() => {
          count('retentionPrunes', 'retention-prune-first');
        }),
      retentionSweep: () =>
        Effect.sync(() => {
          count('retentionSweeps', 'retention-sweep-first');
        }),
    });
    const retention = {
      ...baseRetention,
      program: withFinalizer('retention', baseRetention.program),
    };

    const prepared = yield* prepareBackgroundOwner({
      name: 'p5-natural-exit',
      joinTimeoutMs: 2_000,
      armJoinTimeout: armReferencedJoinTimeout,
      run: (controller) =>
        makeDaemonBackgroundProgram(controller, {
          agentsPoll: withFinalizer(
            'agents',
            makeAgentsPollProgram(
              {
                ingestAgentsPoll() {
                  throw new Error('disabled agents CLI unexpectedly ingested a result');
                },
                spawnLivenessTick() {
                  count('agents', 'agents-first');
                },
              },
              {
                argv: null,
                firstRunDelayMs: 10,
                idlePollIntervalMs: 20,
                pollIntervalMs: 20,
              },
            ),
          ),
          lanRefresh: withFinalizer(
            'lan',
            lanRefresh({
              interval: '15 millis',
              readAddresses: () => Effect.succeed(['10.0.0.2']),
              previousAddresses: () => Effect.succeed(['10.0.0.1']),
              onChange: () =>
                Effect.sync(() => {
                  count('lan', 'lan-first');
                }),
            }),
          ),
          retention,
          boot: {
            clearForkHealing: Effect.sync(() => observe('boot-clear-forks')),
            reconcileSpawns: Effect.sync(() => observe('boot-reconcile-start')).pipe(
              Effect.andThen(Effect.sleep('8 millis')),
              Effect.andThen(Effect.sync(() => observe('boot-reconcile-complete'))),
            ),
            awaitBroadcastIdle: Effect.sleep('4 millis').pipe(
              Effect.andThen(Effect.sync(() => observe('boot-broadcast-idle'))),
            ),
          },
        }),
    });

    const owner = yield* prepared.start;
    yield* prepared.service.awaitReady;
    observe('ready', {
      status: prepared.service.reconciliationStatus(),
      retentionSweeps: counters.retentionSweeps,
    });

    yield* Effect.sleep('180 millis');
    observe('schedules-observed', { ...counters });

    yield* Effect.promise(() => owner.close());
    observe('background-closed', {
      ownerState: owner.state,
      finalized: [...finalized].sort().join(','),
    });

    backgroundClosed = true;
    const stopped = { ...counters };
    observe('store-close', {
      ...stopped,
      finalized: [...finalized].sort().join(','),
    });

    yield* Effect.sleep('90 millis');
    const unchanged =
      counters.agents === stopped.agents &&
      counters.lan === stopped.lan &&
      counters.retentionSweeps === stopped.retentionSweeps &&
      counters.retentionPrunes === stopped.retentionPrunes;
    observe('post-close-stable', {
      ...counters,
      postCloseMutations,
      unchanged,
    });
    if (!unchanged || postCloseMutations !== 0) {
      return yield* Effect.die(new Error('a background callback mutated after owner close'));
    }
  }),
).pipe(Effect.provide(ProcessRunnerLive));

const teardown: Runtime.Teardown = (exit, onExit) => {
  Runtime.defaultTeardown(exit, (code) => {
    observe('teardown', { code });
    onExit(code);
  });
};

BunRuntime.runMain(program, {
  disableErrorReporting: true,
  teardown,
});
