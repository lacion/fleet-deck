// tests/exec-timeout.test.ts
//
// BUG-026 regression: execFileP's advertised timeout must be a WALL-CLOCK
// deadline, not a hope. execFile's own timeout only SIGTERMs the child — the
// callback still waits for the child to actually exit AND its stdio pipes to
// close, so a child that ignores TERM (or that leaves a grandchild holding an
// inherited stdout/stderr pipe open) keeps the promise pending past the
// deadline — for the daemon's lifetime in agents-poll, silently wedging
// discovery and pane liveness. The wrapper now owns settlement: deadline
// fires → SIGTERM → SIGKILL after a short grace → resolve exactly once with
// { ok: false, code: 'ETIMEDOUT' }.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFileP } from '../src/daemon/exec.ts';
import { waitUntil } from './helpers/wait.ts';

function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

test('execFileP settles on a child that ignores SIGTERM', async () => {
  // A TERM-immune child is BUG-026's exact daemon scenario: the old
  // execFile-only pattern's promise stayed pending until the child chose to
  // exit — forever, in agents-poll. This fixture self-exits after 3s so a
  // never-killed child cannot hold the test RUNNER's own stdio open past the
  // run; the timing assertions below pin settlement to the deadline.
  const res = await execFileP(
    process.execPath,
    ['-e', 'process.on("SIGTERM", () => {}); setTimeout(() => {}, 3_000);'],
    { timeout: 300 },
  );
  assert.equal(res.ok, false, 'a TERM-immune child must still settle as a failure');
  assert.equal(res.code, 'ETIMEDOUT', 'the attempt must be marked timed out');
});

test('execFileP resolves a normal command with its stdout', async () => {
  const res = await execFileP(process.execPath, ['-e', 'process.stdout.write("hello")'], {
    timeout: 5_000,
  });
  assert.deepEqual(res, { ok: true, out: 'hello' });
});

test('execFileP settles at the deadline on a child that ignores SIGTERM', async () => {
  const started = Date.now();
  const res = await execFileP(
    process.execPath,
    [
      // Self-exits after 3s so nothing outlives the test process; without the
      // fix the promise settles only then (~3s), with it at ~300ms.
      '-e',
      'process.on("SIGTERM", () => {}); setTimeout(() => {}, 3_000);',
    ],
    { timeout: 300 },
  );
  const elapsed = Date.now() - started;
  assert.equal(res.ok, false, 'a TERM-immune child must still settle as a failure');
  assert.equal(res.code, 'ETIMEDOUT', 'the attempt must be marked timed out');
  assert.ok(
    elapsed < 2_000,
    `settlement took ${elapsed}ms; the deadline must bound it (300ms + 1s SIGKILL grace), not wait out the child`,
  );
});

test('execFileP settles at the deadline while a grandchild holds an inherited pipe open', async () => {
  // The direct child exits INSTANTLY, but not before spawning a detached
  // grandchild that inherits its stdout/stderr. execFile's callback waits for
  // the pipes — which the grandchild keeps open — so the old pattern settled
  // only when the grandchild exited. The grandchild self-exits after 3s (it
  // is detached and was never killed) so nothing outlives the test process.
  const started = Date.now();
  const res = await execFileP(
    process.execPath,
    [
      '-e',
      `const { spawn } = require('node:child_process');
     spawn(process.execPath, ['-e', 'setTimeout(() => {}, 3_000);'], { stdio: ['ignore', 1, 2], detached: true }).unref();`,
    ],
    { timeout: 300 },
  );
  const elapsed = Date.now() - started;
  assert.equal(res.ok, false, 'an open inherited pipe must not hold the attempt hostage');
  assert.equal(res.code, 'ETIMEDOUT', 'the attempt must be marked timed out');
  assert.ok(
    elapsed < 2_000,
    `settlement took ${elapsed}ms; the deadline must bound it (300ms + 1s SIGKILL grace), not wait for the pipes`,
  );
});

test('execFileP aborts a cancellable process group instead of leaving its helper alive', async (t) => {
  if (process.platform === 'win32') return;
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-exec-group-'));
  const pidFile = path.join(dir, 'helper.pid');
  t.after(() => {
    rmSync(dir, { recursive: true, force: true });
  });
  const controller = new AbortController();
  const started = Date.now();
  const run = execFileP(
    process.execPath,
    [
      '-e',
      `const { spawn } = require('node:child_process');
       const fs = require('node:fs');
       const helper = spawn(process.execPath, ['-e', 'process.on("SIGTERM", () => {}); setTimeout(() => {}, 10_000)'], { stdio: ['ignore', 1, 2] });
       fs.writeFileSync(${JSON.stringify(pidFile)}, String(helper.pid));
       setTimeout(() => {}, 10_000);`,
    ],
    { timeout: 10_000, signal: controller.signal, killTree: true },
  );
  await waitUntil(() => existsSync(pidFile), { timeoutMs: 2_000, label: 'helper pid file' });
  const helperPid = Number(readFileSync(pidFile, 'utf8'));
  assert.ok(Number.isInteger(helperPid) && helperPid > 1, 'fixture records the helper pid');
  controller.abort();
  const result = await run;
  assert.deepEqual(result, { ok: false, code: 'ECANCELED', err: 'cancelled' });
  assert.ok(Date.now() - started < 2_000, 'abort owns the wall-clock settlement');
  await waitUntil(() => !pidAlive(helperPid), {
    timeoutMs: 3_000,
    label: 'SIGKILL reaps the TERM-immune helper after its leader exits',
  });
});
