import assert from 'node:assert/strict';

import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Fiber from 'effect/Fiber';

import {
  type LanTickCallbacks,
  type LanTickWork,
  legacyLanTickWork,
  makeStoreLanTickWork,
} from '../../src/daemon/app/db-workflows/lan-tick.ts';
import { lanRefresh, type LanRefreshOptions } from '../../src/daemon/app/lan-refresh.ts';
import { Store } from '../../src/daemon/app/services/store.ts';
import { makeStoreOwner } from '../../src/daemon/app/store-owner.ts';
import type { SqliteHandle } from '../../src/daemon/sqlite.ts';
import { runEffectExit, TestClock, TestServicesLayer } from './helpers.ts';

/**
 * A fake SQLite handle that throws on every query path. Like the retention pilot
 * and the boot slice, this slice only YIELDS Store to declare the dependency; it
 * must not touch the handle yet, so any accidental handle access surfaces
 * immediately as a test defect.
 */
function fakeHandle(): SqliteHandle {
  const forbid = (): never => {
    throw new Error('P8.6 lan-tick workflow must not touch the SQLite handle yet');
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
  assert.ok(Exit.isFailure(exit), 'expected a typed failure');
  const errors = exit.cause.reasons.filter(Cause.isFailReason).map((reason) => reason.error);
  const [error, ...rest] = errors;
  assert.ok(error, 'expected exactly one typed failure');
  assert.equal(rest.length, 0, 'expected exactly one typed failure');
  return error;
}

/**
 * Drive one wired LAN tick through the REAL lanRefresh loop, using the exact
 * onChange shape program.ts wires: a message-producing Effect.try (whose failure
 * is a RefreshError routed to onError) followed by the Store-backed tick under
 * the `catchTag('LanTickError')` silent swallow. Runs two fixed-grid refreshes
 * against a constant address change, then interrupts. Store is discharged once at
 * the fiber boundary, exactly as program.ts does. Legacy work (Environment
 * `never`) is assignable to the `Store` parameter (R is covariant), so the same
 * harness drives both adapters and their swallow outcomes compare byte-for-byte.
 */
async function runLanTickScenario(config: {
  readonly makeWork: (callbacks: LanTickCallbacks) => LanTickWork<Store>;
  readonly onTick: (message: string) => void;
  readonly buildMessageThrows?: boolean;
}): Promise<{ reported: unknown[]; closedCleanly: boolean }> {
  const reported: unknown[] = [];
  const work = config.makeWork({ tick: config.onTick });

  const options: LanRefreshOptions<never, unknown, Store> = {
    interval: '10 seconds',
    readAddresses: () => Effect.succeed(['10.0.0.2']),
    previousAddresses: () => Effect.succeed(['10.0.0.1']),
    // Mirror program.ts: the non-tick side effects live in an Effect.try that
    // returns the feed message (a throw there becomes the RefreshError onError
    // logs); the tick runs under the catchTag swallow that drops LanTickError.
    onChange: (addresses) =>
      Effect.gen(function* () {
        const message = yield* Effect.try({
          try: () => {
            if (config.buildMessageThrows) throw new Error('refresh side-effect failed');
            return `🌐 LAN ${addresses.join(', ')}`;
          },
          catch: (error) => error,
        });
        yield* work(message).pipe(Effect.catchTag('LanTickError', () => Effect.void));
      }),
    onError: (error) =>
      Effect.sync(() => {
        reported.push(error);
      }),
  };

  let closedCleanly = false;
  const program = Effect.gen(function* () {
    const fiber = yield* lanRefresh(options).pipe(Effect.forkChild);
    yield* TestClock.adjust('10 seconds');
    yield* TestClock.adjust('10 seconds');
    yield* Fiber.interrupt(fiber);
    const fiberExit = yield* Fiber.await(fiber);
    closedCleanly = Exit.hasInterrupts(fiberExit) && !Exit.hasDies(fiberExit);
  });

  const exit = await runEffectExit(
    Effect.provide(program, TestServicesLayer).pipe(Effect.provideService(Store, boundService())),
  );
  assert.ok(Exit.isSuccess(exit), Exit.isFailure(exit) ? Cause.pretty(exit.cause) : '');
  return { reported, closedCleanly };
}

describe('P8.6 store-backed LAN tick work', () => {
  test('yields Store and threads success through the existing sync tick', async () => {
    const ticks: string[] = [];
    const work = makeStoreLanTickWork({
      tick(message) {
        ticks.push(message);
      },
    });

    // The requirement is Store: the tick is only runnable once Store is
    // discharged — exactly the local provide program.ts performs on the fiber.
    const service = boundService();
    const exit = await runEffectExit(work('hello').pipe(Effect.provideService(Store, service)));
    assert.ok(Exit.isSuccess(exit), Exit.isFailure(exit) ? Cause.pretty(exit.cause) : '');
    assert.deepEqual(ticks, ['hello']);

    // Negative pin for the family convention itself: without the discharge the
    // tick must DIE on the missing Store service. R is covariant, so deleting the
    // `yield* Store` line would still typecheck and every provide-wrapped
    // assertion above would still pass — this is the one case that fails if the
    // yield is removed.
    const undischargedExit = await runEffectExit(work('nope') as Effect.Effect<void, never, never>);
    assert.ok(Exit.isFailure(undischargedExit) && Cause.hasDies(undischargedExit.cause));
    assert.match(Cause.pretty(undischargedExit.cause), /Service not found.*Store/);
    assert.deepEqual(ticks, ['hello'], 'the undischarged tick must never reach the callback');
  });

  test('a synchronous tick throw becomes the identical LanTickError as the legacy adapter', async () => {
    const boom = new Error('tick blew up');
    const storeWork = makeStoreLanTickWork({
      tick() {
        throw boom;
      },
    });
    const legacyWork = legacyLanTickWork({
      tick() {
        throw boom;
      },
    });

    const storeErr = soleFailure(
      await runEffectExit(storeWork('x').pipe(Effect.provideService(Store, boundService()))),
    );
    const legacyErr = soleFailure(await runEffectExit(legacyWork('x')));

    assert.equal(storeErr._tag, 'LanTickError');
    assert.equal(storeErr.cause, boom);
    // Byte-parity with the legacy capability-parameterized adapter: same tag and
    // preserved cause, so the onChange catchTag swallow drops that tag identically
    // either way.
    assert.deepEqual([storeErr._tag, storeErr.cause], [legacyErr._tag, legacyErr.cause]);
  });

  test('the onChange swallow drops a LanTickError but a missing-Store defect survives it', async () => {
    const boom = new Error('tick blew up');
    const throwingWork = makeStoreLanTickWork({
      tick() {
        throw boom;
      },
    });
    // The exact swallow program.ts's onChange applies around the wired tick.
    const swallow = (work: LanTickWork<Store>, message: string) =>
      work(message).pipe(Effect.catchTag('LanTickError', () => Effect.void));

    // A tick throw is a LanTickError → swallowed → success, exactly as the prior
    // inline `try { core.tick(...) } catch {}`.
    const swallowedExit = await runEffectExit(
      swallow(throwingWork, 'x').pipe(Effect.provideService(Store, boundService())),
    );
    assert.ok(
      Exit.isSuccess(swallowedExit),
      Exit.isFailure(swallowedExit) ? Cause.pretty(swallowedExit.cause) : '',
    );

    // A missing-Store defect is NOT a LanTickError, so the same swallow does not
    // launder it — the family's negative pin, at the wiring boundary.
    const undischargedExit = await runEffectExit(
      swallow(throwingWork, 'x') as Effect.Effect<void, never, never>,
    );
    assert.ok(Exit.isFailure(undischargedExit) && Cause.hasDies(undischargedExit.cause));
    assert.match(Cause.pretty(undischargedExit.cause), /Service not found.*Store/);
  });

  test('the program.ts wiring swallows a tick failure silently — onError is never called (Store provided once)', async () => {
    const attempts: string[] = [];
    const { reported, closedCleanly } = await runLanTickScenario({
      makeWork: makeStoreLanTickWork,
      onTick(message) {
        attempts.push(message);
        throw new Error('tick blew up');
      },
    });

    // Both fixed-grid refreshes reach the feed tick; each throw is dropped by the
    // catchTag swallow, so onError never fires and the loop survives to be
    // interrupted cleanly.
    assert.equal(attempts.length, 2, 'both grid refreshes attempted the feed tick');
    assert.deepEqual(reported, [], 'a tick failure never reaches onError');
    assert.equal(closedCleanly, true, 'the swallowing loop closes on interrupt, not a defect');
  });

  test('a non-tick refresh failure still reaches onError as a RefreshError', async () => {
    const attempts: string[] = [];
    const { reported } = await runLanTickScenario({
      makeWork: makeStoreLanTickWork,
      onTick(message) {
        attempts.push(message);
      },
      buildMessageThrows: true,
    });

    // The restructure preserved the RefreshError path: the message-producing step
    // is outside the tick swallow, so its failure is reported, and the tick — which
    // never receives a message — is never attempted.
    assert.equal(attempts.length, 0, 'the tick never runs when the message build fails');
    assert.equal(reported.length, 2, 'each refresh reports its build failure to onError');
    for (const error of reported) {
      assert.match(String((error as Error)?.message ?? error), /refresh side-effect failed/);
    }
  });

  test('the store-backed swallow is byte-identical to the legacy adapter', async () => {
    const storeAttempts: string[] = [];
    const legacyAttempts: string[] = [];
    const storeOutcome = await runLanTickScenario({
      makeWork: makeStoreLanTickWork,
      onTick(message) {
        storeAttempts.push(message);
        throw new Error('tick blew up');
      },
    });
    const legacyOutcome = await runLanTickScenario({
      makeWork: legacyLanTickWork,
      onTick(message) {
        legacyAttempts.push(message);
        throw new Error('tick blew up');
      },
    });

    // The yield* Store prefix changes nothing about the swallow boundary: same
    // reports (none), same clean interrupt, same messages handed to the tick.
    assert.deepEqual(storeOutcome, legacyOutcome);
    assert.deepEqual(storeAttempts, legacyAttempts);
  });
});
