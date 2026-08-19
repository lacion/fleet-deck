import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Console from 'effect/Console';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Fiber from 'effect/Fiber';
import {
  ApplicationQuiescingError,
  ProcessNonZeroExitError,
  ProcessOutputLimitError,
  ProcessRunnerStartupError,
  ProcessRunnerUnavailableError,
  ProcessSpawnError,
  ProcessTimeoutError,
  StartupConfigurationError,
} from '../../src/daemon/app/errors.ts';
import { kernelProbe } from '../../src/daemon/app/kernel.ts';
import { makeLiveLayer } from '../../src/daemon/app/live-layer.ts';
import { AppConfig } from '../../src/daemon/app/services/app-config.ts';
import type { ProcessRunnerService } from '../../src/daemon/app/services/process-runner.ts';
import { fakeKernelLayer, makeFakeProcessRunner } from './fake-layers.ts';
import { runEffectExit, TestClock, TestConsole, TestServicesLayer } from './helpers.ts';
import { scaleMs } from '../helpers/wait.ts';

const LIFECYCLE_FIXTURE = fileURLToPath(new URL('./fixtures/kernel-lifecycle.ts', import.meta.url));

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
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

async function forceRetire(child: Bun.Subprocess, label: string): Promise<void> {
  if (child.exitCode === null) child.kill('SIGKILL');
  try {
    await within(child.exited, `${label} forced reap`);
  } catch (error) {
    child.unref();
    throw error;
  }
}

