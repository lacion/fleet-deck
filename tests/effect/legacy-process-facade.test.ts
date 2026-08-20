import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'bun:test';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import * as Scope from 'effect/Scope';
import { makeIngressExecFileDelegate } from '../../src/daemon/app/legacy-process-facade.ts';
import {
  ProcessNonZeroExitError,
  ProcessOutputLimitError,
  ProcessRunnerUnavailableError,
  ProcessSpawnError,
  ProcessTimeoutError,
} from '../../src/daemon/app/errors.ts';
import { PROCESS_DRIVER_KILL_GRACE_MS } from '../../src/daemon/app/services/process-driver.ts';
import type { IngressSupervisorService } from '../../src/daemon/app/services/ingress-supervisor.ts';
import {
  type BoundedProcessRequest,
  type BoundedProcessResult,
  type ProcessResult,
  type ProcessSuccess,
  ProcessRunner,
  type ProcessRunnerService,
} from '../../src/daemon/app/services/process-runner.ts';
import { makeIngressSupervisor } from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';
import { ProcessRunnerLive } from '../../src/daemon/platform/bun/process-runner-live.ts';
import { scaleMs, waitUntil } from '../helpers/wait.ts';

const PROCESS_FIXTURE = fileURLToPath(new URL('./fixtures/bun-process-child.ts', import.meta.url));

const request = { argv: ['/bin/echo', 'hello'] as const };
const boundedRequest: BoundedProcessRequest = {
  argv: ['/bin/echo', 'hello'],
  timeoutMs: 1_000,
  maxBytes: 64,
};
const cancelled: ProcessResult = {
  ok: false,
  code: 'ECANCELED',
  err: 'cancelled',
};
const boundedCancelled: BoundedProcessResult = {
  code: null,
  stdout: Buffer.alloc(0),
  stderr: 'cancelled',
  truncated: true,
  timedOut: false,
};

interface SignalMetrics {
  active: number;
  added: number;
  removed: number;
}

function trackedSignal(controller: AbortController): {
  readonly signal: AbortSignal;
  readonly metrics: SignalMetrics;
} {
  const metrics = { active: 0, added: 0, removed: 0 };
  const listeners = new Set<unknown>();
  const source = controller.signal;
  const signal = {
    get aborted() {
      return source.aborted;
    },
    addEventListener(...arguments_: Parameters<AbortSignal['addEventListener']>) {
      metrics.added += 1;
      if (!listeners.has(arguments_[1])) {
        listeners.add(arguments_[1]);
        metrics.active += 1;
      }
      source.addEventListener(...arguments_);
    },
    removeEventListener(...arguments_: Parameters<AbortSignal['removeEventListener']>) {
      metrics.removed += 1;
      if (listeners.delete(arguments_[1])) metrics.active -= 1;
      source.removeEventListener(...arguments_);
    },
  } as AbortSignal;
  return { signal, metrics };
}

function readPid(pidFile: string): number | null {
  try {
    const pid = Number(readFileSync(pidFile, 'utf8'));
    return Number.isSafeInteger(pid) && pid > 1 ? pid : null;
  } catch {
    return null;
  }
}

function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function forceKill(pid: number | null): void {
  if (pid === null) return;
  try {
    process.kill(pid, 'SIGKILL');
  } catch {
    // Already reaped.
  }
}

function completeService(
  service: Pick<ProcessRunnerService, 'run'> & Partial<Pick<ProcessRunnerService, 'runBounded'>>,
): ProcessRunnerService {
  return {
    ...service,
    runBounded:
      service.runBounded ??
      (() =>
        Effect.succeed({
          code: 0,
          stdout: Buffer.alloc(0),
          stderr: '',
          truncated: false,
          timedOut: false,
        })),
  };
}

function serviceLayer(
  service: Pick<ProcessRunnerService, 'run'> & Partial<Pick<ProcessRunnerService, 'runBounded'>>,
): Layer.Layer<ProcessRunner> {
  return Layer.succeed(ProcessRunner, completeService(service));
}

const runTestPromise = Effect.runPromiseWith(Context.empty());

interface FacadeFixture {
  readonly rootScope: Scope.Closeable;
  readonly ingress: IngressSupervisorService<ProcessRunner>;
  readonly delegate: ReturnType<typeof makeIngressExecFileDelegate>;
}

async function makeFixture(
  layer: Layer.Layer<ProcessRunner> = ProcessRunnerLive,
): Promise<FacadeFixture> {
  const rootScope = Scope.makeUnsafe('sequential');
  try {
    const context = await runTestPromise(Layer.buildWithScope(layer, rootScope));
    const ingress = await runTestPromise(makeIngressSupervisor(context, rootScope));
    return {
      rootScope,
      ingress,
      delegate: makeIngressExecFileDelegate(ingress),
    };
  } catch (error) {
    await runTestPromise(Scope.close(rootScope, Exit.void));
    throw error;
  }
}

