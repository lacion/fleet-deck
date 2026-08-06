// tests/smoke-launcher.test.mjs
//
// BUG-187: `npm run smoke` hard-required GNU `timeout` and util-linux
// `setsid`, neither of which ships with base macOS, so the Phase 1 acceptance
// gate was unrunnable outside Linux. demo/run-with-timeout.mjs is the
// portable replacement: same process-group launch (setsid), same deadline
// semantics (exit 124), and it must forward SIGTERM into the child's detached
// process group because the smoke's stop_worker kills the launcher's own
// group and the child is deliberately outside it.

import test from 'node:test';
import assert from 'node:assert/strict';
import { execFile, execFileSync, spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const LAUNCHER = path.join(REPO_ROOT, 'demo', 'run-with-timeout.mjs');
const SMOKE_SCRIPT = path.join(REPO_ROOT, 'demo', 'run-smoke.sh');

const pidAlive = (pid) => {
  try { process.kill(pid, 0); return true; }
  catch (err) { return err?.code !== 'ESRCH'; }
};

test('smoke launches its workers through the portable Node launcher, never GNU timeout/setsid', () => {
  const source = readFileSync(SMOKE_SCRIPT, 'utf8');
  const usesTimeoutLauncher = source.includes('run-with-timeout.mjs');
  assert.ok(usesTimeoutLauncher, 'run-smoke.sh must launch workers via demo/run-with-timeout.mjs (macOS ships neither GNU timeout nor setsid)');
  if (!usesTimeoutLauncher) return;
  assert.equal(/^\s*setsid\s/m.test(source), false, 'run-smoke.sh must not invoke setsid (util-linux, Linux-only)');
  assert.equal(/^\s*timeout\s/m.test(source), false, 'run-smoke.sh must not invoke GNU timeout as a command');
  // And the abort gate that rejected machines without them must be gone.
  assert.equal(/requires (timeout|setsid) on PATH/.test(source), false, 'smoke preflight must not hard-require Linux-only utilities');
});

test('launcher runs the command in its own process group (the setsid half)', async (t) => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-launcher-'));
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const out = path.join(dir, 'pgid.json');

  const result = await new Promise((resolve, reject) => {
    const launched = execFile(process.execPath, [LAUNCHER, '10', process.execPath, '-e',
      `require("node:fs").writeFileSync(${JSON.stringify(out)}, JSON.stringify({ pid: process.pid, ppid: process.ppid })); setTimeout(() => {}, 2000)`],
      (error, stdout, stderr) => {
        clearInterval(poll);
        resolve({ code: error ? error.code : 0, stderr, record: launched.record, pgid: launched.pgid });
      });
    // Read the record while the child is still alive so its process group can
    // be inspected with ps.
    const poll = setInterval(() => {
      if (!existsSync(out)) return;
      clearInterval(poll);
      launched.record = JSON.parse(readFileSync(out, 'utf8'));
      launched.pgid = Number(execFileSync('ps', ['-o', 'pgid=', '-p', String(launched.record.pid)], { encoding: 'utf8' }).trim());
    }, 25);
  });

  assert.equal(result.code, 0, `launcher should relay the child's exit status, stderr: ${result.stderr}`);
  assert.ok(result.record, 'timed out waiting for the launched child to report itself');
  // detached:true makes the child lead its own process group, the portable
  // equivalent of wrapping the command in setsid.
  assert.equal(result.pgid, result.record.pid, 'child must lead its own process group so the smoke can signal the whole tree (setsid semantics)');
});

test('launcher exits 124 and kills a command that outlives its deadline (GNU timeout semantics)', () => {
  assert.throws(
    () => execFileSync(process.execPath, [LAUNCHER, '1', process.execPath, '-e', 'setInterval(() => {}, 1000)'], { stdio: 'pipe' }),
    (error) => error.status === 124,
    'a timed-out command must exit 124 exactly like GNU timeout (the smoke accepts rc=124)',
  );
});

test('launcher forwards SIGTERM into the child process group so stop_worker cannot orphan a worker', async (t) => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-launcher-'));
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const victimPidFile = path.join(dir, 'victim.pid');
  const child = spawn(process.execPath, [LAUNCHER, '300', process.execPath, '-e',
    `require("node:fs").writeFileSync(${JSON.stringify(victimPidFile)}, String(process.pid)); setInterval(() => {}, 1000)`],
    { stdio: 'ignore' });
  t.after(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} });

  let victimPid = null;
  for (let i = 0; i < 100 && victimPid === null; i += 1) {
    if (existsSync(victimPidFile)) victimPid = Number(readFileSync(victimPidFile, 'utf8'));
    else await new Promise(resolve => setTimeout(resolve, 50));
  }
  assert.ok(victimPid > 0, 'timed out waiting for the launched child to start');
  assert.ok(pidAlive(victimPid), 'sanity: launched child should be alive before the signal');

  // This is exactly what the smoke's stop_worker does: signal the launcher
  // process (its own group), never the detached worker's group.
  process.kill(child.pid, 'SIGTERM');

  const exited = await new Promise(resolve => child.once('exit', () => resolve(true)));
  assert.ok(exited, 'launcher should exit after SIGTERM');
  for (let i = 0; i < 100 && pidAlive(victimPid); i += 1) {
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  assert.equal(pidAlive(victimPid), false, 'SIGTERM to the launcher must reach the detached worker process group');
});
