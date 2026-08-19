// tests/exec-timeout.test.ts
//
// Legacy parity matrix for the shared subprocess boundary. These assertions
// intentionally describe execFileP's public result shapes, byte accounting,
// cancellation races, and cleanup before P3 swaps its Node driver for Bun.spawn.

import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import type { TestContext } from 'node:test';

import { execFileP, type ExecResult } from '../src/daemon/exec.ts';
import test from './helpers/harness-test.ts';
import { waitUntil } from './helpers/wait.ts';

const MAX_OUTPUT_BYTES = 1024 * 1024;
const OUTPUT_LIMIT_RESULT: ExecResult = {
  ok: false,
  code: 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER',
  err: `subprocess output exceeded ${MAX_OUTPUT_BYTES} bytes`,
};

function pause(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
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
          label: `${prefix} process cleanup`,
        });
      } catch {
        /* the assertion in the owning test reports residue; cleanup is best effort */
      }
    }
    rmSync(dir, { recursive: true, force: true });
  });
  return { pidFile, readPid };
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
      metrics.added++;
      source.addEventListener(...args);
    },
    removeEventListener(...args: Parameters<AbortSignal['removeEventListener']>) {
      metrics.removed++;
      source.removeEventListener(...args);
    },
  } as AbortSignal;
  return { signal, metrics };
}

test('execFileP resolves a normal command with its stdout', async () => {
  const res = await execFileP(process.execPath, ['-e', 'process.stdout.write("hello")'], {
    timeout: 5_000,
  });
  assert.deepEqual(res, { ok: true, out: 'hello' });
});

test('execFileP reports a missing executable through its error event', async () => {
  const missing = path.join(
    tmpdir(),
    `fleetdeck-definitely-missing-${String(process.pid)}-${String(Date.now())}`,
  );
  assert.equal(existsSync(missing), false);
  const res = await execFileP(missing, [], { timeout: 2_000 });
  assert.equal(res.ok, false);
  assert.equal(res.code, 'ENOENT');
  assert.match(res.err, /ENOENT|no such file/i);
});

test('execFileP converts a synchronous spawn validation failure to its legacy shape', async () => {
  const res = await execFileP('invalid\0command', [], { timeout: 2_000 });
  assert.equal(res.ok, false);
  assert.equal(Object.hasOwn(res, 'code'), false, 'sync throws do not publish an exit code');
  assert.match(res.err, /null byte/i);
});

test('execFileP returns trimmed stderr and the numeric code for a non-zero exit', async () => {
  const res = await execFileP(
    process.execPath,
    [
      '-e',
      `const fs = require('node:fs');
       fs.writeSync(1, 'stdout is not the failure diagnostic');
       fs.writeSync(2, '  fatal: fixture failed  \\n');
       process.exit(7);`,
    ],
    { timeout: 5_000 },
  );
  assert.deepEqual(res, { ok: false, code: 7, err: 'fatal: fixture failed' });
});

test('the combined stdout/stderr cap accepts exactly 1 MiB', async () => {
  const stdoutBytes = 640 * 1024;
  const stderrBytes = MAX_OUTPUT_BYTES - stdoutBytes;
  const res = await execFileP(
    process.execPath,
    [
      '-e',
      `const fs = require('node:fs');
       fs.writeSync(1, Buffer.alloc(${String(stdoutBytes)}, 0x61));
       fs.writeSync(2, Buffer.alloc(${String(stderrBytes)}, 0x62));`,
    ],
    { timeout: 5_000 },
  );
  assert.equal(res.ok, true);
  if (!res.ok) return;
  assert.equal(res.out.length, stdoutBytes);
  assert.equal(res.out[0], 'a');
  assert.equal(res.out.at(-1), 'a');
});

test('the combined stdout/stderr cap rejects exactly 1 MiB plus one byte', async () => {
  const stdoutBytes = 640 * 1024;
  const stderrBytes = MAX_OUTPUT_BYTES + 1 - stdoutBytes;
  const res = await execFileP(
    process.execPath,
    [
      '-e',
      `const fs = require('node:fs');
       fs.writeSync(1, Buffer.alloc(${String(stdoutBytes)}, 0x61));
       fs.writeSync(2, Buffer.alloc(${String(stderrBytes)}, 0x62));`,
    ],
    { timeout: 5_000 },
  );
  assert.deepEqual(res, OUTPUT_LIMIT_RESULT);
});

