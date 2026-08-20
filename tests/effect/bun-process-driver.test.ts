import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Fiber from 'effect/Fiber';
import {
  PROCESS_DRIVER_KILL_GRACE_MS,
  PROCESS_DRIVER_MAX_OUTPUT_BYTES,
  makeProcessRunnerServiceFromDriver,
  type ProcessDriver,
  type ProcessExecution,
} from '../../src/daemon/app/services/process-driver.ts';
import {
  ProcessNonZeroExitError,
  ProcessOutputLimitError,
  ProcessSpawnError,
  ProcessTimeoutError,
} from '../../src/daemon/app/errors.ts';
import { type ProcessResult, ProcessRunner } from '../../src/daemon/app/services/process-runner.ts';
import { makeBunProcessDriver } from '../../src/daemon/platform/bun/process-driver.ts';
import { ProcessRunnerLive } from '../../src/daemon/platform/bun/process-runner-live.ts';
import { makeNodeProcessDriverReference } from '../../src/daemon/platform/node/process-driver-reference.ts';
import { scaleMs, waitUntil } from '../helpers/wait.ts';
import { runEffectExit } from './helpers.ts';

const FIXTURE = fileURLToPath(new URL('./fixtures/bun-process-child.ts', import.meta.url));
const FILES_FIXTURE = fileURLToPath(
  new URL('../fixtures/files-run-bounded-child.mjs', import.meta.url),
);

const CANCELLED_RESULT = { ok: false, code: 'ECANCELED', err: 'cancelled' } as const;
const OUTPUT_LIMIT_RESULT = {
  ok: false,
  code: 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER',
  err: `subprocess output exceeded ${String(PROCESS_DRIVER_MAX_OUTPUT_BYTES)} bytes`,
} as const;

interface SignalMetrics {
  added: number;
  removed: number;
}

function fixtureArgv(mode: string, ...arguments_: string[]): [string, ...string[]] {
  return [process.execPath, FIXTURE, mode, ...arguments_];
}

