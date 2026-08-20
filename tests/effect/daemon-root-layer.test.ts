import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import * as Scope from 'effect/Scope';
import { DaemonResources } from '../../src/daemon/daemon-resources.ts';
import { DaemonStartupError } from '../../src/daemon/app/errors.ts';
import {
  composeDaemonRootLayer,
  type DaemonAcquisitionShutdownFailure,
  makeDaemonLifecycleCoordinator,
  makeDaemonLifecycleLayer,
} from '../../src/daemon/app/live-layer.ts';
import { DaemonLifecycle } from '../../src/daemon/app/services/daemon-lifecycle.ts';
import { AppConfig } from '../../src/daemon/app/services/app-config.ts';
import {
  IngressSupervisor,
  type RootIngressSupervisorService,
} from '../../src/daemon/app/services/ingress-supervisor.ts';
import {
  ProcessRunner,
  type ProcessRunnerService,
} from '../../src/daemon/app/services/process-runner.ts';
import {
  ProcessRuntimeControl,
  type ProcessRuntimeControlService,
} from '../../src/daemon/app/services/process-runtime-control.ts';
import type { AcquiredDaemonResources } from '../../src/daemon/app/program.ts';
import { runEffectExit } from './helpers.ts';
import { scaleMs } from '../helpers/wait.ts';

const runTestPromise = Effect.runPromiseWith(Context.empty());

const IMPORT_FIXTURE = fileURLToPath(
  new URL('./fixtures/daemon-root-layer-import.ts', import.meta.url),
);

const processRunnerService: ProcessRunnerService = {
  run: () => Effect.succeed({ ok: true, out: '' }),
  runBounded: () =>
    Effect.succeed({
      code: 0,
      stdout: Buffer.alloc(0),
      stderr: '',
      truncated: false,
      timedOut: false,
    }),
};

const processRuntimeControl: ProcessRuntimeControlService = {
  force: () => undefined,
  close: async () => undefined,
};

const acquisitionShutdownOptions = {
  acquisitionShutdownTimeoutMs: 1_000,
  acquisitionShutdownReserveMs: 0,
  onAcquisitionShutdownFailure: () => undefined,
} as const;

function acquired(resources: DaemonResources): AcquiredDaemonResources {
  return {
    resources,
    readiness: Promise.resolve(),
    shutdownExitCode: () => 0,
    releaseProcessAtHostExit: () => undefined,
  };
}

function makeApplicationLayer(
  events: string[],
  control: ProcessRuntimeControlService = processRuntimeControl,
): Layer.Layer<AppConfig | ProcessRunner | ProcessRuntimeControl> {
  const configLayer = Layer.succeed(AppConfig, {
    home: '/unused',
    port: 4922,
    version: 'p4.3-test',
  });
  const processLayer = Layer.effect(
    ProcessRunner,
    Effect.gen(function* () {
      yield* AppConfig;
      return yield* Effect.acquireRelease(
        Effect.sync(() => {
          events.push('process:acquire');
          return processRunnerService;
        }),
        () => Effect.sync(() => events.push('process:release')),
      );
    }),
  );
  const processAndControl = Layer.merge(
    processLayer,
    Layer.succeed(ProcessRuntimeControl, control),
  );
  return processAndControl.pipe(Layer.provideMerge(configLayer));
}