describe('Effect application kernel', () => {
  test('tagged startup, application, and process families remain narrow Error values', () => {
    const configuration = new StartupConfigurationError({
      setting: 'port',
      message: 'must be a positive integer',
    });
    const startup = new ProcessRunnerStartupError({ message: 'driver construction failed' });
    const unavailable = new ProcessRunnerUnavailableError({ message: 'runner is closed' });
    const quiescing = new ApplicationQuiescingError({
      operation: 'spawn',
      message: 'application is quiescing',
    });
    const spawnResult = { ok: false as const, code: 'ENOENT', err: 'missing executable' };
    const spawn = new ProcessSpawnError({
      message: spawnResult.err,
      result: spawnResult,
    });
    const nonZeroResult = { ok: false as const, code: 7, err: 'process exited 7' };
    const nonZero = new ProcessNonZeroExitError({
      message: nonZeroResult.err,
      exitCode: 7,
      result: nonZeroResult,
    });
    const timeoutResult = { ok: false as const, code: 'ETIMEDOUT', err: 'timed out after 25ms' };
    const timeout = new ProcessTimeoutError({
      message: timeoutResult.err,
      timeoutMs: 25,
      result: timeoutResult,
    });
    const outputResult = {
      ok: false as const,
      code: 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER',
      err: 'subprocess output exceeded 1048576 bytes',
    };
    const outputLimit = new ProcessOutputLimitError({
      message: outputResult.err,
      maxOutputBytes: 1_048_576,
      result: outputResult,
    });

    assert.ok(configuration instanceof Error);
    assert.equal(configuration._tag, 'StartupConfigurationError');
    assert.equal(startup._tag, 'ProcessRunnerStartupError');
    assert.equal(unavailable._tag, 'ProcessRunnerUnavailableError');
    assert.equal(quiescing._tag, 'ApplicationQuiescingError');
    assert.equal(spawn._tag, 'ProcessSpawnError');
    assert.equal(nonZero._tag, 'ProcessNonZeroExitError');
    assert.equal(timeout._tag, 'ProcessTimeoutError');
    assert.equal(outputLimit._tag, 'ProcessOutputLimitError');
    assert.equal(spawn.result, spawnResult);
  });

  test('Live Layer composes successful config with effectful service acquisition', async () => {
    let acquisitions = 0;
    const acquireProcessRunner = Effect.gen(function* () {
      const config = yield* AppConfig;
      yield* Effect.sync(() => {
        acquisitions += 1;
      });
      return {
        run(request) {
          return Effect.succeed({
            ok: true as const,
            out: `${config.version}:${request.argv.join('|')}`,
          });
        },
        runBounded: () =>
          Effect.succeed({
            code: 0,
            stdout: Buffer.alloc(0),
            stderr: '',
            truncated: false,
            timedOut: false,
          }),
      } satisfies ProcessRunnerService;
    });
    const layer = makeLiveLayer({
      config: { home: '/unused', port: 4922, version: '1.2.3-test' },
      acquireProcessRunner,
    });

    const exit = await runEffectExit(
      Effect.provide(kernelProbe({ argv: ['fleetdeck-probe', '--version'] }), layer),
    );

    assert.ok(Exit.isSuccess(exit));
    assert.deepEqual(exit.value, {
      version: '1.2.3-test',
      port: 4922,
      process: { ok: true, out: '1.2.3-test:fleetdeck-probe|--version' },
    });
    assert.equal(acquisitions, 1);
  });

  test('Live Layer preserves a typed startup acquisition failure', async () => {
    const startupError = new ProcessRunnerStartupError({ message: 'probe driver unavailable' });
    const layer = makeLiveLayer({
      config: { home: '/unused', port: 4711, version: 'test' },
      acquireProcessRunner: Effect.fail(startupError),
    });
    const exit = await runEffectExit(
      Effect.provide(kernelProbe({ argv: ['fleetdeck-probe'] }), layer),
    );

    assert.ok(Exit.isFailure(exit));
    assert.equal(Exit.hasFails(exit), true);
    assert.equal(Exit.hasDies(exit), false);
    assert.equal(Cause.squash(exit.cause), startupError);
  });

  test('fake Layers are reusable and retain argv requests', async () => {
    const processRunner = makeFakeProcessRunner();
    const { layer } = fakeKernelLayer({
      config: { port: 5001, version: 'fake-version' },
      processRunner,
    });
    const request = { argv: ['bun', '--revision'] as const };

    const first = await runEffectExit(Effect.provide(kernelProbe(request), layer));
    const second = await runEffectExit(Effect.provide(kernelProbe(request), layer));

    assert.ok(Exit.isSuccess(first));
    assert.ok(Exit.isSuccess(second));
    assert.equal(first.value.process.ok, true);
    assert.equal(processRunner.requests.length, 2);
    assert.deepEqual(
      processRunner.requests.map(({ argv }) => argv),
      [request.argv, request.argv],
    );
  });

  test('fake Layers preserve typed process failures, interruption, and defects', async () => {
    const result = { ok: false as const, code: 'ETIMEDOUT', err: 'timed out after 10ms' };
    const typedError = new ProcessTimeoutError({
      message: result.err,
      timeoutMs: 10,
      result,
    });
    const typed = makeFakeProcessRunner({ execute: () => Effect.fail(typedError) });
    const typedLayer = fakeKernelLayer({ processRunner: typed }).layer;
    const typedExit = await runEffectExit(
      Effect.provide(kernelProbe({ argv: ['typed-failure'] }), typedLayer),
    );
    assert.ok(Exit.isFailure(typedExit));
    assert.equal(Exit.hasFails(typedExit), true);
    assert.equal(Cause.squash(typedExit.cause), typedError);

    const interrupted = makeFakeProcessRunner({ execute: () => Effect.interrupt });
    const interruptedLayer = fakeKernelLayer({ processRunner: interrupted }).layer;
    const interruptedExit = await runEffectExit(
      Effect.provide(kernelProbe({ argv: ['interrupted'] }), interruptedLayer),
    );
    assert.ok(Exit.isFailure(interruptedExit));
    assert.equal(Cause.hasInterruptsOnly(interruptedExit.cause), true);

    const defect = new Error('fake process defect');
    const defective = makeFakeProcessRunner({ execute: () => Effect.die(defect) });
    const defectiveLayer = fakeKernelLayer({ processRunner: defective }).layer;
    const defectExit = await runEffectExit(
      Effect.provide(kernelProbe({ argv: ['defect'] }), defectiveLayer),
    );
    assert.ok(Exit.isFailure(defectExit));
    assert.equal(Exit.hasDies(defectExit), true);
    assert.equal(Cause.squash(defectExit.cause), defect);
  });

  test('runEffectExit keeps typed failures and defects distinct', async () => {
    const failure = { ok: false as const, code: 'ENOENT', err: 'expected refusal' };
    const typedError = new ProcessSpawnError({
      message: failure.err,
      result: failure,
    });
    const defect = new Error('unexpected defect');
    const typedExit = await runEffectExit(Effect.fail(typedError));
    const defectExit = await runEffectExit(Effect.die(defect));

    assert.ok(Exit.isFailure(typedExit));
    assert.equal(Exit.hasFails(typedExit), true);
    assert.equal(Exit.hasDies(typedExit), false);
    assert.equal(Cause.squash(typedExit.cause), typedError);

    assert.ok(Exit.isFailure(defectExit));
    assert.equal(Exit.hasFails(defectExit), false);
    assert.equal(Exit.hasDies(defectExit), true);
    assert.equal(Cause.squash(defectExit.cause), defect);
  });

  test('helpers expose RC.110 TestClock and TestConsole services', async () => {
    const program = Effect.gen(function* () {
      yield* Console.log('kernel-ready');
      const sleeper = yield* Effect.sleep('1 second').pipe(Effect.as('awake'), Effect.forkChild);
      yield* TestClock.adjust('1 second');
      const value = yield* Fiber.join(sleeper);
      const lines = yield* TestConsole.logLines;
      return { value, lines };
    });

    const exit = await runEffectExit(Effect.provide(program, TestServicesLayer));

    assert.ok(Exit.isSuccess(exit));
    assert.deepEqual(exit.value, { value: 'awake', lines: ['kernel-ready'] });
  });
});