function pause(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 5_000): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out after ${String(scaleMs(timeoutMs))}ms`)),
          scaleMs(timeoutMs),
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function settled<Decision>(execution: ProcessExecution<Decision>): Promise<Decision> {
  const decision = await within(execution.decision, 'process decision');
  await within(execution.cleanup, 'process cleanup');
  return decision;
}

async function compareBoundedDrivers(
  nodeDriver: ProcessDriver,
  bunDriver: ProcessDriver,
  request: Parameters<ProcessDriver['startBounded']>[0],
): Promise<void> {
  const [nodeResult, bunResult] = await Promise.all([
    settled(nodeDriver.startBounded(request)),
    settled(bunDriver.startBounded(request)),
  ]);
  assert.deepEqual(bunResult, nodeResult);
}

function trackedSignal(controller: AbortController): {
  readonly signal: AbortSignal;
  readonly metrics: SignalMetrics;
} {
  const metrics = { added: 0, removed: 0 };
  const source = controller.signal;
  const signal = {
    get aborted() {
      return source.aborted;
    },
    addEventListener(...arguments_: Parameters<AbortSignal['addEventListener']>) {
      metrics.added += 1;
      source.addEventListener(...arguments_);
    },
    removeEventListener(...arguments_: Parameters<AbortSignal['removeEventListener']>) {
      metrics.removed += 1;
      source.removeEventListener(...arguments_);
    },
  } as AbortSignal;
  return { signal, metrics };
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
    // Already gone.
  }
}

function readPid(pidFile: string): number | null {
  try {
    const pid = Number(readFileSync(pidFile, 'utf8'));
    return Number.isSafeInteger(pid) && pid > 1 ? pid : null;
  } catch {
    return null;
  }
}

function scratchPidFile(prefix: string): { readonly dir: string; readonly pidFile: string } {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  return { dir, pidFile: path.join(dir, 'child.pid') };
}

async function compareDrivers(
  nodeDriver: ProcessDriver,
  bunDriver: ProcessDriver,
  request: Parameters<ProcessDriver['start']>[0],
): Promise<void> {
  const nodeExecution = nodeDriver.start(request);
  const bunExecution = bunDriver.start(request);
  const [nodeResult, bunResult] = await Promise.all([
    settled(nodeExecution),
    settled(bunExecution),
  ]);
  assert.deepEqual(bunResult, nodeResult);
}

describe('Bun ProcessDriver compatibility policy', () => {
  test('differentially matches the extracted Node driver for exact result bytes', async () => {
    const nodeDriver = makeNodeProcessDriverReference();
    const bunDriver = makeBunProcessDriver();
    const stdoutBytes = 640 * 1024;

    try {
      await compareDrivers(nodeDriver, bunDriver, {
        argv: fixtureArgv('roundtrip', '$HOME;$(touch never)'),
        stdin: new Uint8Array([0x66, 0x6c, 0x65, 0x65, 0x74]),
        env: { FLEETDECK_BUN_DRIVER_OVERRIDE_ENV: 'differential' },
        timeoutMs: 5_000,
      });
      await compareDrivers(nodeDriver, bunDriver, {
        argv: fixtureArgv('nonzero'),
        timeoutMs: 2_000,
      });
      await compareDrivers(nodeDriver, bunDriver, {
        argv: fixtureArgv(
          'bytes',
          String(stdoutBytes),
          String(PROCESS_DRIVER_MAX_OUTPUT_BYTES - stdoutBytes),
        ),
        timeoutMs: 5_000,
      });
      await compareDrivers(nodeDriver, bunDriver, {
        argv: fixtureArgv(
          'bytes',
          String(stdoutBytes),
          String(PROCESS_DRIVER_MAX_OUTPUT_BYTES + 1 - stdoutBytes),
        ),
        timeoutMs: 5_000,
      });
      await compareDrivers(nodeDriver, bunDriver, {
        argv: fixtureArgv('fragmented-utf8', String(PROCESS_DRIVER_MAX_OUTPUT_BYTES - 4)),
        timeoutMs: 5_000,
      });
      await compareDrivers(nodeDriver, bunDriver, {
        argv: fixtureArgv('incomplete-utf8', '0'),
        timeoutMs: 2_000,
      });
    } finally {
      await Promise.all([nodeDriver.close(), bunDriver.close()]);
    }
  });

  test('settles decision and cleanup when a child exits before consuming large finite stdin', async () => {
    const nodeDriver = makeNodeProcessDriverReference();
    const bunDriver = makeBunProcessDriver();
    const unhandledRejections: unknown[] = [];
    const onUnhandledRejection = (reason: unknown) => {
      unhandledRejections.push(reason);
    };
    process.on('unhandledRejection', onUnhandledRejection);

    try {
      const request = {
        argv: fixtureArgv('stdin-early-exit'),
        stdin: Buffer.alloc(16 * 1024 * 1024, 0x61),
        timeoutMs: 5_000,
      };
      const nodeExecution = nodeDriver.start(request);
      const bunExecution = bunDriver.start(request);
      const [nodeResult, bunResult] = await Promise.all([
        within(nodeExecution.decision, 'Node early-stdin decision'),
        within(bunExecution.decision, 'Bun early-stdin decision'),
      ]);
      await Promise.all([
        within(nodeExecution.cleanup, 'Node early-stdin cleanup'),
        within(bunExecution.cleanup, 'Bun early-stdin cleanup'),
      ]);

      const expected = { ok: true, out: 'stdin was not consumed\n' } as const;
      assert.deepEqual(nodeResult, expected);
      assert.deepEqual(bunResult, expected);
      await Promise.all([
        within(nodeDriver.close(), 'Node early-stdin driver close'),
        within(bunDriver.close(), 'Bun early-stdin driver close'),
      ]);
      await pause(20);
      assert.deepEqual(unhandledRejections, []);
    } finally {
      await Promise.all([nodeDriver.close(), bunDriver.close()]);
      process.off('unhandledRejection', onUnhandledRejection);
    }
  });

  test('differentially preserves files.ts bounded bytes, flags, stdin, and spawn failures', async () => {
    const nodeDriver = makeNodeProcessDriverReference();
    const bunDriver = makeBunProcessDriver();
    const missing = path.join(
      tmpdir(),
      `fleetdeck-bounded-missing-${String(process.pid)}-${String(Date.now())}`,
    );
    const filesArgv = (mode: string, ...arguments_: string[]): [string, ...string[]] => [
      process.execPath,
      FILES_FIXTURE,
      mode,
      ...arguments_,
    ];

    try {
      await compareBoundedDrivers(nodeDriver, bunDriver, {
        argv: filesArgv('bytes', '24', '40'),
        timeoutMs: 2_000,
        maxBytes: 64,
      });
      await compareBoundedDrivers(nodeDriver, bunDriver, {
        argv: filesArgv('bytes', '24', '41', 'hold'),
        timeoutMs: 2_000,
        maxBytes: 64,
      });
      await compareBoundedDrivers(nodeDriver, bunDriver, {
        argv: filesArgv('stdin'),
        stdin: new Uint8Array([0, 1, 0xfe, 0xff]),
        timeoutMs: 2_000,
        maxBytes: 128,
      });
      await compareBoundedDrivers(nodeDriver, bunDriver, {
        argv: filesArgv('nonzero'),
        timeoutMs: 2_000,
        maxBytes: 128,
      });
      await compareBoundedDrivers(nodeDriver, bunDriver, {
        argv: [missing],
        timeoutMs: 1_000,
        maxBytes: 64,
      });
      await compareBoundedDrivers(nodeDriver, bunDriver, {
        argv: ['invalid\0command'],
        timeoutMs: 1_000,
        maxBytes: 64,
      });
    } finally {
      await Promise.all([nodeDriver.close(), bunDriver.close()]);
    }
  });

  test('differentially preserves legacy synchronous spawn validation and quoting', async () => {
    const nodeDriver = makeNodeProcessDriverReference();
    const bunDriver = makeBunProcessDriver();
    const quoteAndControl = `x'"\0y`;
    const requests: Array<Parameters<ProcessDriver['start']>[0]> = [
      { argv: [''] },
      { argv: [quoteAndControl] },
      { argv: [process.execPath, quoteAndControl] },
      { argv: [process.execPath], cwd: quoteAndControl },
      { argv: [process.execPath], env: { [quoteAndControl]: 'valid' } },
      { argv: [process.execPath], env: { FLEETDECK_INVALID_ENV: quoteAndControl } },
      {
        argv: [process.execPath, 'argument\0wins'],
        cwd: 'cwd\0loses',
        env: { FLEETDECK_INVALID_ENV: 'environment\0loses' },
      },
    ];

    try {
      for (const request of requests) {
        await compareDrivers(nodeDriver, bunDriver, request);
        await compareBoundedDrivers(nodeDriver, bunDriver, {
          argv: request.argv,
          ...(request.cwd === undefined ? {} : { cwd: request.cwd }),
          ...(request.env === undefined ? {} : { env: request.env }),
          timeoutMs: 1_000,
          maxBytes: 64,
        });
      }
    } finally {
      await Promise.all([nodeDriver.close(), bunDriver.close()]);
    }
  });

  test('preserves argv, cwd, live env, env overrides, stdin, and concurrent drains', async () => {
    const driver = makeBunProcessDriver();
    const directory = mkdtempSync(path.join(tmpdir(), 'fleetdeck-bun-process-roundtrip-'));
    const controller = new AbortController();
    const { signal, metrics } = trackedSignal(controller);
    const liveKey = 'FLEETDECK_BUN_DRIVER_LIVE_ENV';
    const previous = process.env[liveKey];
    const literal = '$HOME;$(touch never) "quoted value"';

    try {
      process.env[liveKey] = 'mutated-after-driver-construction';
      const result = await settled(
        driver.start({
          argv: fixtureArgv('roundtrip', literal),
          cwd: directory,
          env: { FLEETDECK_BUN_DRIVER_OVERRIDE_ENV: 'request-override' },
          stdin: 'finite stdin 💩',
          timeoutMs: 5_000,
          signal,
        }),
      );
      controller.abort();
      await pause(20);

      assert.equal(result.ok, true);
      assert.deepEqual(JSON.parse(result.out), {
        literal,
        cwd: realpathSync(directory),
        liveEnv: 'mutated-after-driver-construction',
        overrideEnv: 'request-override',
        stdin: 'finite stdin 💩',
      });
      assert.deepEqual(metrics, { added: 1, removed: 1 });
    } finally {
      if (previous === undefined) delete process.env[liveKey];
      else process.env[liveKey] = previous;
      await driver.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });

  test('maps spawn failures, nonzero exits, and immediate publication races exactly', async () => {
    const driver = makeBunProcessDriver();
    try {
      const missing = await settled(
        driver.start({
          argv: [path.join(tmpdir(), `fleetdeck-missing-${String(process.pid)}-${Date.now()}`)],
          timeoutMs: 2_000,
        }),
      );
      assert.equal(missing.ok, false);
      if (missing.ok) throw new Error('missing executable unexpectedly succeeded');
      assert.equal(missing.code, 'ENOENT');
      assert.match(missing.err, /ENOENT|no such file/i);

      const invalid = await settled(driver.start({ argv: ['invalid\0command'], timeoutMs: 2_000 }));
      assert.equal(invalid.ok, false);
      if (invalid.ok) throw new Error('invalid argv unexpectedly succeeded');
      assert.equal(Object.hasOwn(invalid, 'code'), false);
      assert.equal(
        invalid.err,
        "The argument 'file' must be a string without null bytes. Received 'invalid\0command'",
      );

      assert.deepEqual(
        await settled(driver.start({ argv: fixtureArgv('nonzero'), timeoutMs: 2_000 })),
        { ok: false, code: 7, err: 'fatal: bun fixture failed' },
      );

      const immediate = Array.from({ length: 24 }, () =>
        driver.start({ argv: fixtureArgv('immediate'), timeoutMs: 2_000 }),
      );
      const results = await Promise.all(immediate.map((execution) => execution.decision));
      await Promise.all(immediate.map((execution) => execution.cleanup));
      for (const result of results) assert.deepEqual(result, { ok: true, out: '' });
    } finally {
      await driver.close();
    }
  });

  test('enforces one shared pre-decode cap and Buffer UTF-8 semantics', async () => {
    const driver = makeBunProcessDriver();
    const stdoutBytes = 640 * 1024;
    try {
      const exact = await settled(
        driver.start({
          argv: fixtureArgv(
            'bytes',
            String(stdoutBytes),
            String(PROCESS_DRIVER_MAX_OUTPUT_BYTES - stdoutBytes),
          ),
          timeoutMs: 5_000,
        }),
      );
      assert.equal(exact.ok, true);
      assert.equal(exact.out.length, stdoutBytes);
      assert.equal(exact.out[0], 'a');
      assert.equal(exact.out.at(-1), 'a');

      const controller = new AbortController();
      const { signal, metrics } = trackedSignal(controller);
      const over = driver.start({
        argv: fixtureArgv(
          'bytes',
          String(stdoutBytes),
          String(PROCESS_DRIVER_MAX_OUTPUT_BYTES + 1 - stdoutBytes),
        ),
        timeoutMs: 5_000,
        signal,
      });
      assert.deepEqual(await within(over.decision, 'output-limit decision'), OUTPUT_LIMIT_RESULT);
      await within(over.cleanup, 'output-limit cleanup');
      controller.abort();
      await pause(20);
      assert.deepEqual(metrics, { added: 1, removed: 1 });

      const asciiBytes = PROCESS_DRIVER_MAX_OUTPUT_BYTES - 4;
      const fragmented = await settled(
        driver.start({
          argv: fixtureArgv('fragmented-utf8', String(asciiBytes)),
          timeoutMs: 5_000,
        }),
      );
      assert.equal(fragmented.ok, true);
      assert.equal(Buffer.byteLength(fragmented.out, 'utf8'), PROCESS_DRIVER_MAX_OUTPUT_BYTES);
      assert.equal(fragmented.out.endsWith('💩'), true);

      const incomplete = await settled(
        driver.start({
          argv: fixtureArgv('incomplete-utf8', String(PROCESS_DRIVER_MAX_OUTPUT_BYTES - 1)),
          timeoutMs: 5_000,
        }),
      );
      assert.equal(incomplete.ok, true);
      assert.equal(incomplete.out.endsWith('\uFFFD'), true);
      assert.equal(incomplete.out.length, PROCESS_DRIVER_MAX_OUTPUT_BYTES);
      assert.equal(Buffer.byteLength(incomplete.out, 'utf8'), PROCESS_DRIVER_MAX_OUTPUT_BYTES + 2);
    } finally {
      await driver.close();
    }
  });

  test('pre-abort and close are exact-once, idempotent, and refuse later starts', async () => {
    const driver = makeBunProcessDriver();
    const controller = new AbortController();
    controller.abort();
    const { signal, metrics } = trackedSignal(controller);

    const execution = driver.start({
      argv: fixtureArgv('immediate'),
      timeoutMs: 2_000,
      signal,
    });
    assert.deepEqual(await settled(execution), CANCELLED_RESULT);
    assert.deepEqual(metrics, { added: 0, removed: 1 });

    const firstClose = driver.close();
    const secondClose = driver.close();
    assert.equal(firstClose, secondClose);
    await firstClose;
    assert.deepEqual(
      await settled(driver.start({ argv: fixtureArgv('immediate'), timeoutMs: 2_000 })),
      CANCELLED_RESULT,
    );
  });

  test('timeout decides immediately, then TERM-to-KILL cleanup reaps the child', async () => {
    const driver = makeBunProcessDriver();
    const scratch = scratchPidFile('fleetdeck-bun-process-timeout-');
    const controller = new AbortController();
    const { signal, metrics } = trackedSignal(controller);
    let pid: number | null = null;

    try {
      const timeoutMs = 300;
      const started = Date.now();
      const execution = driver.start({
        argv: fixtureArgv('term-resistant', scratch.pidFile),
        timeoutMs,
        signal,
      });
      pid = await waitUntil(() => readPid(scratch.pidFile), {
        timeoutMs: 2_000,
        intervalMs: 10,
        label: 'TERM-resistant Bun child pid',
      });
      const decision = await within(execution.decision, 'timeout decision');
      const decisionElapsed = Date.now() - started;
      assert.deepEqual(decision, {
        ok: false,
        code: 'ETIMEDOUT',
        err: `timed out after ${String(timeoutMs)}ms`,
      });
      assert.ok(decisionElapsed < scaleMs(900), `decision took ${String(decisionElapsed)}ms`);

      const cleanupStarted = Date.now();
      const earlyCleanup = await Promise.race([
        execution.cleanup.then(() => 'done' as const),
        pause(200).then(() => 'pending' as const),
      ]);
      assert.equal(earlyCleanup, 'pending', 'public timeout must precede TERM/KILL cleanup');
      await within(execution.cleanup, 'timeout reap', 3_000);
      assert.ok(
        Date.now() - cleanupStarted >= PROCESS_DRIVER_KILL_GRACE_MS - 350,
        'TERM-resistant cleanup must observe the one-second grace',
      );
      await waitUntil(() => !pidAlive(pid as number), {
        timeoutMs: 2_000,
        intervalMs: 10,
        label: 'TERM-resistant Bun child reaped',
      });
      controller.abort();
      await pause(20);
      assert.deepEqual(metrics, { added: 1, removed: 1 });
    } finally {
      await driver.close();
      forceKill(pid);
      rmSync(scratch.dir, { recursive: true, force: true });
    }
  });

  test('POSIX cancellation targets and reaps the complete process group', async () => {
    if (process.platform === 'win32') return;
    const driver = makeBunProcessDriver();
    const scratch = scratchPidFile('fleetdeck-bun-process-group-');
    const controller = new AbortController();
    const { signal, metrics } = trackedSignal(controller);
    let helperPid: number | null = null;

    try {
      const execution = driver.start({
        argv: fixtureArgv('group-parent', scratch.pidFile),
        timeoutMs: 10_000,
        signal,
        killTree: true,
      });
      helperPid = await waitUntil(() => readPid(scratch.pidFile), {
        timeoutMs: 2_000,
        intervalMs: 10,
        label: 'Bun process-group helper pid',
      });
      const cancelledAt = Date.now();
      controller.abort();
      assert.deepEqual(await within(execution.decision, 'group cancellation'), CANCELLED_RESULT);
      assert.ok(Date.now() - cancelledAt < scaleMs(500), 'cancellation decision is immediate');
      await within(execution.cleanup, 'process-group cleanup', 3_000);
      await waitUntil(() => !pidAlive(helperPid as number), {
        timeoutMs: 2_000,
        intervalMs: 10,
        label: 'TERM-immune process-group helper reaped',
      });
      assert.deepEqual(metrics, { added: 1, removed: 1 });
    } finally {
      await driver.close();
      forceKill(helperPid);
      rmSync(scratch.dir, { recursive: true, force: true });
    }
  });

  test('driver close cancels active work and publishes one joined close Promise', async () => {
    const driver = makeBunProcessDriver();
    const scratch = scratchPidFile('fleetdeck-bun-process-close-');
    let pid: number | null = null;
    try {
      const execution = driver.start({
        argv: fixtureArgv('term-resistant', scratch.pidFile),
        timeoutMs: 10_000,
      });
      pid = await waitUntil(() => readPid(scratch.pidFile), {
        timeoutMs: 2_000,
        intervalMs: 10,
        label: 'driver-close child pid',
      });
      const firstClose = driver.close();
      const secondClose = driver.close();
      assert.equal(firstClose, secondClose);
      assert.deepEqual(await within(execution.decision, 'driver-close decision'), CANCELLED_RESULT);
      await within(firstClose, 'driver-close join', 3_000);
      assert.equal(pidAlive(pid), false);
    } finally {
      await driver.close();
      forceKill(pid);
      rmSync(scratch.dir, { recursive: true, force: true });
    }
  });

  test('forceClose bypasses an in-flight TERM grace and reaps the complete process group', async () => {
    if (process.platform === 'win32') return;
    const driver = makeBunProcessDriver();
    const scratch = scratchPidFile('fleetdeck-bun-process-force-close-');
    let helperPid: number | null = null;
    try {
      const execution = driver.start({
        argv: fixtureArgv('group-parent', scratch.pidFile),
        timeoutMs: 10_000,
        killTree: true,
      });
      helperPid = await waitUntil(() => readPid(scratch.pidFile), {
        timeoutMs: 2_000,
        intervalMs: 10,
        label: 'force-close TERM-immune group helper pid',
      });

      const close = driver.close();
      assert.deepEqual(await within(execution.decision, 'force-close decision'), CANCELLED_RESULT);
      await pause(50);
      const forcedAt = Date.now();
      driver.forceClose();
      assert.equal(driver.close(), close, 'force escalation retains the published close join');
      await within(close, 'forced process-group join', 1_000);
      const forceElapsed = Date.now() - forcedAt;

      assert.ok(
        forceElapsed < scaleMs(500),
        `force join must bypass the one-second TERM grace (${String(forceElapsed)}ms)`,
      );
      await waitUntil(() => !pidAlive(helperPid as number), {
        timeoutMs: 500,
        intervalMs: 10,
        label: 'forced TERM-immune group helper reaped',
      });
      assert.deepEqual(
        await settled(driver.start({ argv: fixtureArgv('immediate'), timeoutMs: 2_000 })),
        CANCELLED_RESULT,
        'force closes later process admission',
      );
    } finally {
      await driver.close();
      forceKill(helperPid);
      rmSync(scratch.dir, { recursive: true, force: true });
    }
  });
});

