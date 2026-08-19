import assert from 'node:assert/strict';
import { describe, test } from 'bun:test';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { createBootstrapProcessRuntimeBridge } from '../../src/daemon/app/bootstrap-process-runtime.ts';
import {
  ProcessNonZeroExitError,
  ProcessOutputLimitError,
  ProcessRunnerStartupError,
  ProcessRunnerUnavailableError,
  ProcessSpawnError,
  ProcessTimeoutError,
} from '../../src/daemon/app/errors.ts';
import {
  type BoundedProcessRequest,
  type BoundedProcessResult,
  type ProcessResult,
  type ProcessSuccess,
  ProcessRunner,
  type ProcessRunnerService,
} from '../../src/daemon/app/services/process-runner.ts';

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

describe('BootstrapProcessRuntimeBridge', () => {
  test('an unused bridge stays lazy, shares close identity, and refuses later work', async () => {
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
    const bridge = createBootstrapProcessRuntimeBridge(layer);

    const first = bridge.close();
    const refusedBeforeCloseSettles = bridge.run(request);
    const second = bridge.close();
    assert.equal(first, second);
    assert.deepEqual(await refusedBeforeCloseSettles, cancelled);
    await first;

    assert.equal(acquired, 0);
    assert.equal(released, 0);
    assert.deepEqual(await bridge.run(request), cancelled);
  });

  test('success and typed process/startup failures retain exact compatibility results', async () => {
    const seen: unknown[] = [];
    const success = createBootstrapProcessRuntimeBridge(
      serviceLayer({
        run(actual) {
          seen.push(actual);
          return Effect.succeed({ ok: true, out: actual.argv.join('\u0000') });
        },
      }),
    );
    assert.deepEqual(await success.run(request), { ok: true, out: '/bin/echo\u0000hello' });
    assert.deepEqual(seen, [request]);
    await success.close();

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
      const failed = createBootstrapProcessRuntimeBridge(
        serviceLayer({ run: () => Effect.fail(failure) }),
      );
      assert.deepEqual(await failed.run(request), failure.result);
      await failed.close();
    }

    const startup = createBootstrapProcessRuntimeBridge(
      Layer.effect(
        ProcessRunner,
        Effect.fail(new ProcessRunnerStartupError({ message: 'driver acquisition failed' })),
      ),
    );
    assert.deepEqual(await startup.run(request), {
      ok: false,
      err: 'driver acquisition failed',
    });
    await startup.close();
  });

  test('application interruption maps to canonical cancellation without becoming a tag', async () => {
    const bridge = createBootstrapProcessRuntimeBridge(
      serviceLayer({ run: () => Effect.interrupt }),
    );
    assert.deepEqual(await bridge.run(request), cancelled);
    await bridge.close();
  });

  test('bounded requests use the same runtime and retain their distinct result contract', async () => {
    const expected: BoundedProcessResult = {
      code: 7,
      stdout: Buffer.from('partial'),
      stderr: 'raw stderr\n',
      truncated: true,
      timedOut: false,
    };
    const bridge = createBootstrapProcessRuntimeBridge(
      serviceLayer({
        run: () => Effect.succeed({ ok: true, out: 'unused' }),
        runBounded: () => Effect.succeed(expected),
      }),
    );

    assert.deepEqual(await bridge.runBounded(boundedRequest), expected);
    bridge.quiesce();
    assert.deepEqual(await bridge.runBounded(boundedRequest), boundedCancelled);
    await bridge.close();

    const unavailable = createBootstrapProcessRuntimeBridge(
      serviceLayer({
        run: () => Effect.succeed({ ok: true, out: 'unused' }),
        runBounded: () =>
          Effect.fail(new ProcessRunnerUnavailableError({ message: 'bounded runner unavailable' })),
      }),
    );
    assert.deepEqual(await unavailable.runBounded(boundedRequest), {
      code: null,
      stdout: Buffer.alloc(0),
      stderr: 'bounded runner unavailable',
      truncated: false,
      timedOut: false,
    });
    await unavailable.close();
  });

  test('defects reject with their original identity instead of becoming process failures', async () => {
    const defect = new Error('runner invariant broke');
    const bridge = createBootstrapProcessRuntimeBridge(
      serviceLayer({ run: () => Effect.die(defect) }),
    );

    await assert.rejects(bridge.run(request), (error: unknown) => error === defect);
    await bridge.close();
  });

  test('quiesce interrupts and joins admitted work before Layer release', async () => {
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
    const bridge = createBootstrapProcessRuntimeBridge(layer);

    const running = bridge.run(request);
    await started;
    bridge.quiesce();
    assert.deepEqual(await bridge.run(request), cancelled);

    let closed = false;
    const closing = bridge.close().then(() => {
      closed = true;
    });
    await Bun.sleep(20);
    assert.equal(cleanupStarted, 1);
    assert.equal(closed, false);
    assert.equal(released, 0);

    cleanupResolve();
    assert.deepEqual(await running, cancelled);
    await closing;
    assert.equal(released, 1);
  });
});