test('fragmented valid UTF-8 is decoded only after exact-cap byte accumulation', async () => {
  const asciiBytes = MAX_OUTPUT_BYTES - 4;
  const res = await execFileP(
    process.execPath,
    [
      '-e',
      `const fs = require('node:fs');
       fs.writeSync(1, Buffer.alloc(${String(asciiBytes)}, 0x61));
       fs.writeSync(1, Buffer.from([0xf0]));
       setTimeout(() => {
         fs.writeSync(1, Buffer.from([0x9f, 0x92, 0xa9]));
       }, 10);`,
    ],
    { timeout: 5_000 },
  );
  assert.equal(res.ok, true);
  if (!res.ok) return;
  assert.equal(Buffer.byteLength(res.out, 'utf8'), MAX_OUTPUT_BYTES);
  assert.equal(res.out.length, asciiBytes + 2, 'the four UTF-8 bytes decode to one surrogate pair');
  assert.equal(res.out.endsWith('💩'), true);
});

test('an incomplete UTF-8 sequence at the exact byte cap uses Buffer replacement semantics', async () => {
  const asciiBytes = MAX_OUTPUT_BYTES - 1;
  const res = await execFileP(
    process.execPath,
    [
      '-e',
      `const fs = require('node:fs');
       fs.writeSync(1, Buffer.alloc(${String(asciiBytes)}, 0x61));
       fs.writeSync(1, Buffer.from([0xc3]));`,
    ],
    { timeout: 5_000 },
  );
  assert.equal(res.ok, true);
  if (!res.ok) return;
  assert.equal(res.out.length, MAX_OUTPUT_BYTES);
  assert.equal(res.out.endsWith('\uFFFD'), true);
  assert.equal(
    Buffer.byteLength(res.out, 'utf8'),
    MAX_OUTPUT_BYTES + 2,
    'the cap counts captured bytes, not bytes after replacement-character decoding',
  );
});

test('a pre-aborted signal settles as cancellation and never arms an abort listener', async () => {
  const controller = new AbortController();
  controller.abort();
  const { signal, metrics } = trackedSignal(controller);
  const started = Date.now();
  const res = await execFileP(process.execPath, ['-e', 'setTimeout(() => {}, 500);'], {
    timeout: 5_000,
    signal,
  });
  assert.deepEqual(res, { ok: false, code: 'ECANCELED', err: 'cancelled' });
  assert.ok(Date.now() - started < 1_000, 'pre-abort must own wall-clock settlement');
  assert.deepEqual(metrics, { added: 0, removed: 1 });
});

test('execFileP passes live process.env mutations to the child', async () => {
  const key = `FLEETDECK_EXEC_LIVE_ENV_${String(process.pid)}_${String(Date.now())}`;
  const value = 'visible-after-runtime-mutation';
  const previous = process.env[key];
  try {
    process.env[key] = value;
    const res = await execFileP(
      process.execPath,
      ['-e', `process.stdout.write(process.env[${JSON.stringify(key)}] || '')`],
      { timeout: 5_000 },
    );
    assert.deepEqual(res, { ok: true, out: value });
  } finally {
    if (previous === undefined) delete process.env[key];
    else process.env[key] = previous;
  }
});

test('immediate exits settle reliably across the handle-publication boundary', async () => {
  // Node publishes ChildProcess before close/error events. The same black-box
  // case is the regression tripwire for Bun.spawn's onExit callback potentially
  // winning before the future driver stores its returned subprocess handle.
  const results = await Promise.all(
    Array.from({ length: 24 }, () =>
      execFileP(process.execPath, ['-e', 'process.exit(0)'], { timeout: 5_000 }),
    ),
  );
  for (const result of results) assert.deepEqual(result, { ok: true, out: '' });
});

test('exit, abort, timeout, and output-limit races each clean settlement exactly once', async () => {
  // Promise resolution alone cannot prove internal exactly-once cleanup. Every
  // terminal path removes the registered abort listener from this instrumented
  // signal; a late competing event must not remove it a second time.
  {
    const controller = new AbortController();
    const { signal, metrics } = trackedSignal(controller);
    const res = await execFileP(process.execPath, ['-e', 'process.exit(0)'], {
      timeout: 2_000,
      signal,
    });
    controller.abort(); // a late abort races the already-observed exit
    await pause(20);
    assert.deepEqual(res, { ok: true, out: '' });
    assert.deepEqual(metrics, { added: 1, removed: 1 }, 'exit cleanup');
  }

  {
    const controller = new AbortController();
    const { signal, metrics } = trackedSignal(controller);
    const run = execFileP(process.execPath, ['-e', 'setTimeout(() => {}, 500);'], {
      timeout: 2_000,
      signal,
    });
    setTimeout(() => controller.abort(), 10);
    const res = await run;
    await pause(30); // allow the losing close/error event to arrive
    assert.deepEqual(res, { ok: false, code: 'ECANCELED', err: 'cancelled' });
    assert.deepEqual(metrics, { added: 1, removed: 1 }, 'abort cleanup');
  }

  {
    const controller = new AbortController();
    const { signal, metrics } = trackedSignal(controller);
    const timeout = 25;
    const res = await execFileP(process.execPath, ['-e', 'setTimeout(() => {}, 500);'], {
      timeout,
      signal,
    });
    controller.abort(); // a late abort races timeout-triggered termination/close
    await pause(30);
    assert.deepEqual(res, {
      ok: false,
      code: 'ETIMEDOUT',
      err: `timed out after ${String(timeout)}ms`,
    });
    assert.deepEqual(metrics, { added: 1, removed: 1 }, 'timeout cleanup');
  }

  {
    const controller = new AbortController();
    const { signal, metrics } = trackedSignal(controller);
    const res = await execFileP(
      process.execPath,
      [
        '-e',
        `require('node:fs').writeSync(1, Buffer.alloc(${String(MAX_OUTPUT_BYTES + 1)}, 0x78));`,
      ],
      { timeout: 5_000, signal },
    );
    controller.abort(); // a late abort races the output-limit termination/close
    await pause(30);
    assert.deepEqual(res, OUTPUT_LIMIT_RESULT);
    assert.deepEqual(metrics, { added: 1, removed: 1 }, 'output-limit cleanup');
  }
});