async function closeIngress(fixture: FacadeFixture): Promise<void> {
  fixture.ingress.quiesce();
  await fixture.ingress.close();
}

async function closeRoot(fixture: FacadeFixture): Promise<void> {
  await runTestPromise(Scope.close(fixture.rootScope, Exit.void));
}

async function closeFixture(fixture: FacadeFixture): Promise<void> {
  try {
    await closeIngress(fixture);
  } finally {
    await closeRoot(fixture);
  }
}

describe('legacy process facade through root IngressSupervisor', () => {
  test('reuses one eager root Context and owns no runtime, Layer, or Scope', async () => {
    let acquired = 0;
    let released = 0;
    const layer = Layer.effect(
      ProcessRunner,
      Effect.acquireRelease(
        Effect.sync(() => {
          acquired++;
          return completeService({
            run: () => Effect.succeed({ ok: true as const, out: 'unexpected' }),
          });
        }),
        () =>
          Effect.sync(() => {
            released++;
          }),
      ),
    );
    const fixture = await makeFixture(layer);
    try {
      assert.equal(acquired, 1, 'the test-owned root Scope eagerly builds ProcessRunner once');

      fixture.ingress.quiesce();
      assert.deepEqual(await fixture.delegate.run(request), cancelled);
      assert.deepEqual(await fixture.delegate.run(request), cancelled);
      assert.equal(acquired, 1, 'the delegate never rebuilds its captured Context');

      const first = fixture.ingress.close();
      assert.equal(first, fixture.ingress.close());
      await first;
      assert.equal(released, 0, 'the facade and ingress do not own the ProcessRunner Layer');
      await closeRoot(fixture);
      assert.equal(released, 1);
    } finally {
      await closeFixture(fixture);
    }
  });

  test('success and typed process failures retain exact compatibility results', async () => {
    const seen: unknown[] = [];
    const success = await makeFixture(
      serviceLayer({
        run(actual) {
          seen.push(actual);
          return Effect.succeed({ ok: true, out: actual.argv.join('\u0000') });
        },
      }),
    );
    try {
      assert.deepEqual(await success.delegate.run(request), {
        ok: true,
        out: '/bin/echo\u0000hello',
      });
      assert.deepEqual(seen, [request]);
    } finally {
      await closeFixture(success);
    }

    const failures = [
      new ProcessSpawnError({
        message: 'missing executable',
        result: { ok: false, code: 'ENOENT', err: 'missing executable' },
      }),
      new ProcessNonZeroExitError({
        message: 'fatal: failed',
        exitCode: 7,
        result: { ok: false, code: 7, err: 'fatal: failed' },
      }),
      new ProcessTimeoutError({
        message: 'timed out after 25ms',
        timeoutMs: 25,
        result: { ok: false, code: 'ETIMEDOUT', err: 'timed out after 25ms' },
      }),
      new ProcessOutputLimitError({
        message: 'subprocess output exceeded 1048576 bytes',
        maxOutputBytes: 1_048_576,
        result: {
          ok: false,
          code: 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER',
          err: 'subprocess output exceeded 1048576 bytes',
        },
      }),
    ] as const;
    for (const failure of failures) {
      const failed = await makeFixture(serviceLayer({ run: () => Effect.fail(failure) }));
      try {
        assert.deepEqual(await failed.delegate.run(request), failure.result);
      } finally {
        await closeFixture(failed);
      }
    }
  });

  test('application interruption maps to canonical cancellation without becoming a tag', async () => {
    const fixture = await makeFixture(serviceLayer({ run: () => Effect.interrupt }));
    try {
      assert.deepEqual(await fixture.delegate.run(request), cancelled);
    } finally {
      await closeFixture(fixture);
    }
  });

  test('a pre-aborted request publishes cancellation immediately and removes its listener', async () => {
    const controller = new AbortController();
    const { signal, metrics } = trackedSignal(controller);
    controller.abort();
    const fixture = await makeFixture();

    try {
      const startedAt = performance.now();
      assert.deepEqual(
        await fixture.delegate.run({
          argv: [process.execPath, '-e', '0'],
          signal,
          timeoutMs: 5_000,
        }),
        cancelled,
      );
      assert.ok(performance.now() - startedAt < scaleMs(100));
      assert.deepEqual(metrics, { active: 0, added: 0, removed: 1 });
    } finally {
      await closeFixture(fixture);
    }
  });

  test('request abort settles publicly before root Scope joins TERM/KILL cleanup', async () => {
    const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-ingress-cancel-'));
    const pidFile = path.join(scratch, 'child.pid');
    const controller = new AbortController();
    const { signal, metrics } = trackedSignal(controller);
    const fixture = await makeFixture();
    let pid: number | null = null;

    try {
      let publicDeliveries = 0;
      const running = fixture.delegate
        .run({
          argv: [process.execPath, PROCESS_FIXTURE, 'term-resistant', pidFile],
          signal,
          timeoutMs: 10_000,
          killTree: true,
        })
        .then(
          (result) => {
            publicDeliveries += 1;
            return result;
          },
          (defect) => {
            publicDeliveries += 1;
            throw defect;
          },
        );
      pid = await waitUntil(() => readPid(pidFile), {
        timeoutMs: 2_000,
        intervalMs: 10,
        label: 'ingress cancellation child pid',
      });

      const abortedAt = performance.now();
      controller.abort();
      assert.deepEqual(await running, cancelled);
      const publicLatencyMs = performance.now() - abortedAt;
      assert.ok(
        publicLatencyMs < scaleMs(100),
        `cancellation took ${String(publicLatencyMs)}ms to publish`,
      );

      await closeIngress(fixture);
      let scopeClosed = false;
      const closeStartedAt = performance.now();
      const closing = closeRoot(fixture).then(() => {
        scopeClosed = true;
      });
      await Bun.sleep(scaleMs(100));
      assert.equal(scopeClosed, false);
      assert.equal(pidAlive(pid), true);

      await closing;
      assert.ok(performance.now() - closeStartedAt >= PROCESS_DRIVER_KILL_GRACE_MS - 350);
      assert.equal(pidAlive(pid), false);
      assert.equal(publicDeliveries, 1);
      assert.deepEqual(metrics, { active: 0, added: 1, removed: 1 });
    } finally {
      await closeFixture(fixture);
      forceKill(pid);
      rmSync(scratch, { recursive: true, force: true });
    }
  });

  test('bounded requests use the same ingress and retain their distinct result contract', async () => {
    const expected: BoundedProcessResult = {
      code: 7,
      stdout: Buffer.from('partial'),
      stderr: 'raw stderr\n',
      truncated: true,
      timedOut: false,
    };
    const fixture = await makeFixture(
      serviceLayer({
        run: () => Effect.succeed({ ok: true, out: 'unused' }),
        runBounded: () => Effect.succeed(expected),
      }),
    );

    try {
      assert.deepEqual(await fixture.delegate.runBounded(boundedRequest), expected);
      fixture.ingress.quiesce();
      assert.deepEqual(await fixture.delegate.runBounded(boundedRequest), boundedCancelled);
    } finally {
      await closeFixture(fixture);
    }

    const unavailable = await makeFixture(
      serviceLayer({
        run: () => Effect.succeed({ ok: true, out: 'unused' }),
        runBounded: () =>
          Effect.fail(new ProcessRunnerUnavailableError({ message: 'bounded runner unavailable' })),
      }),
    );
    try {
      assert.deepEqual(await unavailable.delegate.runBounded(boundedRequest), {
        code: null,
        stdout: Buffer.alloc(0),
        stderr: 'bounded runner unavailable',
        truncated: false,
        timedOut: false,
      });
    } finally {
      await closeFixture(unavailable);
    }
  });

  test('defects reject with their original identity instead of becoming process failures', async () => {
    const defect = new Error('runner invariant broke');
    const controller = new AbortController();
    const { signal, metrics } = trackedSignal(controller);
    const fixture = await makeFixture(serviceLayer({ run: () => Effect.die(defect) }));

    try {
      await assert.rejects(
        fixture.delegate.run({ ...request, signal }),
        (error: unknown) => error === defect,
      );
      assert.equal(metrics.active, 0);
    } finally {
      await closeFixture(fixture);
    }
  });

  test('quiesce refuses and close interrupts admitted work before Layer release', async () => {
    let startedResolve: () => void = () => undefined;
    const started = new Promise<void>((resolve) => {
      startedResolve = resolve;
    });
    let cleanupResolve: () => void = () => undefined;
    const cleanupGate = new Promise<void>((resolve) => {
      cleanupResolve = resolve;
    });
    let cleanupStarted = 0;
    let released = 0;
    const service: ProcessRunnerService = completeService({
      run: () =>
        Effect.callback<ProcessSuccess>(() => {
          startedResolve();
          return Effect.promise(async () => {
            cleanupStarted++;
            await cleanupGate;
          });
        }),
    });
    const layer = Layer.effect(
      ProcessRunner,
      Effect.acquireRelease(Effect.succeed(service), () =>
        Effect.sync(() => {
          released++;
        }),
      ),
    );
    const fixture = await makeFixture(layer);

    try {
      const running = fixture.delegate.run(request);
      await started;
      fixture.ingress.quiesce();
      assert.deepEqual(await fixture.delegate.run(request), cancelled);

      let closed = false;
      const closing = fixture.ingress.close().then(() => {
        closed = true;
      });
      await Bun.sleep(20);
      assert.equal(cleanupStarted, 1);
      assert.equal(closed, false);
      assert.equal(released, 0);

      cleanupResolve();
      assert.deepEqual(await running, cancelled);
      await closing;
      assert.equal(released, 0);
      await closeRoot(fixture);
      assert.equal(released, 1);
    } finally {
      cleanupResolve();
      await closeFixture(fixture);
    }
  });
});