function makeRootFixture(events: string[]) {
  let capturedIngress: RootIngressSupervisorService | null = null;
  const resources = new DaemonResources({
    process: {
      name: 'pidfile',
      owner: {
        close: () => events.push(`daemon:release:${capturedIngress?.state ?? 'missing'}`),
      },
    },
  });
  const daemonLayer = makeDaemonLifecycleLayer({
    ...acquisitionShutdownOptions,
    acquireDaemonResources: async (_signal, ingress) => {
      capturedIngress = ingress;
      events.push(`ingress:ready:${ingress.state}`);
      events.push('daemon:acquire');
      return acquired(resources);
    },
    makeLifecycleCoordinator: (value) =>
      makeDaemonLifecycleCoordinator(value, { timeoutMs: 1_000 }),
  });
  return {
    resources,
    layer: composeDaemonRootLayer(makeApplicationLayer(events), daemonLayer),
    ingress: () => capturedIngress,
  };
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 2_000): Promise<T> {
  const effectiveTimeoutMs = scaleMs(timeoutMs);
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out after ${effectiveTimeoutMs}ms`)),
          effectiveTimeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

describe('P4.3 aggregate daemon root Layer', () => {
  test('construction is cold and includes every root service type', async () => {
    const events: string[] = [];
    const { layer, ingress: acquiredIngress } = makeRootFixture(events);
    assert.deepEqual(events, []);

    let capturedIngressState = '';
    const program = Effect.gen(function* () {
      const config = yield* AppConfig;
      yield* ProcessRunner;
      const lifecycle = yield* DaemonLifecycle;
      const ingress = yield* IngressSupervisor;
      capturedIngressState = ingress.state;
      return {
        version: config.version,
        sameResources: lifecycle.acquired.resources instanceof DaemonResources,
      };
    });
    const exit = await runEffectExit(Effect.provide(program, layer));

    assert.ok(Exit.isSuccess(exit));
    assert.deepEqual(exit.value, { version: 'p4.3-test', sameResources: true });
    assert.equal(capturedIngressState, 'open');
    assert.deepEqual(events, [
      'process:acquire',
      'ingress:ready:open',
      'daemon:acquire',
      'process:release',
    ]);
    assert.equal(acquiredIngress()?.state, 'quiescing');
  });

  test('partial root acquisition maps a typed failure and releases the acquired prefix', async () => {
    const events: string[] = [];
    const startupCause = new Error('bind fixture failed');
    const daemonLayer = makeDaemonLifecycleLayer({
      ...acquisitionShutdownOptions,
      acquireDaemonResources: async () => {
        events.push('daemon:acquire');
        throw startupCause;
      },
      makeLifecycleCoordinator: (value) =>
        makeDaemonLifecycleCoordinator(value, { timeoutMs: 1_000 }),
    });
    const layer = composeDaemonRootLayer(makeApplicationLayer(events), daemonLayer);

    const exit = await runEffectExit(Effect.provide(Effect.void, layer));

    assert.ok(Exit.isFailure(exit));
    assert.equal(Exit.hasFails(exit), true);
    assert.equal(Exit.hasDies(exit), false);
    const failure = exit.cause.reasons.find(Cause.isFailReason)?.error;
    assert.ok(failure instanceof DaemonStartupError);
    assert.equal(failure.cause, startupCause);
    assert.deepEqual(events, ['process:acquire', 'daemon:acquire', 'process:release']);
  });

  test('acquisition shutdown reserve rejects negative and over-budget values before acquisition', async () => {
    for (const reserveMs of [-1, 1_001]) {
      const events: string[] = [];
      let acquisitionCalls = 0;
      const daemonLayer = makeDaemonLifecycleLayer({
        acquisitionShutdownTimeoutMs: 1_000,
        acquisitionShutdownReserveMs: reserveMs,
        onAcquisitionShutdownFailure: () => undefined,
        acquireDaemonResources: async () => {
          acquisitionCalls++;
          return acquired(new DaemonResources());
        },
        makeLifecycleCoordinator: (value) =>
          makeDaemonLifecycleCoordinator(value, { timeoutMs: 1_000 }),
      });
      const layer = composeDaemonRootLayer(makeApplicationLayer(events), daemonLayer);

      const exit = await runEffectExit(Effect.provide(Effect.void, layer));

      assert.ok(Exit.isFailure(exit));
      const failure = exit.cause.reasons.find(Cause.isFailReason)?.error;
      assert.ok(failure instanceof DaemonStartupError);
      assert.ok(failure.cause instanceof RangeError);
      assert.equal(acquisitionCalls, 0);
      assert.deepEqual(events, ['process:acquire', 'process:release']);
    }
  });

  test('root interruption aborts and joins asynchronous acquisition cleanup before Scope exit', async () => {
    const events: string[] = [];
    let announceStarted: () => void = () => undefined;
    const started = new Promise<void>((resolve) => {
      announceStarted = resolve;
    });
    let announceAborted: () => void = () => undefined;
    const aborted = new Promise<void>((resolve) => {
      announceAborted = resolve;
    });
    let releaseCleanup: () => void = () => undefined;
    const cleanupGate = new Promise<void>((resolve) => {
      releaseCleanup = resolve;
    });
    const daemonLayer = makeDaemonLifecycleLayer({
      ...acquisitionShutdownOptions,
      acquireDaemonResources: (signal) =>
        new Promise<AcquiredDaemonResources>((_resolve, reject) => {
          events.push('daemon:acquire');
          announceStarted();
          signal.addEventListener(
            'abort',
            () => {
              events.push('daemon:abort');
              announceAborted();
              void cleanupGate.then(() => {
                events.push('daemon:cleanup');
                reject(signal.reason);
              });
            },
            { once: true },
          );
        }),
      makeLifecycleCoordinator: (value) =>
        makeDaemonLifecycleCoordinator(value, { timeoutMs: 1_000 }),
    });
    const layer = composeDaemonRootLayer(makeApplicationLayer(events), daemonLayer);
    const controller = new AbortController();
    const running = runTestPromise(Effect.exit(Effect.provide(Effect.never, layer)), {
      signal: controller.signal,
    });
    const observedRunning = running.then(
      (value) => ({ _tag: 'Success' as const, value }),
      (error: unknown) => ({ _tag: 'Failure' as const, error }),
    );

    await within(started, 'acquisition start');
    controller.abort();
    await within(aborted, 'acquisition abort');
    let rootSettled = false;
    void observedRunning.then(() => {
      rootSettled = true;
    });
    await Bun.sleep(10);
    assert.equal(rootSettled, false, 'root waits for the aborted acquisition cleanup Promise');

    releaseCleanup();
    const result = await within(observedRunning, 'root acquisition cleanup join');
    assert.equal(result._tag, 'Failure');
    assert.deepEqual(events, [
      'process:acquire',
      'daemon:acquire',
      'daemon:abort',
      'daemon:cleanup',
      'process:release',
    ]);
  });

  test('an acquisition that fulfills after interruption is retired before app fallbacks', async () => {
    const events: string[] = [];
    let announceStarted: () => void = () => undefined;
    const started = new Promise<void>((resolve) => {
      announceStarted = resolve;
    });
    let resolveLate: (value: AcquiredDaemonResources) => void = () => undefined;
    const lateAcquisition = new Promise<AcquiredDaemonResources>((resolve) => {
      resolveLate = resolve;
    });
    const resources = new DaemonResources({
      process: {
        name: 'pidfile',
        owner: { close: () => events.push('daemon:release') },
      },
    });
    const daemonLayer = makeDaemonLifecycleLayer({
      ...acquisitionShutdownOptions,
      acquireDaemonResources: async () => {
        events.push('daemon:acquire');
        announceStarted();
        return lateAcquisition;
      },
      makeLifecycleCoordinator: (value) =>
        makeDaemonLifecycleCoordinator(value, { timeoutMs: 1_000 }),
    });
    const layer = composeDaemonRootLayer(makeApplicationLayer(events), daemonLayer);
    const controller = new AbortController();
    const running = runTestPromise(Effect.exit(Effect.provide(Effect.never, layer)), {
      signal: controller.signal,
    }).then(
      (value) => ({ _tag: 'Success' as const, value }),
      (error: unknown) => ({ _tag: 'Failure' as const, error }),
    );

    await within(started, 'late acquisition start');
    controller.abort();
    resolveLate({
      resources,
      readiness: Promise.resolve(),
      shutdownExitCode: () => 0,
      releaseProcessAtHostExit: () => events.push('host-exit-fallback'),
    });

    const result = await within(running, 'late acquisition retirement');
    assert.equal(result._tag, 'Failure');
    assert.deepEqual(events, [
      'process:acquire',
      'daemon:acquire',
      'daemon:release',
      'host-exit-fallback',
      'process:release',
    ]);
  });

  test('acquisition cancellation forces process control and stops waiting at one absolute deadline', async () => {
    const events: string[] = [];
    const failures: DaemonAcquisitionShutdownFailure[] = [];
    let announceStarted: () => void = () => undefined;
    const started = new Promise<void>((resolve) => {
      announceStarted = resolve;
    });
    let rejectAcquisition: (cause: unknown) => void = () => undefined;
    const control: ProcessRuntimeControlService = {
      force: () => events.push('process-control:force'),
      close: async () => {
        events.push('process-control:close');
      },
    };
    const daemonLayer = makeDaemonLifecycleLayer({
      acquisitionShutdownTimeoutMs: scaleMs(30),
      acquisitionShutdownReserveMs: 0,
      onAcquisitionShutdownFailure: (failure) => failures.push(failure),
      acquireDaemonResources: (_signal) => {
        announceStarted();
        return new Promise<AcquiredDaemonResources>((_resolve, reject) => {
          rejectAcquisition = reject;
        });
      },
      makeLifecycleCoordinator: (value) =>
        makeDaemonLifecycleCoordinator(value, { timeoutMs: 1_000 }),
    });
    const layer = composeDaemonRootLayer(makeApplicationLayer(events, control), daemonLayer);
    const controller = new AbortController();
    const running = runTestPromise(Effect.exit(Effect.provide(Effect.never, layer)), {
      signal: controller.signal,
    }).then(
      (value) => ({ _tag: 'Success' as const, value }),
      (error: unknown) => ({ _tag: 'Failure' as const, error }),
    );

    await within(started, 'bounded acquisition start');
    const interruptedAt = performance.now();
    controller.abort();
    const result = await within(running, 'bounded acquisition cancellation', 500);
    const elapsedMs = performance.now() - interruptedAt;

    assert.equal(result._tag, 'Failure');
    assert.ok(
      elapsedMs < scaleMs(400),
      `acquisition cancellation exceeded its injected deadline (${elapsedMs.toFixed(1)}ms)`,
    );
    assert.deepEqual(events.slice(0, 3), [
      'process:acquire',
      'process-control:force',
      'process-control:close',
    ]);
    assert.deepEqual(
      failures.map((failure) => failure._tag),
      ['TimedOut'],
    );

    rejectAcquisition(controller.signal.reason);
    await Bun.sleep(0);
  });

  test('a successful acquisition arriving after the deadline is still retired exactly once', async () => {
    const events: string[] = [];
    const failures: DaemonAcquisitionShutdownFailure[] = [];
    let announceStarted: () => void = () => undefined;
    const started = new Promise<void>((resolve) => {
      announceStarted = resolve;
    });
    let resolveLate: (value: AcquiredDaemonResources) => void = () => undefined;
    const lateAcquisition = new Promise<AcquiredDaemonResources>((resolve) => {
      resolveLate = resolve;
    });
    let processClosePromise: Promise<void> | null = null;
    const control: ProcessRuntimeControlService = {
      force: () => events.push('process-control:force'),
      close: () => {
        events.push('process-control:close');
        processClosePromise ??= Promise.resolve();
        return processClosePromise;
      },
    };
    const resources = new DaemonResources({
      process: {
        name: 'pidfile',
        owner: { close: () => events.push('daemon:release') },
      },
    });
    const daemonLayer = makeDaemonLifecycleLayer({
      acquisitionShutdownTimeoutMs: scaleMs(30),
      acquisitionShutdownReserveMs: 0,
      onAcquisitionShutdownFailure: (failure) => failures.push(failure),
      acquireDaemonResources: async () => {
        announceStarted();
        return lateAcquisition;
      },
      makeLifecycleCoordinator: (value) =>
        makeDaemonLifecycleCoordinator(value, { timeoutMs: 1_000 }),
    });
    const layer = composeDaemonRootLayer(makeApplicationLayer(events, control), daemonLayer);
    const controller = new AbortController();
    const running = runTestPromise(Effect.exit(Effect.provide(Effect.never, layer)), {
      signal: controller.signal,
    }).then(
      (value) => ({ _tag: 'Success' as const, value }),
      (error: unknown) => ({ _tag: 'Failure' as const, error }),
    );

    await within(started, 'late acquisition start');
    controller.abort();
    const result = await within(running, 'late acquisition deadline', 500);
    assert.equal(result._tag, 'Failure');
    assert.deepEqual(
      failures.map((failure) => failure._tag),
      ['TimedOut'],
    );

    resolveLate({
      resources,
      readiness: Promise.resolve(),
      shutdownExitCode: () => 0,
      releaseProcessAtHostExit: () => events.push('host-exit-fallback'),
    });
    await within(
      (async () => {
        while (!events.includes('host-exit-fallback')) await Bun.sleep(1);
      })(),
      'late acquisition continuation',
      500,
    );
    await resources.close();

    assert.equal(events.filter((event) => event === 'daemon:release').length, 1);
    assert.equal(events.filter((event) => event === 'host-exit-fallback').length, 1);
    assert.equal(events.filter((event) => event === 'process-control:force').length, 1);
  });

  test('acquisition cleanup and process-control failures upgrade host policy without extending cancellation', async () => {
    const events: string[] = [];
    const failures: DaemonAcquisitionShutdownFailure[] = [];
    const cleanupFailure = new Error('prefix cleanup failed');
    const forceFailure = new Error('process force failed');
    const closeFailure = new Error('process close failed');
    let announceStarted: () => void = () => undefined;
    const started = new Promise<void>((resolve) => {
      announceStarted = resolve;
    });
    const control: ProcessRuntimeControlService = {
      force: () => {
        events.push('process-control:force');
        throw forceFailure;
      },
      close: () => {
        events.push('process-control:close');
        return Promise.reject(closeFailure);
      },
    };
    const daemonLayer = makeDaemonLifecycleLayer({
      acquisitionShutdownTimeoutMs: scaleMs(500),
      acquisitionShutdownReserveMs: 0,
      onAcquisitionShutdownFailure: (failure) => failures.push(failure),
      acquireDaemonResources: (signal) =>
        new Promise<AcquiredDaemonResources>((_resolve, reject) => {
          announceStarted();
          signal.addEventListener('abort', () => reject(cleanupFailure), { once: true });
        }),
      makeLifecycleCoordinator: (value) =>
        makeDaemonLifecycleCoordinator(value, { timeoutMs: 1_000 }),
    });
    const layer = composeDaemonRootLayer(makeApplicationLayer(events, control), daemonLayer);
    const controller = new AbortController();
    const running = runTestPromise(Effect.exit(Effect.provide(Effect.never, layer)), {
      signal: controller.signal,
    }).then(
      (value) => ({ _tag: 'Success' as const, value }),
      (error: unknown) => ({ _tag: 'Failure' as const, error }),
    );

    await within(started, 'failed acquisition cleanup start');
    controller.abort();
    const result = await within(running, 'failed acquisition cleanup', 500);

    assert.equal(result._tag, 'Failure');
    assert.deepEqual(events.slice(0, 3), [
      'process:acquire',
      'process-control:force',
      'process-control:close',
    ]);
    assert.deepEqual(
      failures.map((failure) => failure._tag).sort(),
      ['AcquisitionCleanupFailed', 'ProcessCloseFailed', 'ProcessForceFailed'].sort(),
    );
    assert.equal(
      failures.some((failure) => failure._tag === 'TimedOut'),
      false,
    );
  });

  test('coordinator assembly failure retires the acquired daemon before app fallbacks', async () => {
    const events: string[] = [];
    const assemblyCause = new Error('coordinator fixture failed');
    const resources = new DaemonResources({
      process: {
        name: 'pidfile',
        owner: { close: () => events.push('daemon:release') },
      },
    });
    const daemonLayer = makeDaemonLifecycleLayer({
      ...acquisitionShutdownOptions,
      acquireDaemonResources: async () => {
        events.push('daemon:acquire');
        return acquired(resources);
      },
      makeLifecycleCoordinator: () => {
        throw assemblyCause;
      },
    });
    const layer = composeDaemonRootLayer(makeApplicationLayer(events), daemonLayer);

    const exit = await runEffectExit(Effect.provide(Effect.void, layer));

    assert.ok(Exit.isFailure(exit));
    const failure = exit.cause.reasons.find(Cause.isFailReason)?.error;
    assert.ok(failure instanceof DaemonStartupError);
    assert.equal(failure.cause, assemblyCause);
    assert.deepEqual(events, [
      'process:acquire',
      'daemon:acquire',
      'daemon:release',
      'process:release',
    ]);
  });

  test('Scope fallback joins an in-flight policy close before ingress/process finalizers', async () => {
    const events: string[] = [];
    let capturedIngress: RootIngressSupervisorService | null = null;
    let announceCloseStarted: () => void = () => undefined;
    const closeStarted = new Promise<void>((resolve) => {
      announceCloseStarted = resolve;
    });
    let releaseClose: () => void = () => undefined;
    const closeGate = new Promise<void>((resolve) => {
      releaseClose = resolve;
    });
    const resources = new DaemonResources({
      core: {
        quiesce() {},
        close: async () => {
          events.push(`daemon:core:start:${capturedIngress?.state ?? 'missing'}`);
          announceCloseStarted();
          await closeGate;
          events.push(`daemon:core:end:${capturedIngress?.state ?? 'missing'}`);
        },
      },
      process: {
        name: 'pidfile',
        owner: {
          close: () => events.push(`daemon:release:${capturedIngress?.state ?? 'missing'}`),
        },
      },
    });
    const daemonLayer = makeDaemonLifecycleLayer({
      ...acquisitionShutdownOptions,
      acquireDaemonResources: async (_signal, ingress) => {
        capturedIngress = ingress;
        events.push(`ingress:ready:${ingress.state}`);
        return acquired(resources);
      },
      makeLifecycleCoordinator: (value) =>
        makeDaemonLifecycleCoordinator(value, { timeoutMs: 1_000 }),
    });
    const layer = composeDaemonRootLayer(makeApplicationLayer(events), daemonLayer);
    const rootScope = Scope.makeUnsafe('sequential');
    const context = await runTestPromise(Layer.buildWithScope(layer, rootScope));
    const lifecycle = Context.get(context, DaemonLifecycle);
    const ingress = Context.get(context, IngressSupervisor);

    const explicitClosing = runTestPromise(
      lifecycle.coordinator.close({ _tag: 'Requested', reason: 'fixture' }),
    );
    await within(closeStarted, 'explicit policy close start');
    const fallbackClosing = runTestPromise(Scope.close(rootScope, Exit.void));

    try {
      await Bun.sleep(0);
      assert.equal(ingress.state, 'open', 'Scope finalization waits behind daemon policy close');
      assert.equal(events.filter((event) => event.includes('daemon:core:start')).length, 1);
      assert.equal(events.includes('process:release'), false);
    } finally {
      releaseClose();
    }
    await within(
      Promise.all([explicitClosing, fallbackClosing]),
      'shared policy and fallback close',
    );

    assert.equal(resources.closeErrors.length, 0);
    assert.deepEqual(events, [
      'process:acquire',
      'ingress:ready:open',
      'daemon:core:start:open',
      'daemon:core:end:open',
      'process:release',
    ]);
    assert.equal(
      events.some((event) => event.startsWith('daemon:release:')),
      false,
    );
    assert.equal(
      ingress.state,
      'quiescing',
      'ingress fallback runs only after the daemon coordinator has closed',
    );
  });

  test('the same root Layer definition rebuilds fresh scoped owners', async () => {
    const events: string[] = [];
    let daemonAcquisitions = 0;
    const applicationLayer = makeApplicationLayer(events);
    const daemonLayer = makeDaemonLifecycleLayer({
      ...acquisitionShutdownOptions,
      acquireDaemonResources: async () => {
        daemonAcquisitions++;
        events.push(`daemon:acquire:${daemonAcquisitions}`);
        return acquired(
          new DaemonResources({
            process: {
              name: `pidfile-${daemonAcquisitions}`,
              owner: { close: () => events.push(`daemon:release:${daemonAcquisitions}`) },
            },
          }),
        );
      },
      makeLifecycleCoordinator: (value) =>
        makeDaemonLifecycleCoordinator(value, { timeoutMs: 1_000 }),
    });
    const layer = composeDaemonRootLayer(applicationLayer, daemonLayer);

    const first = await runEffectExit(Effect.provide(Effect.as(DaemonLifecycle, 'first'), layer));
    const second = await runEffectExit(Effect.provide(Effect.as(DaemonLifecycle, 'second'), layer));

    assert.ok(Exit.isSuccess(first));
    assert.ok(Exit.isSuccess(second));
    assert.equal(daemonAcquisitions, 2);
    assert.equal(events.filter((event) => event === 'process:acquire').length, 2);
    assert.equal(events.filter((event) => event === 'process:release').length, 2);
    assert.equal(events.filter((event) => event.startsWith('daemon:release:')).length, 0);
  });

  test('importing root definitions acquires no runtime and exits naturally', async () => {
    const child = Bun.spawn({
      cmd: [process.execPath, IMPORT_FIXTURE],
      cwd: process.cwd(),
      env: { ...process.env },
      stdin: 'ignore',
      stdout: 'pipe',
      stderr: 'pipe',
    });

    const [exitCode, stdout, stderr] = await within(
      Promise.all([
        child.exited,
        new Response(child.stdout).text(),
        new Response(child.stderr).text(),
      ]),
      'definition-only import',
    );

    assert.equal(exitCode, 0, stderr);
    assert.equal(stdout, 'definition-only\n');
    assert.equal(stderr, '');
  });
});