describe('BunRuntime kernel lifecycle fixture', () => {
  test('a finite root clears its keepalive and exits naturally', async () => {
    const child = Bun.spawn([process.execPath, LIFECYCLE_FIXTURE, 'natural-exit'], {
      stdin: 'ignore',
      stdout: 'pipe',
      stderr: 'pipe',
    });
    const stdoutPromise = new Response(child.stdout).text();
    const stderrPromise = new Response(child.stderr).text();

    try {
      const exitCode = await within(child.exited, 'natural root exit');
      const [stdout, stderr] = await within(
        Promise.all([stdoutPromise, stderrPromise]),
        'natural root pipe drain',
      );

      assert.equal(exitCode, 0);
      assert.equal(stdout, 'natural-complete\n');
      assert.equal(stderr, '');
    } finally {
      await forceRetire(child, 'natural root');
      await within(
        Promise.allSettled([stdoutPromise, stderrPromise]),
        'natural root cleanup pipe drain',
      );
    }
  });

  test('an unfinished root stays alive until BunRuntime receives SIGTERM', async () => {
    const child = Bun.spawn([process.execPath, LIFECYCLE_FIXTURE, 'root-keepalive'], {
      stdin: 'ignore',
      stdout: 'pipe',
      stderr: 'pipe',
    });
    const stderrPromise = new Response(child.stderr).text();
    const reader = child.stdout.getReader();

    try {
      const ready = await within(reader.read(), 'root readiness');
      assert.equal(ready.done, false);
      assert.equal(new TextDecoder().decode(ready.value), 'root-ready\n');

      const state = await Promise.race([
        child.exited.then((exitCode) => ({ tag: 'exited' as const, exitCode })),
        delay(250).then(() => ({ tag: 'alive' as const })),
      ]);
      assert.deepEqual(state, { tag: 'alive' });

      child.kill('SIGTERM');
      // RC.110's stock BunRuntime teardown maps an interrupted root to 130. P4 must install the
      // Fleet Deck root interpreter that treats a clean, coordinated daemon SIGTERM as exit 0.
      assert.equal(await within(child.exited, 'interrupted root exit'), 130);
      assert.equal(await stderrPromise, '');
    } finally {
      await reader.cancel().catch(() => undefined);
      reader.releaseLock();
      await forceRetire(child, 'held root');
      await within(stderrPromise, 'held root cleanup stderr drain');
    }
  });
});
