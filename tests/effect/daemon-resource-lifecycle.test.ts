import assert from 'node:assert/strict';
import { describe, test } from 'bun:test';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import { DaemonResources } from '../../src/daemon/daemon-resources.ts';
import { makeDaemonResourceLifecycleOwner } from '../../src/daemon/app/daemon-resource-lifecycle.ts';
import {
  ForceLatch,
  LifecycleCoordinator,
  ShutdownBudget,
  type LifecycleOwner,
  type ShutdownPhase,
  type ShutdownPhaseContext,
} from '../../src/daemon/app/lifecycle-coordinator.ts';

const runTestPromise = Effect.runPromiseWith(Context.empty());

interface Deferred<T> {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
  readonly reject: (error: unknown) => void;
}

function deferred<T = void>(): Deferred<T> {
  let resolve: (value: T) => void = () => undefined;
  let reject: (error: unknown) => void = () => undefined;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function phaseContext(phase: ShutdownPhase, timeoutMs = 1_000): ShutdownPhaseContext {
  const budget = ShutdownBudget.start(timeoutMs);
  const forceLatch = new ForceLatch();
  return {
    phase,
    trigger: { _tag: 'Requested' },
    budget,
    forceLatch,
    startedAtMs: budget.startedAtMs,
    deadlineMs: budget.deadlineMs,
    remainingMs: () => budget.remainingMs(),
    isForced: () => forceLatch.forced,
  };
}

async function waitFor(predicate: () => boolean, label: string, timeoutMs = 1_000): Promise<void> {
  const deadline = performance.now() + timeoutMs;
  while (!predicate()) {
    if (performance.now() >= deadline) throw new Error(`timed out waiting for ${label}`);
    await Bun.sleep(1);
  }
}

function fullResources(
  events: string[],
  options: {
    readonly graceful?: Promise<void>;
    readonly force?: () => Promise<void>;
    readonly forceClients?: () => void;
    readonly producer?: () => void | Promise<void>;
    readonly closeClients?: () => void | Promise<void>;
    readonly join?: () => void | Promise<void>;
    readonly processDriverClose?: () => void | Promise<void>;
    readonly processDriverForce?: () => void;
  } = {},
): DaemonResources {
  return new DaemonResources({
    http: {
      quiesce() {
        events.push('http.quiesce');
      },
      beginGracefulStop() {
        events.push('http.beginGracefulStop');
        return options.graceful ?? Promise.resolve();
      },
      forceStop() {
        events.push('http.forceStop');
        return options.force?.() ?? Promise.resolve();
      },
      releaseHolds() {
        events.push('http.releaseHolds');
      },
      async closeClients() {
        events.push('http.closeClients');
        await options.closeClients?.();
      },
      forceClients() {
        events.push('http.forceClients');
        options.forceClients?.();
      },
      close() {
        events.push('http.close');
      },
    },
    core: {
      quiesce() {
        events.push('core.quiesce');
      },
      close() {
        events.push('core.close');
      },
    },
    producers: [
      {
        name: 'producer-a',
        owner: { close: () => events.push('producer-a.close') },
      },
      {
        name: 'producer-b',
        owner: {
          async close() {
            events.push('producer-b.close');
            await options.producer?.();
          },
        },
      },
    ],
    discovery: [
      {
        name: 'discovery',
        owner: { close: () => events.push('discovery.close') },
      },
    ],
    processRuntime: {
      name: 'ingress',
      owner: {
        quiesce() {
          events.push('ingress.quiesce');
        },
        interrupt() {
          events.push('ingress.interrupt');
        },
        async join() {
          events.push('ingress.join');
          await options.join?.();
        },
        close() {
          events.push('ingress.close');
        },
      },
    },
    processDriver: {
      name: 'bun-process-driver',
      owner: {
        force() {
          events.push('process-driver.force');
          options.processDriverForce?.();
        },
        async close() {
          events.push('process-driver.close');
          await options.processDriverClose?.();
        },
      },
    },
    store: {
      name: 'sqlite',
      owner: { close: () => events.push('sqlite.close') },
    },
    process: {
      name: 'host-process',
      owner: { close: () => events.push('host-process.close') },
    },
  });
}

describe('DaemonResources LifecycleOwner adapter', () => {
  test('executes all eight resource-sized phases once in policy order', async () => {
    const events: string[] = [];
    const resources = fullResources(events);
    const owner = makeDaemonResourceLifecycleOwner(resources);
    assert.equal(makeDaemonResourceLifecycleOwner(resources), owner);

    const coordinator = new LifecycleCoordinator(owner, { timeoutMs: 1_000 });
    const close = coordinator.close();
    assert.equal(coordinator.close(), close);
    const outcome = await runTestPromise(close);
    await runTestPromise(coordinator.close());

    assert.deepEqual(
      outcome.phases.map(({ phase, _tag }) => [phase, _tag]),
      [
        ['quiescing', 'Completed'],
        ['stopping-producers', 'Completed'],
        ['withdrawing', 'Completed'],
        ['releasing-holds', 'Completed'],
        ['closing-clients', 'Completed'],
        ['closing-http', 'Completed'],
        ['closing-store', 'Completed'],
        ['releasing-process', 'Completed'],
      ],
    );
    assert.deepEqual(events, [
      'ingress.quiesce',
      'http.quiesce',
      'http.beginGracefulStop',
      'core.quiesce',
      'producer-b.close',
      'producer-a.close',
      'discovery.close',
      'http.releaseHolds',
      'ingress.interrupt',
      'process-driver.close',
      'http.closeClients',
      'ingress.join',
      'core.close',
      'ingress.close',
      'sqlite.close',
    ]);
    assert.deepEqual(resources.closeErrors, []);
  });

  test('publishes each phase completion before a callback can re-enter it', async () => {
    const events: string[] = [];
    let owner: LifecycleOwner;
    let reentered: void | PromiseLike<void> | undefined;
    const context = phaseContext('quiescing');
    const resources = new DaemonResources({
      processRuntime: {
        name: 'ingress',
        owner: {
          quiesce() {
            events.push('ingress.quiesce');
            reentered = owner.quiescing.run(context);
          },
          interrupt() {},
          join() {},
          close() {},
        },
      },
    });
    owner = makeDaemonResourceLifecycleOwner(resources);

    const first = owner.quiescing.run(context);
    const second = owner.quiescing.run(context);
    assert.equal(reentered, first);
    assert.equal(second, first);
    await first;
    assert.deepEqual(events, ['ingress.quiesce']);
  });

  test('starts every producer stop latch before joining a stuck producer', async () => {
    const events: string[] = [];
    const blockedProducer = deferred();
    const resources = fullResources(events, { producer: () => blockedProducer.promise });
    const coordinator = new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(resources), {
      timeoutMs: 40,
      forceReserveMs: 20,
    });

    const outcome = await runTestPromise(coordinator.close());
    assert.equal(outcome.deadlineExpired, true);
    assert.ok(events.indexOf('producer-b.close') >= 0);
    assert.ok(events.indexOf('producer-a.close') > events.indexOf('producer-b.close'));
    assert.ok(events.indexOf('discovery.close') > events.indexOf('producer-a.close'));
    blockedProducer.resolve();
  });

  test('an early second signal preserves hold/client ordering before forcing HTTP', async () => {
    const events: string[] = [];
    const graceful = deferred();
    const producer = deferred();
    const forced = deferred();
    const resources = fullResources(events, {
      graceful: graceful.promise,
      producer: () => producer.promise,
      force: () => forced.promise,
    });
    const coordinator = new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(resources), {
      timeoutMs: 1_000,
    });
    const closing = runTestPromise(coordinator.close({ _tag: 'Interruption', signal: 'SIGTERM' }));

    await waitFor(() => coordinator.state === 'stopping-producers', 'producer phase');
    assert.equal(coordinator.force({ _tag: 'SecondSignal', signal: 'SIGTERM' }), true);
    assert.equal(events.filter((event) => event === 'ingress.interrupt').length, 1);
    assert.equal(events.filter((event) => event === 'process-driver.force').length, 1);
    assert.equal(events.filter((event) => event === 'process-driver.close').length, 1);
    assert.equal(
      events.filter((event) => event === 'http.forceStop').length,
      0,
      'force-stop cannot retire transport before held responses are released',
    );
    assert.equal(
      events.filter((event) => event === 'http.forceClients').length,
      0,
      'client force cannot run before the held-response phase',
    );

    producer.resolve();
    await waitFor(() => events.includes('http.forceStop'), 'ordered HTTP force-stop');
    assert.ok(events.indexOf('http.forceStop') > events.indexOf('http.releaseHolds'));
    assert.ok(events.indexOf('http.forceStop') > events.indexOf('http.closeClients'));
    assert.ok(events.indexOf('http.forceClients') > events.indexOf('http.releaseHolds'));
    assert.ok(events.indexOf('http.forceClients') >= events.indexOf('http.closeClients'));
    forced.resolve();
    const outcome = await closing;

    assert.equal(outcome.forced, true);
    assert.equal(outcome.forceSignal?.reason._tag, 'SecondSignal');
    assert.equal(outcome.phases.find(({ phase }) => phase === 'closing-http')?._tag, 'Forced');
    assert.equal(events.filter((event) => event === 'http.forceStop').length, 1);
    assert.equal(events.filter((event) => event === 'http.forceClients').length, 1);
    assert.equal(events.filter((event) => event === 'ingress.interrupt').length, 1);
    assert.equal(events.filter((event) => event === 'process-driver.force').length, 1);
    assert.equal(events.filter((event) => event === 'process-driver.close').length, 1);
    assert.ok(events.indexOf('sqlite.close') > events.indexOf('ingress.close'));
  });

  test('force starts the shared process-driver close synchronously and joins it before store', async () => {
    const events: string[] = [];
    const producer = deferred();
    const processDriverClose = deferred();
    const resources = fullResources(events, {
      producer: () => producer.promise,
      processDriverClose: () => processDriverClose.promise,
    });
    const coordinator = new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(resources), {
      timeoutMs: 1_000,
    });
    const closing = runTestPromise(coordinator.close());

    await waitFor(() => coordinator.state === 'stopping-producers', 'producer phase');
    coordinator.force({ _tag: 'SecondSignal', signal: 'SIGTERM' });
    assert.deepEqual(
      events.filter((event) => event.startsWith('process-driver.')),
      ['process-driver.force', 'process-driver.close'],
      'force publishes immediate KILL and the one joined driver close before returning',
    );

    producer.resolve();
    await waitFor(() => events.includes('ingress.join'), 'runtime join');
    assert.equal(
      events.includes('core.close'),
      false,
      'driver cleanup remains a store-safety join',
    );
    processDriverClose.resolve();
    const outcome = await closing;

    assert.equal(outcome.deadlineExpired, false);
    assert.equal(events.filter((event) => event === 'process-driver.force').length, 1);
    assert.equal(events.filter((event) => event === 'process-driver.close').length, 1);
    assert.ok(events.indexOf('sqlite.close') > events.indexOf('process-driver.close'));
  });

  test('uses the root force reserve to settle stop(true) before coordinator return', async () => {
    const events: string[] = [];
    const graceful = deferred();
    let forceSettled = false;
    const resources = fullResources(events, {
      graceful: graceful.promise,
      force: async () => {
        await Bun.sleep(5);
        forceSettled = true;
        events.push('http.forceStop.settled');
      },
    });
    const coordinator = new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(resources), {
      timeoutMs: 500,
      forceReserveMs: 400,
    });

    const outcome = await runTestPromise(coordinator.close());

    assert.equal(forceSettled, true);
    assert.equal(outcome.forceSignal?.reason._tag, 'DeadlineReserve');
    assert.equal(outcome.deadlineExpired, false);
    assert.equal(events.includes('host-process.close'), false);
    assert.equal(events.at(-1), 'sqlite.close');
  });

  test('hard deadline retains process ownership for the synchronous host-exit fallback', async () => {
    const events: string[] = [];
    const graceful = deferred();
    const forced = deferred();
    const resources = fullResources(events, {
      graceful: graceful.promise,
      force: () => forced.promise,
    });
    const coordinator = new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(resources), {
      timeoutMs: 40,
      forceReserveMs: 20,
    });

    const outcome = await runTestPromise(coordinator.close());
    assert.equal(outcome.deadlineExpired, true);
    assert.equal(events.includes('http.forceStop'), true);
    assert.equal(events.includes('host-process.close'), false);

    forced.resolve();
    await Bun.sleep(0);
    assert.equal(
      events.includes('host-process.close'),
      false,
      'only synchronous host teardown may release HOME ownership',
    );
  });

  test('retains and reports a typed force-stop rejection to the coordinator', async () => {
    const events: string[] = [];
    const graceful = deferred();
    const forceError = new Error('native force stop failed');
    const resources = fullResources(events, {
      graceful: graceful.promise,
      force: () => Promise.reject(forceError),
    });
    const coordinator = new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(resources), {
      timeoutMs: 1_000,
    });
    const closing = runTestPromise(coordinator.close());

    await waitFor(() => coordinator.state === 'closing-http', 'HTTP close phase');
    coordinator.force({ _tag: 'SecondSignal', signal: 'SIGINT' });
    const outcome = await closing;

    const http = outcome.phases.find(({ phase }) => phase === 'closing-http');
    assert.equal(http?._tag, 'Failed');
    if (http?._tag === 'Failed') assert.equal(http.error, forceError);
    assert.equal(outcome.phases.find(({ phase }) => phase === 'closing-store')?._tag, 'Skipped');
    assert.deepEqual(resources.closeErrors, [{ name: 'http.forceStop', error: forceError }]);
    assert.equal(events.includes('sqlite.close'), false);
    assert.equal(events.includes('host-process.close'), false);
  });

  test('a rejected graceful stop immediately forces native HTTP retirement', async () => {
    const events: string[] = [];
    const gracefulError = new Error('native graceful stop failed');
    const resources = fullResources(events, {
      graceful: Promise.reject(gracefulError),
      force: async () => {
        events.push('http.forceStop.settled');
      },
    });
    const coordinator = new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(resources), {
      timeoutMs: 1_000,
      forceReserveMs: 250,
    });

    const outcome = await runTestPromise(coordinator.close());
    const http = outcome.phases.find(({ phase }) => phase === 'closing-http');
    assert.equal(http?._tag, 'Failed');
    if (http?._tag === 'Failed') assert.equal(http.error, gracefulError);
    assert.deepEqual(
      events.filter((event) => event.startsWith('http.forceStop')),
      ['http.forceStop', 'http.forceStop.settled'],
    );
    assert.equal(outcome.phases.find(({ phase }) => phase === 'closing-store')?._tag, 'Skipped');
    assert.equal(events.includes('host-process.close'), false);
  });

  test('retains both graceful and forced HTTP failures without releasing HOME', async () => {
    const events: string[] = [];
    const gracefulError = new Error('native graceful stop failed');
    const forceError = new Error('native forced stop failed');
    const resources = fullResources(events, {
      graceful: Promise.reject(gracefulError),
      force: () => Promise.reject(forceError),
    });
    const coordinator = new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(resources), {
      timeoutMs: 1_000,
    });

    const outcome = await runTestPromise(coordinator.close());
    const http = outcome.phases.find(({ phase }) => phase === 'closing-http');
    assert.equal(http?._tag, 'Failed');
    if (http?._tag === 'Failed') {
      assert.ok(http.error instanceof AggregateError);
      assert.deepEqual(http.error.errors, [gracefulError, forceError]);
    }
    assert.equal(events.filter((event) => event === 'http.forceStop').length, 1);
    assert.equal(events.includes('host-process.close'), false);
  });

  test('a phase failure skips store retirement and retains ownership through host teardown', async () => {
    const events: string[] = [];
    const producerError = new Error('producer join failed');
    const resources = fullResources(events, {
      producer: () => Promise.reject(producerError),
    });
    const coordinator = new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(resources), {
      timeoutMs: 1_000,
    });
    const outcome = await runTestPromise(coordinator.close());

    const producers = outcome.phases.find(({ phase }) => phase === 'stopping-producers');
    assert.equal(producers?._tag, 'Failed');
    if (producers?._tag === 'Failed') assert.equal(producers.error, producerError);
    assert.equal(outcome.phases.find(({ phase }) => phase === 'closing-store')?._tag, 'Skipped');
    assert.equal(events.includes('core.close'), false);
    assert.equal(events.includes('ingress.close'), false);
    assert.equal(events.includes('sqlite.close'), false);
    assert.equal(events.includes('host-process.close'), false);
  });

  test('does not close SQLite until client and ingress users have both joined', async () => {
    const events: string[] = [];
    const clients = deferred();
    const ingress = deferred();
    const resources = fullResources(events, {
      closeClients: () => clients.promise,
      join: () => ingress.promise,
    });
    const coordinator = new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(resources), {
      timeoutMs: 1_000,
    });
    const closing = runTestPromise(coordinator.close());

    await waitFor(() => events.includes('http.closeClients'), 'client close');
    assert.equal(events.includes('core.close'), false);
    assert.equal(events.includes('sqlite.close'), false);
    clients.resolve();
    await waitFor(() => events.includes('ingress.join'), 'ingress join');
    assert.equal(events.includes('core.close'), false);
    assert.equal(events.includes('sqlite.close'), false);
    ingress.resolve();
    await closing;

    assert.ok(events.indexOf('sqlite.close') > events.indexOf('http.closeClients'));
    assert.ok(events.indexOf('sqlite.close') > events.indexOf('ingress.join'));
    assert.ok(events.indexOf('sqlite.close') > events.indexOf('core.close'));
    assert.ok(events.indexOf('sqlite.close') > events.indexOf('ingress.close'));
  });

  test('keeps the P1 close contract for legacy opaque owners', async () => {
    const events: string[] = [];
    const resources = new DaemonResources({
      http: {
        quiesce: () => events.push('http.quiesce'),
        releaseHolds: () => events.push('http.releaseHolds'),
        close: () => events.push('http.close'),
      },
      core: {
        quiesce: () => events.push('core.quiesce'),
        close: () => events.push('core.close'),
      },
      processRuntime: {
        name: 'legacy-runtime',
        owner: {
          quiesce: () => events.push('runtime.quiesce'),
          close: () => events.push('runtime.close'),
        },
      },
      store: { name: 'sqlite', owner: { close: () => events.push('sqlite.close') } },
      process: { name: 'pid', owner: { close: () => events.push('pid.close') } },
    });

    assert.throws(() => makeDaemonResourceLifecycleOwner(resources), /cannot be split safely/);
    const first = resources.close();
    assert.equal(resources.close(), first);
    await first;
    assert.deepEqual(events, [
      'runtime.quiesce',
      'http.quiesce',
      'core.quiesce',
      'http.releaseHolds',
      'http.close',
      'core.close',
      'runtime.close',
      'sqlite.close',
      'pid.close',
    ]);
  });
});