describe('Bun ProcessRunnerLive ownership', () => {
  test('service maps driver failures to tags while preserving interruption and defects', async () => {
    let nextDecision: Promise<ProcessResult> = Promise.resolve({ ok: true, out: '' });
    const driver: ProcessDriver = {
      start: () => ({ decision: nextDecision, cleanup: Promise.resolve(), cancel() {} }),
      startBounded: () => {
        throw new Error('bounded policy is outside this typed process fixture');
      },
      close: () => Promise.resolve(),
    };
    const service = makeProcessRunnerServiceFromDriver(driver);
    const cases = [
      {
        result: { ok: false, code: 'ENOENT', err: 'missing executable' } as const,
        tag: 'ProcessSpawnError',
      },
      {
        result: { ok: false, code: 7, err: 'process exited 7' } as const,
        tag: 'ProcessNonZeroExitError',
      },
      {
        result: { ok: false, code: 'ETIMEDOUT', err: 'timed out after 23ms' } as const,
        tag: 'ProcessTimeoutError',
      },
      {
        result: OUTPUT_LIMIT_RESULT,
        tag: 'ProcessOutputLimitError',
      },
    ] as const;

    for (const item of cases) {
      nextDecision = Promise.resolve(item.result);
      const exit = await runEffectExit(service.run({ argv: ['fixture'], timeoutMs: 23 }));
      assert.ok(Exit.isFailure(exit));
      assert.equal(Exit.hasFails(exit), true);
      assert.equal(Exit.hasDies(exit), false);
      const error = Cause.squash(exit.cause);
      assert.ok(
        error instanceof ProcessSpawnError ||
          error instanceof ProcessNonZeroExitError ||
          error instanceof ProcessTimeoutError ||
          error instanceof ProcessOutputLimitError,
      );
      assert.equal(error._tag, item.tag);
      assert.deepEqual(error.result, item.result);
      if (error instanceof ProcessTimeoutError) assert.equal(error.timeoutMs, 23);
      if (error instanceof ProcessOutputLimitError) {
        assert.equal(error.maxOutputBytes, PROCESS_DRIVER_MAX_OUTPUT_BYTES);
      }
    }

    nextDecision = Promise.resolve(CANCELLED_RESULT);
    const interrupted = await runEffectExit(service.run({ argv: ['fixture'] }));
    assert.ok(Exit.isFailure(interrupted));
    assert.equal(Cause.hasInterruptsOnly(interrupted.cause), true);
    assert.equal(Exit.hasFails(interrupted), false);

    const defect = new Error('driver decision defect');
    nextDecision = Promise.reject(defect);
    const defective = await runEffectExit(service.run({ argv: ['fixture'] }));
    assert.ok(Exit.isFailure(defective));
    assert.equal(Exit.hasDies(defective), true);
    assert.equal(Cause.squash(defective.cause), defect);
  });

  test('Layer is rebuildable and Effect interruption joins callback cleanup', async () => {
    for (let index = 0; index < 2; index += 1) {
      const exit = await runEffectExit(
        Effect.provide(
          ProcessRunner.use((runner) =>
            runner.run({ argv: fixtureArgv('immediate'), timeoutMs: 2_000 }),
          ),
          ProcessRunnerLive,
        ),
      );
      assert.ok(Exit.isSuccess(exit));
      assert.deepEqual(exit.value, { ok: true, out: '' });
    }

    const scratch = scratchPidFile('fleetdeck-bun-process-effect-interrupt-');
    let pid: number | null = null;
    try {
      const program = Effect.gen(function* () {
        const runner = yield* ProcessRunner;
        const fiber = yield* Effect.forkChild(
          runner.run({
            argv: fixtureArgv('term-resistant', scratch.pidFile),
            timeoutMs: 10_000,
          }),
        );
        pid = yield* Effect.promise(() =>
          waitUntil(() => readPid(scratch.pidFile), {
            timeoutMs: 2_000,
            intervalMs: 10,
            label: 'Effect-owned child pid',
          }),
        );
        const interruptedAt = Date.now();
        yield* Fiber.interrupt(fiber);
        return Date.now() - interruptedAt;
      });
      const exit = await within(
        runEffectExit(Effect.provide(program, ProcessRunnerLive)),
        'ProcessRunnerLive interruption',
        4_000,
      );
      assert.ok(Exit.isSuccess(exit));
      assert.ok(
        exit.value >= PROCESS_DRIVER_KILL_GRACE_MS - 350,
        'Effect.callback interruption must await TERM/KILL cleanup',
      );
      assert.ok(pid !== null);
      assert.equal(pidAlive(pid), false);
    } finally {
      forceKill(pid);
      rmSync(scratch.dir, { recursive: true, force: true });
    }
  });
});
