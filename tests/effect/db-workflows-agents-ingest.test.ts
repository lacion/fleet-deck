import assert from 'node:assert/strict';

import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Duration from 'effect/Duration';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';

import {
  type AgentsIngestCallbacks,
  type AgentsIngestWork,
  legacyAgentsIngestWork,
  makeAgentsPollOwner,
  makeAgentsPollProgram,
} from '../../src/daemon/app/agents-poll.ts';
import { makeStoreAgentsIngestWork } from '../../src/daemon/app/db-workflows/agents-ingest.ts';
import { ProcessNonZeroExitError } from '../../src/daemon/app/errors.ts';
import { ProcessRunner } from '../../src/daemon/app/services/process-runner.ts';
import { Store } from '../../src/daemon/app/services/store.ts';
import { makeStoreOwner } from '../../src/daemon/app/store-owner.ts';
import type { SqliteHandle } from '../../src/daemon/sqlite.ts';
import { type FakeProcessRunner, makeFakeProcessRunner } from './fake-layers.ts';
import { runEffectExit, TestClock, TestServicesLayer } from './helpers.ts';

/**
 * A fake SQLite handle that throws on every query path. Like the retention pilot
 * and the boot slice, this slice only YIELDS Store to declare the dependency; it
 * must not touch the handle yet, so any accidental handle access surfaces
 * immediately as a test defect. Later slices that thread `handle` will replace
 * this with a real scratch connection.
 */
