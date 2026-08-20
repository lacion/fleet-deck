import assert from 'node:assert/strict';
import { describe, test } from 'bun:test';
import * as Deferred from 'effect/Deferred';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Fiber from 'effect/Fiber';
import {
  lanRefresh,
  sameLanAddressSet,
  type LanRefreshOptions,
} from '../../src/daemon/app/lan-refresh.ts';
import { nextFixedGridDelayMs } from '../../src/daemon/app/fixed-grid-schedule.ts';
import { runEffectExit, TestClock, TestServicesLayer } from './helpers.ts';

interface TestFailure {
  readonly message: string;
}

function testProgram(
  overrides: Partial<LanRefreshOptions<TestFailure, TestFailure, never>> = {},
): LanRefreshOptions<TestFailure, TestFailure, never> {
  return {
    interval: '10 seconds',
    readAddresses: () => Effect.succeed(['10.0.0.2']),
    previousAddresses: () => Effect.succeed(['10.0.0.1']),
    onChange: () => Effect.void,
    ...overrides,
  };
}

describe('P5 Effect LAN refresh', () => {
  test('set comparison is order-independent and preserves duplicate cardinality', () => {
    assert.equal(sameLanAddressSet(['10.0.0.2', '10.0.0.1'], ['10.0.0.1', '10.0.0.2']), true);
    assert.equal(sameLanAddressSet(['10.0.0.1'], ['10.0.0.1', '10.0.0.1']), false);
  });

  test('fixed-grid policy always selects the next future boundary', () => {
    assert.equal(nextFixedGridDelayMs(1_000, 1_000, 100), 100);
    assert.equal(nextFixedGridDelayMs(1_000, 1_099, 100), 1);
    assert.equal(nextFixedGridDelayMs(1_000, 1_100, 100), 100);
    assert.equal(nextFixedGridDelayMs(1_000, 1_351, 100), 49);
    assert.throws(() => nextFixedGridDelayMs(0, 0, 0), /positive finite/);
  });

  test('delays the first read, skips equal sets, and refreshes changed addresses', async () => {
    const reads: number[] = [];
    const changes: Array<{ addresses: readonly string[]; previous: readonly string[] }> = [];
    let current = ['10.0.0.1'];
    let previous = ['10.0.0.1'];

    const program = Effect.gen(function* () {
      const fiber = yield* lanRefresh(
        testProgram({
          readAddresses: () =>
            Effect.sync(() => {
              reads.push(reads.length + 1);
              return current;
            }),
          previousAddresses: () => Effect.sync(() => previous),
          onChange: (addresses, before) =>
            Effect.sync(() => {
              changes.push({ addresses, previous: before });
              previous = [...addresses];
            }),
        }),
      ).pipe(Effect.forkChild);

      yield* TestClock.adjust('9999 millis');
      assert.deepEqual(reads, []);
      yield* TestClock.adjust('1 millis');
      assert.deepEqual(reads, [1]);
      assert.deepEqual(changes, []);

      current = ['10.0.0.2'];
      yield* TestClock.adjust('10 seconds');
      assert.deepEqual(changes, [{ addresses: ['10.0.0.2'], previous: ['10.0.0.1'] }]);
      yield* Fiber.interrupt(fiber);
    });

    const exit = await runEffectExit(Effect.provide(program, TestServicesLayer));
    assert.ok(Exit.isSuccess(exit));
  });

  test('keeps one callback in flight and skips missed fixed-grid ticks', async () => {
    const starts: number[] = [];

    const program = Effect.gen(function* () {
      const firstStarted = yield* Deferred.make<void>();
      const releaseFirst = yield* Deferred.make<void>();
      let previous = ['10.0.0.1'];
      let invocation = 0;
      const fiber = yield* lanRefresh(
        testProgram({
          readAddresses: () => Effect.succeed(['10.0.0.2']),
          previousAddresses: () => Effect.sync(() => previous),
          onChange: (addresses) =>
            Effect.gen(function* () {
              invocation += 1;
              starts.push(invocation);
              previous = [...addresses];
              if (invocation === 1) {
                yield* Deferred.succeed(firstStarted, undefined);
                yield* Deferred.await(releaseFirst);
                previous = ['10.0.0.1'];
              }
            }),
        }),
      ).pipe(Effect.forkChild);

      yield* TestClock.adjust('10 seconds');
      yield* Deferred.await(firstStarted);
      yield* TestClock.adjust('25 seconds');
      assert.deepEqual(starts, [1]);
      yield* Deferred.succeed(releaseFirst, undefined);
      yield* Effect.yieldNow;
      yield* TestClock.adjust('4999 millis');
      assert.deepEqual(starts, [1]);
      yield* TestClock.adjust('1 millis');
      assert.deepEqual(starts, [1, 2]);
      yield* Fiber.interrupt(fiber);
    });

    const exit = await runEffectExit(Effect.provide(program, TestServicesLayer));
    assert.ok(Exit.isSuccess(exit));
  });

  test('silences address-read failures and reports refresh failures without stopping', async () => {
    const reported: TestFailure[] = [];
    let reads = 0;

    const program = Effect.gen(function* () {
      const fiber = yield* lanRefresh(
        testProgram({
          readAddresses: () => {
            reads += 1;
            return reads === 1
              ? Effect.fail({ message: 'address read failed' })
              : Effect.succeed(['10.0.0.2']);
          },
          onChange: () => Effect.fail({ message: 'refresh failed' }),
          onError: (error) =>
            Effect.sync(() => {
              reported.push(error);
            }),
        }),
      ).pipe(Effect.forkChild);

      yield* TestClock.adjust('10 seconds');
      assert.deepEqual(reported, []);
      yield* TestClock.adjust('10 seconds');
      assert.deepEqual(reported, [{ message: 'refresh failed' }]);
      yield* TestClock.adjust('10 seconds');
      assert.equal(reads, 3);
      yield* Fiber.interrupt(fiber);
    });

    const exit = await runEffectExit(Effect.provide(program, TestServicesLayer));
    assert.ok(Exit.isSuccess(exit));
  });

  test('interrupts and joins the active callback with no late continuation', async () => {
    let lateMutation = 0;

    const program = Effect.gen(function* () {
      const entered = yield* Deferred.make<void>();
      const fiber = yield* lanRefresh(
        testProgram({
          onChange: () =>
            Effect.gen(function* () {
              yield* Deferred.succeed(entered, undefined);
              yield* Effect.never;
              lateMutation += 1;
            }),
        }),
      ).pipe(Effect.forkChild);

      yield* TestClock.adjust('10 seconds');
      yield* Deferred.await(entered);
      yield* Fiber.interrupt(fiber);
      yield* TestClock.adjust('1 hour');
      assert.equal(lateMutation, 0);
    });

    const exit = await runEffectExit(Effect.provide(program, TestServicesLayer));
    assert.ok(Exit.isSuccess(exit));
  });

  test('typed refresh failures are recoverable while defects still terminate the fiber', async () => {
    const defect = new Error('unexpected refresh defect');
    const program = Effect.gen(function* () {
      const fiber = yield* lanRefresh(
        testProgram({
          onChange: () => Effect.die(defect),
        }),
      ).pipe(Effect.forkChild);
      yield* TestClock.adjust('10 seconds');
      return yield* Fiber.await(fiber);
    });

    const outer = await runEffectExit(Effect.provide(program, TestServicesLayer));
    assert.ok(Exit.isSuccess(outer));
    assert.ok(Exit.isFailure(outer.value));
    assert.equal(Exit.hasDies(outer.value), true);
  });
});