test('execFileP settles at the deadline and reaps a child that ignores SIGTERM', async (t) => {
  const fixture = pidFixture(t, 'fleetdeck-exec-term-');
  const timeout = 300;
  const started = Date.now();
  const res = await execFileP(
    process.execPath,
    [
      '-e',
      `const fs = require('node:fs');
       fs.writeFileSync(${JSON.stringify(fixture.pidFile)}, String(process.pid));
       process.on('SIGTERM', () => {});
       setTimeout(() => {}, 4_000);`,
    ],
    { timeout },
  );
  const elapsed = Date.now() - started;
  assert.deepEqual(res, {
    ok: false,
    code: 'ETIMEDOUT',
    err: `timed out after ${String(timeout)}ms`,
  });
  assert.ok(
    elapsed < 2_000,
    `the ${String(timeout)}ms deadline settled after ${String(elapsed)}ms`,
  );
  const pid = fixture.readPid();
  assert.ok(pid !== null, 'the TERM-resistant fixture published its pid');
  await waitUntil(() => !pidAlive(pid), {
    timeoutMs: 3_000,
    intervalMs: 10,
    label: 'SIGKILL reaps the TERM-resistant child',
  });
});

test('execFileP settles at the deadline while a grandchild holds an inherited pipe open', async (t) => {
  const fixture = pidFixture(t, 'fleetdeck-exec-open-pipe-');
  const started = Date.now();
  const res = await execFileP(
    process.execPath,
    [
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
    { timeout: 300 },
  );
  const elapsed = Date.now() - started;
  assert.deepEqual(res, { ok: false, code: 'ETIMEDOUT', err: 'timed out after 300ms' });
  assert.ok(elapsed < 2_000, `open pipes held settlement for ${String(elapsed)}ms`);
  const holderPid = fixture.readPid();
  assert.ok(holderPid !== null, 'the inherited-pipe fixture published its descendant pid');
  assert.equal(pidAlive(holderPid), true, 'the descendant, not process exit, held the pipes open');
  forceKill(holderPid);
  await waitUntil(() => !pidAlive(holderPid), {
    timeoutMs: 2_000,
    intervalMs: 10,
    label: 'open-pipe descendant cleanup',
  });
});

test('execFileP aborts an in-flight process group instead of leaving its helper alive', async (t) => {
  if (process.platform === 'win32') return;
  const fixture = pidFixture(t, 'fleetdeck-exec-group-');
  const controller = new AbortController();
  let run: Promise<ExecResult> | null = null;
  t.after(async () => {
    controller.abort();
    await run;
  });
  const started = Date.now();
  run = execFileP(
    process.execPath,
    [
      '-e',
      `const { spawn } = require('node:child_process');
       const fs = require('node:fs');
       const helper = spawn(process.execPath, ['-e', 'process.on("SIGTERM", () => {}); setTimeout(() => {}, 4_000)'], { stdio: ['ignore', 1, 2] });
       fs.writeFileSync(${JSON.stringify(fixture.pidFile)}, String(helper.pid));
       setTimeout(() => {}, 4_000);`,
    ],
    { timeout: 10_000, signal: controller.signal, killTree: true },
  );
  await waitUntil(() => existsSync(fixture.pidFile), {
    timeoutMs: 2_000,
    intervalMs: 10,
    label: 'helper pid file',
  });
  const helperPid = fixture.readPid();
  assert.ok(helperPid !== null, 'fixture records the helper pid');
  controller.abort();
  const result = await run;
  assert.deepEqual(result, { ok: false, code: 'ECANCELED', err: 'cancelled' });
  assert.ok(Date.now() - started < 2_000, 'abort owns wall-clock settlement');
  await waitUntil(() => !pidAlive(helperPid), {
    timeoutMs: 3_000,
    intervalMs: 10,
    label: 'SIGKILL reaps the TERM-immune helper after its leader exits',
  });
});