function fakeHandle(): SqliteHandle {
  const forbid = (): never => {
    throw new Error('P8.6 agents-ingest workflow must not touch the SQLite handle yet');
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

const INTERACTIVE_RECORDS = [{ kind: 'interactive', pid: 13, startedAt: 113 }] as const;

/**
 * Drive one wired ingest work through the REAL scheduler, where every poll ingest
 * throws. Returns the observable loop outcome: how many polls ran, what each poll
 * handed the ingest, and whether the fiber closed on a clean interrupt (not a
 * defect). Legacy work (Environment `never`) is assignable to the `Store`
 * parameter (R is covariant), so the same harness drives both adapters and their
 * fail-open outcomes can be compared byte-for-byte. Store is discharged once at
 * the fiber boundary, exactly as program.ts does.
 */
async function runThrowingPollLoop(
  makeIngest: (callbacks: AgentsIngestCallbacks) => AgentsIngestWork<Store>,
): Promise<{ invocation: number; ingests: unknown[]; closedCleanly: boolean }> {
  let invocation = 0;
  const ingests: unknown[] = [];
  const ingest = makeIngest({
    ingestAgentsPoll(records) {
      ingests.push(records);
      throw new Error('named ingest skip');
    },
  });
  const processRunner = makeFakeProcessRunner({
    execute: () => {
      invocation++;
      return Effect.succeed({ ok: true as const, out: JSON.stringify(INTERACTIVE_RECORDS) });
    },
  });
  let closedCleanly = false;

  const scenario = Effect.gen(function* () {
    const fiber = yield* Effect.forkChild(
      makeAgentsPollProgram(
        {
          ingestAgentsPoll() {
            assert.fail('the wired ingest work must own ingestion, not the poll core');
          },
        },
        { argv: ['fixture'], firstRunDelayMs: 0, idlePollIntervalMs: 10, pollIntervalMs: 10 },
        ingest,
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
  return { invocation, ingests, closedCleanly };
}

describe('P8.6 store-backed agents ingest work', () => {
  test('yields Store and threads success through the existing sync ingest', async () => {
    const records = [{ kind: 'background', pid: 11, startedAt: 111 }];
    const ingested: unknown[] = [];
    const work = makeStoreAgentsIngestWork({
      ingestAgentsPoll(value) {
        ingested.push(value);
      },
    });

    // The requirement is Store: the ingest is only runnable once Store is
    // discharged — exactly the local provide program.ts performs on the fiber.
    const service = boundService();
    const exit = await runEffectExit(work(records).pipe(Effect.provideService(Store, service)));
    assert.ok(Exit.isSuccess(exit), Exit.isFailure(exit) ? Cause.pretty(exit.cause) : '');
    assert.deepEqual(ingested, [records]);

    // Negative pin for the family convention itself: without the discharge the
    // ingest must DIE on the missing Store service. R is covariant, so deleting
    // the `yield* Store` line would still typecheck and every provide-wrapped
    // assertion above would still pass — this is the one case that fails if the
    // yield is removed.
    const undischargedExit = await runEffectExit(
      work(records) as Effect.Effect<void, never, never>,
    );
    assert.ok(Exit.isFailure(undischargedExit) && Cause.hasDies(undischargedExit.cause));
    assert.match(Cause.pretty(undischargedExit.cause), /Service not found.*Store/);
    assert.deepEqual(ingested, [records], 'the undischarged ingest must never reach the callback');
  });

  test('a synchronous ingest throw becomes the identical named failure as the legacy adapter', async () => {
    const boom = new Error('ingest blew up');
    const records = [{ kind: 'interactive', pid: 12, startedAt: 112 }];
    const storeWork = makeStoreAgentsIngestWork({
      ingestAgentsPoll() {
        throw boom;
      },
    });
    const legacyWork = legacyAgentsIngestWork({
      ingestAgentsPoll() {
        throw boom;
      },
    });

    const storeErr = soleFailure(
      await runEffectExit(storeWork(records).pipe(Effect.provideService(Store, boundService()))),
    );
    const legacyErr = soleFailure(await runEffectExit(legacyWork(records)));

    assert.equal(storeErr._tag, 'AgentsPollIngestError');
    assert.equal(storeErr.cause, boom);
    // Byte-parity with the legacy capability-parameterized adapter: same tag and
    // preserved cause, so the scheduler's fail-open ingestPoll boundary catches
    // that tag identically either way.
    assert.deepEqual([storeErr._tag, storeErr.cause], [legacyErr._tag, legacyErr.cause]);
  });

  test('the program.ts wiring threads records through the store work with Store provided once', async () => {
    let invocation = 0;
    const ingests: unknown[] = [];
    const storeWork = makeStoreAgentsIngestWork({
      ingestAgentsPoll(records) {
        ingests.push(records);
      },
    });
    const processRunner = makeFakeProcessRunner({
      execute: () => {
        invocation++;
        return invocation === 1
          ? Effect.succeed({ ok: true as const, out: JSON.stringify(INTERACTIVE_RECORDS) })
          : Effect.fail(
              new ProcessNonZeroExitError({
                exitCode: 1,
                message: 'idle',
                result: { ok: false as const, code: 1, err: 'idle' },
              }),
            );
      },
    });

    const scenario = Effect.gen(function* () {
      const fiber = yield* Effect.forkChild(
        // Mirror program.ts exactly: makeAgentsPollProgram(core, options, wiredWork).
        // The poll core's own ingestAgentsPoll must never fire — the injected work
        // owns the DB leg.
        makeAgentsPollProgram(
          {
            ingestAgentsPoll() {
              assert.fail('the injected store work must own ingestion, not the poll core');
            },
          },
          { argv: ['fixture'], firstRunDelayMs: 0, idlePollIntervalMs: 10, pollIntervalMs: 10 },
          storeWork,
        ),
      );
      const owner = makeAgentsPollOwner(fiber);
      yield* Effect.yieldNow;
      for (let index = 1; index < 4; index += 1) {
        yield* TestClock.adjust(Duration.millis(10));
      }
      yield* Effect.promise(() => owner.close());
    });

    // Store is discharged ONCE at the fiber boundary; the forked poll child
    // inherits it, so the store-backed ingest runs on every ValidPoll.
    const exit = await runEffectExit(
      Effect.provide(scenario, testLayer(processRunner)).pipe(
        Effect.provideService(Store, boundService()),
      ),
    );
    assertSuccess(exit);
    assert.deepEqual(ingests, [INTERACTIVE_RECORDS], 'only the one ValidPoll is ingested');
  });

  test('a throwing store-backed ingest is swallowed by the scheduler fail-open skip; the loop keeps polling', async () => {
    const { invocation, ingests, closedCleanly } =
      await runThrowingPollLoop(makeStoreAgentsIngestWork);

    // firstRunDelayMs 0 fires the poll at t0, then three 10 ms grid ticks fire
    // three more: the ingest throws on every one, the named fail-open skip
    // swallows each, and the loop is still alive to be interrupted cleanly.
    assert.equal(invocation, 4);
    assert.equal(ingests.length, 4);
    for (const entry of ingests) assert.deepEqual(entry, INTERACTIVE_RECORDS);
    assert.equal(closedCleanly, true, 'the fail-open loop closes on interrupt, not a defect');
  });

  test('a missing-Store defect is not laundered by the fail-open skip and terminates the scheduler', async () => {
    const processRunner = makeFakeProcessRunner({
      execute: () =>
        Effect.succeed({ ok: true as const, out: JSON.stringify(INTERACTIVE_RECORDS) }),
    });
    const storeWork = makeStoreAgentsIngestWork({
      ingestAgentsPoll() {
        assert.fail('a missing-Store ingest must die before it reaches the callback');
      },
    });

    const scenario = Effect.gen(function* () {
      const fiber = yield* Effect.forkChild(
        // Cast drops the Store requirement so the program is runnable under a
        // ProcessRunner-only layer: the missing service surfaces at runtime as a
        // defect inside the tick, exactly the case the fail-open catchTag must
        // NOT launder.
        makeAgentsPollProgram(
          {
            ingestAgentsPoll() {
              assert.fail('unused poll-core ingest');
            },
          },
          { argv: ['fixture'], firstRunDelayMs: 0, pollIntervalMs: 10 },
          storeWork,
        ) as Effect.Effect<never, never, ProcessRunner>,
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
    const storeOutcome = await runThrowingPollLoop(makeStoreAgentsIngestWork);
    const legacyOutcome = await runThrowingPollLoop(legacyAgentsIngestWork);

    // The yield* Store prefix changes nothing about the fail-open loop: same poll
    // count, same records handed to the ingest, same clean interrupt on close.
    assert.deepEqual(storeOutcome, legacyOutcome);
  });
});
