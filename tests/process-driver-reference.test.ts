import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import type { TestContext } from 'node:test';
import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Fiber from 'effect/Fiber';
import {
  makeProcessRunnerLayerFromDriver,
  makeProcessRunnerServiceFromDriver,
  PROCESS_DRIVER_MAX_OUTPUT_BYTES,
  type ProcessDriver,
  type ProcessExecution,
} from '../src/daemon/app/services/process-driver.ts';
import {
  type ProcessRequest,
  type ProcessResult,
  ProcessRunner,
} from '../src/daemon/app/services/process-runner.ts';
import { execFileP, type ExecResult } from '../src/daemon/exec.ts';
import { makeNodeProcessDriverReference } from '../src/daemon/platform/node/process-driver-reference.ts';
import { runEffectExit } from './effect/helpers.ts';
import test from './helpers/harness-test.ts';
import { waitUntil } from './helpers/wait.ts';

const CANCELLED_RESULT: ProcessResult = {
  ok: false,
  code: 'ECANCELED',
  err: 'cancelled',
};

function pause(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 4_000): Promise<T> {
  let timer: NodeJS.Timeout | null = null;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out after ${String(timeoutMs)}ms`)),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
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

function forceKill(pid: number): void {
  try {
    process.kill(pid, 'SIGKILL');
  } catch {
    /* already gone */
  }
}

function pidFixture(
  t: TestContext,
  prefix: string,
): { pidFile: string; readPid: () => number | null } {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  const pidFile = path.join(dir, 'child.pid');
  const readPid = (): number | null => {
    try {
      const pid = Number(readFileSync(pidFile, 'utf8'));
      return Number.isInteger(pid) && pid > 1 ? pid : null;
    } catch {
      return null;
    }
  };
  t.after(async () => {
    const pid = readPid();
    if (pid !== null && pidAlive(pid)) {
      forceKill(pid);
      try {
        await waitUntil(() => !pidAlive(pid), {
          timeoutMs: 2_000,
          intervalMs: 10,
          label: `${prefix} cleanup`,
        });
      } catch {
        /* the owning assertion reports residue; this hook is best effort */
      }
    }
    rmSync(dir, { recursive: true, force: true });
  });
  return { pidFile, readPid };
}

interface LegacyOptions {
  timeout?: number;
  env?: NodeJS.ProcessEnv;
  signal?: AbortSignal;
  killTree?: boolean;
}

function legacyRun(
  executable: string,
  args: readonly string[],
  options: LegacyOptions = {},
): Promise<ExecResult> {
  return execFileP(executable, args, options);
}

function referenceStart(
  driver: ProcessDriver,
  executable: string,
  args: readonly string[],
  options: LegacyOptions = {},
): ProcessExecution {
  return driver.start({
    argv: [executable, ...args],
    ...(options.timeout === undefined ? {} : { timeoutMs: options.timeout }),
    ...(options.env === undefined ? {} : { env: options.env }),
    ...(options.signal === undefined ? {} : { signal: options.signal }),
    ...(options.killTree === undefined ? {} : { killTree: options.killTree }),
  });
}

interface SignalMetrics {
  added: number;
  removed: number;
}

function trackedSignal(controller: AbortController): {
  signal: AbortSignal;
  metrics: SignalMetrics;
} {
  const metrics = { added: 0, removed: 0 };
  const source = controller.signal;
  const signal = {
    get aborted() {
      return source.aborted;
    },
    addEventListener(...args: Parameters<AbortSignal['addEventListener']>) {
      metrics.added += 1;
      source.addEventListener(...args);
    },
    removeEventListener(...args: Parameters<AbortSignal['removeEventListener']>) {
      metrics.removed += 1;
      source.removeEventListener(...args);
    },
  } as AbortSignal;
  return { signal, metrics };
}

test('Node reference differentially preserves the legacy result/byte/error matrix', async (t) => {
  const driver = makeNodeProcessDriverReference();
  t.after(() => driver.close());
  const missing = path.join(
    tmpdir(),
    `fleetdeck-reference-missing-${String(process.pid)}-${String(Date.now())}`,
  );
  assert.equal(existsSync(missing), false);

  const stdoutBytes = 640 * 1024;
  const exactStderrBytes = PROCESS_DRIVER_MAX_OUTPUT_BYTES - stdoutBytes;
  const cases: ReadonlyArray<{
    name: string;
    executable: string;
    args: readonly string[];
    options?: LegacyOptions;
  }> = [
    {
      name: 'success',
      executable: process.execPath,
      args: ['-e', 'process.stdout.write("hello")'],
    },
    { name: 'missing executable', executable: missing, args: [] },
    { name: 'synchronous spawn validation', executable: 'invalid\0command', args: [] },
    {
      name: 'nonzero and trimmed stderr',
      executable: process.execPath,
      args: [
        '-e',
        `const fs = require('node:fs');
         fs.writeSync(1, 'ignored stdout');
         fs.writeSync(2, '  fatal: reference fixture  \\n');
         process.exit(7);`,
      ],
    },
    {
      name: 'combined exact 1 MiB',
      executable: process.execPath,
      args: [
        '-e',
        `const fs = require('node:fs');
         fs.writeSync(1, Buffer.alloc(${String(stdoutBytes)}, 0x61));
         fs.writeSync(2, Buffer.alloc(${String(exactStderrBytes)}, 0x62));`,
      ],
      options: { timeout: 5_000 },
    },
    {
      name: 'combined 1 MiB plus one',
      executable: process.execPath,
      args: [
        '-e',
        `const fs = require('node:fs');
         fs.writeSync(1, Buffer.alloc(${String(stdoutBytes)}, 0x61));
         fs.writeSync(2, Buffer.alloc(${String(exactStderrBytes + 1)}, 0x62));`,
      ],
      options: { timeout: 5_000 },
    },
    {
      name: 'fragmented valid UTF-8',
      executable: process.execPath,
      args: [
        '-e',
        `const fs = require('node:fs');
         fs.writeSync(1, Buffer.from([0xf0]));
         setTimeout(() => fs.writeSync(1, Buffer.from([0x9f, 0x92, 0xa9])), 10);`,
      ],
    },
    {
      name: 'invalid UTF-8 replacement',
      executable: process.execPath,
      args: ['-e', `require('node:fs').writeSync(1, Buffer.from([0xc3]));`],
    },
  ];

  for (const fixture of cases) {
    const expected = await legacyRun(fixture.executable, fixture.args, fixture.options);
    const execution = referenceStart(driver, fixture.executable, fixture.args, fixture.options);
    const actual = await within(execution.decision, `${fixture.name} decision`);
    await within(execution.cleanup, `${fixture.name} cleanup`);
    assert.deepEqual(actual, expected, fixture.name);
  }
});

test('Node reference supports cwd, live env overlays, and finite string/byte stdin', async (t) => {
  const driver = makeNodeProcessDriverReference();
  t.after(() => driver.close());
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-reference-cwd-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const envKey = `FLEETDECK_REFERENCE_ENV_${String(process.pid)}_${String(Date.now())}`;
  const previous = process.env[envKey];

  try {
    process.env[envKey] = 'live-parent-value';
    const request: ProcessRequest = {
      argv: [
        process.execPath,
        '-e',
        `const chunks = [];
         process.stdin.on('data', (chunk) => chunks.push(chunk));
         process.stdin.on('end', () => process.stdout.write(JSON.stringify({
           cwd: process.cwd(),
           inherited: process.env[${JSON.stringify(envKey)}],
           overlay: process.env.FLEETDECK_REFERENCE_OVERLAY,
           input: Buffer.concat(chunks).toString('hex'),
         })));`,
      ],
      cwd: realpathSync(cwd),
      env: { FLEETDECK_REFERENCE_OVERLAY: 'overlay-value' },
      stdin: new Uint8Array([0, 1, 0xfe, 0xff]),
      timeoutMs: 5_000,
    };
    const execution = driver.start(request);
    const decision = await within(execution.decision, 'byte stdin decision');
    await within(execution.cleanup, 'byte stdin cleanup');
    assert.equal(decision.ok, true);
    if (!decision.ok) return;
    assert.deepEqual(JSON.parse(decision.out), {
      cwd: realpathSync(cwd),
      inherited: 'live-parent-value',
      overlay: 'overlay-value',
      input: '0001feff',
    });

    const stringExecution = driver.start({
      argv: [
        process.execPath,
        '-e',
        `process.stdin.setEncoding('utf8'); let value = ''; process.stdin.on('data', (chunk) => value += chunk); process.stdin.on('end', () => process.stdout.write(value));`,
      ],
      stdin: 'héllo',
      timeoutMs: 5_000,
    });
    assert.deepEqual(await stringExecution.decision, { ok: true, out: 'héllo' });
    await within(stringExecution.cleanup, 'string stdin cleanup');
  } finally {
    if (previous === undefined) delete process.env[envKey];
    else process.env[envKey] = previous;
  }
});

test('Node reference differentially preserves pre-abort, in-flight abort, and cleanup-once', async (t) => {
  const driver = makeNodeProcessDriverReference();
  t.after(() => driver.close());

  const legacyPreController = new AbortController();
  legacyPreController.abort();
  const legacyTracked = trackedSignal(legacyPreController);
  const referencePreController = new AbortController();
  referencePreController.abort();
  const referenceTracked = trackedSignal(referencePreController);
  const legacyPre = await legacyRun(process.execPath, ['-e', 'setTimeout(() => {}, 500);'], {
    timeout: 5_000,
    signal: legacyTracked.signal,
  });
  const referencePreExecution = referenceStart(
    driver,
    process.execPath,
    ['-e', 'setTimeout(() => {}, 500);'],
    { timeout: 5_000, signal: referenceTracked.signal },
  );
  const referencePre = await referencePreExecution.decision;
  await within(referencePreExecution.cleanup, 'pre-abort cleanup');
  assert.deepEqual(referencePre, legacyPre);
  assert.deepEqual(referenceTracked.metrics, legacyTracked.metrics);
  assert.deepEqual(referenceTracked.metrics, { added: 0, removed: 1 });

  const legacyController = new AbortController();
  const referenceController = new AbortController();
  const legacy = legacyRun(process.execPath, ['-e', 'setTimeout(() => {}, 1_000);'], {
    timeout: 5_000,
    signal: legacyController.signal,
  });
  const referenceExecution = referenceStart(
    driver,
    process.execPath,
    ['-e', 'setTimeout(() => {}, 1_000);'],
    { timeout: 5_000, signal: referenceController.signal },
  );
  setTimeout(() => {
    legacyController.abort();
    referenceController.abort();
  }, 20);
  assert.deepEqual(await referenceExecution.decision, await legacy);
  assert.deepEqual(await referenceExecution.decision, CANCELLED_RESULT);
  await within(referenceExecution.cleanup, 'in-flight abort cleanup');

  const onceController = new AbortController();
  const onceTracked = trackedSignal(onceController);
  const immediate = referenceStart(driver, process.execPath, ['-e', 'process.exit(0)'], {
    timeout: 2_000,
    signal: onceTracked.signal,
  });
  assert.deepEqual(await immediate.decision, { ok: true, out: '' });
  await immediate.cleanup;
  onceController.abort();
  immediate.cancel();
  await pause(20);
  assert.deepEqual(onceTracked.metrics, { added: 1, removed: 1 });
});

test('Node reference separates timeout decision from TERM/KILL reap completion', async (t) => {
  const driver = makeNodeProcessDriverReference();
  t.after(() => driver.close());
  const fixture = pidFixture(t, 'fleetdeck-reference-term-');
  const timeoutMs = 250;
  const started = Date.now();
  const execution = driver.start({
    argv: [
      process.execPath,
      '-e',
      `const fs = require('node:fs');
       fs.writeFileSync(${JSON.stringify(fixture.pidFile)}, String(process.pid));
       process.on('SIGTERM', () => {});
       setTimeout(() => {}, 4_000);`,
    ],
    timeoutMs,
  });

  assert.deepEqual(await execution.decision, {
    ok: false,
    code: 'ETIMEDOUT',
    err: `timed out after ${String(timeoutMs)}ms`,
  });
  assert.ok(Date.now() - started < 1_000, 'decision owns the wall-clock timeout');
  const pid = fixture.readPid();
  assert.ok(pid !== null, 'fixture published its pid before the timeout');
  assert.equal(pidAlive(pid), true, 'TERM-resistant child remains during the grace period');
  await within(execution.cleanup, 'TERM/KILL cleanup', 3_000);
  assert.equal(pidAlive(pid), false, 'cleanup resolves only after the child is reaped');
});

test('Node reference cleanup closes inherited pipes without claiming an unowned descendant', async (t) => {
  const driver = makeNodeProcessDriverReference();
  t.after(() => driver.close());
  const fixture = pidFixture(t, 'fleetdeck-reference-open-pipe-');
  // Give a concurrently-loaded Node trust-anchor worker enough time to spawn
  // and publish the detached holder before the timeout path is selected.
  const timeoutMs = 1_000;
  const execution = driver.start({
    argv: [
      process.execPath,
      '-e',
      `const { spawn } = require('node:child_process');
       const fs = require('node:fs');
       const holder = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 4_000);'], {
         stdio: ['ignore', 1, 2],
         detached: true,
       });
       fs.writeFileSync(${JSON.stringify(fixture.pidFile)}, String(holder.pid));
       holder.unref();`,
    ],
    timeoutMs,
  });

  assert.deepEqual(await execution.decision, {
    ok: false,
    code: 'ETIMEDOUT',
    err: `timed out after ${String(timeoutMs)}ms`,
  });
  const holderPid = fixture.readPid();
  assert.ok(holderPid !== null, 'fixture published the inherited-pipe holder');
  assert.equal(pidAlive(holderPid), true);
  await within(execution.cleanup, 'inherited-pipe cleanup', 3_000);
  assert.equal(
    pidAlive(holderPid),
    true,
    'killTree=false closes owned descriptors but does not claim an independently detached process',
  );
  forceKill(holderPid);
  await waitUntil(() => !pidAlive(holderPid), {
    timeoutMs: 2_000,
    intervalMs: 10,
    label: 'inherited-pipe fixture cleanup',
  });
});

test('Node reference close is idempotent, joins a process group, and refuses later starts', async (t) => {
  if (process.platform === 'win32') return;
  const driver = makeNodeProcessDriverReference();
  t.after(() => driver.close());
  const fixture = pidFixture(t, 'fleetdeck-reference-group-');
  const execution = driver.start({
    argv: [
      process.execPath,
      '-e',
      `const { spawn } = require('node:child_process');
       const fs = require('node:fs');
       const helper = spawn(process.execPath, ['-e', 'process.on("SIGTERM", () => {}); setTimeout(() => {}, 4_000)'], { stdio: ['ignore', 1, 2] });
       fs.writeFileSync(${JSON.stringify(fixture.pidFile)}, String(helper.pid));
       process.on('SIGTERM', () => {});
       setTimeout(() => {}, 4_000);`,
    ],
    timeoutMs: 10_000,
    killTree: true,
  });
  await waitUntil(() => existsSync(fixture.pidFile), {
    timeoutMs: 2_000,
    intervalMs: 10,
    label: 'process-group helper pid',
  });
  const helperPid = fixture.readPid();
  assert.ok(helperPid !== null);

  const firstClose = driver.close();
  const secondClose = driver.close();
  assert.equal(firstClose, secondClose, 'concurrent close callers share one Promise');
  assert.deepEqual(await execution.decision, CANCELLED_RESULT);
  await within(firstClose, 'driver close process-group join', 3_000);
  assert.equal(pidAlive(helperPid), false, 'close joins the TERM/KILL process-group cleanup');

  const refused = driver.start({ argv: [process.execPath, '-e', 'process.exit(99)'] });
  assert.deepEqual(await refused.decision, CANCELLED_RESULT);
  await refused.cleanup;
  refused.cancel();
});

test('Effect service interruption awaits cleanup and Layer release closes its acquired driver', async () => {
  let resolveStarted!: () => void;
  const started = new Promise<void>((resolve) => {
    resolveStarted = resolve;
  });
  let resolveDecision!: (decision: ProcessResult) => void;
  const decision = new Promise<ProcessResult>((resolve) => {
    resolveDecision = resolve;
  });
  let resolveCleanup!: () => void;
  const cleanup = new Promise<void>((resolve) => {
    resolveCleanup = resolve;
  });
  let cancelCalls = 0;
  let cleanupFinished = false;
  const execution: ProcessExecution = {
    decision,
    cleanup,
    cancel() {
      cancelCalls += 1;
      resolveDecision(CANCELLED_RESULT);
      setTimeout(() => {
        cleanupFinished = true;
        resolveCleanup();
      }, 25);
    },
  };
  const driver: ProcessDriver = {
    start() {
      resolveStarted();
      return execution;
    },
    startBounded() {
      throw new Error('bounded execution is outside this interruption fixture');
    },
    close() {
      return Promise.resolve();
    },
  };
  const service = makeProcessRunnerServiceFromDriver(driver);
  const interrupted = await runEffectExit(
    Effect.gen(function* () {
      const fiber = yield* service.run({ argv: ['fixture'] }).pipe(Effect.forkChild);
      yield* Effect.promise(() => started);
      yield* Fiber.interrupt(fiber);
      return yield* Fiber.await(fiber);
    }),
  );
  assert.ok(Exit.isSuccess(interrupted));
  assert.equal(Exit.isFailure(interrupted.value), true);
  if (Exit.isFailure(interrupted.value)) {
    assert.equal(Cause.hasInterruptsOnly(interrupted.value.cause), true);
  }
  assert.equal(cancelCalls, 1);
  assert.equal(cleanupFinished, true, 'Fiber.interrupt waits for callback cleanup');

  let acquisitions = 0;
  let closeCalls = 0;
  const layerDriver: ProcessDriver = {
    start() {
      return {
        decision: Promise.resolve({ ok: true, out: 'layer-driver' }),
        cleanup: Promise.resolve(),
        cancel() {},
      };
    },
    startBounded() {
      throw new Error('bounded execution is outside this Layer fixture');
    },
    close() {
      closeCalls += 1;
      return Promise.resolve();
    },
  };
  const layer = makeProcessRunnerLayerFromDriver(
    Effect.sync(() => {
      acquisitions += 1;
      return layerDriver;
    }),
  );
  const layerExit = await runEffectExit(
    Effect.provide(
      Effect.gen(function* () {
        const runner = yield* ProcessRunner;
        return yield* runner.run({ argv: ['fixture'] });
      }),
      layer,
    ),
  );
  assert.ok(Exit.isSuccess(layerExit));
  assert.deepEqual(layerExit.value, { ok: true, out: 'layer-driver' });
  assert.equal(acquisitions, 1);
  assert.equal(closeCalls, 1, 'Layer acquireRelease closes the acquired driver once');
});
