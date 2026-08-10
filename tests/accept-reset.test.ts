// tests/accept-reset.test.ts
//
// BUG-008 — the demo acceptance reset used to run `kill "$(cat fleetd.pid)"`
// on whatever bytes the pidfile held. A legacy plain-PID pidfile surviving a
// crash/reboot can name a PID the OS has since recycled for an unrelated
// process, and the acceptance gate would SIGTERM that stranger mid-work.
//
// The fix is demo/lib/kill-verified-daemon.sh: stop_pidfile_daemon only
// signals a pid whose fleetd identity is proven — strict pid record, the
// /health pid matching the pidfile when a port is recorded, and the
// production verifyDaemonPid gate (pidfile match + fleetd /proc shape) from
// scripts/fleetd/takeover.mjs. Legacy plain-PID records can never be
// positively identified, so they are treated as unowned and never signalled.
//
// These tests source the helper in a real bash subprocess, exactly as
// demo/run-accept-phase3.sh / run-accept-plan.sh / run-accept-spawn.sh do,
// against scratch homes under the OS tmpdir. No port 4711, no real
// ~/.fleetdeck, and every spawned process is reaped in t.after.

import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon, randomPort, REPO_ROOT } from './helpers/daemon.ts';
import { waitUntil } from './helpers/wait.ts';

const HELPER = path.join(REPO_ROOT, 'demo/lib/kill-verified-daemon.sh');

// The whole suite pins the fixed helper: without it there is no verified-stop
// contract to test (and pre-fix, sourcing the missing file exits 127).
assert.ok(
  existsSync(HELPER),
  'demo/lib/kill-verified-daemon.sh must exist — the acceptance reset sources it for identity-bound daemon stops (BUG-008)',
);

interface StopResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

function scratchHome(t: TestContext): string {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-accept-reset-'));
  t.after(() => {
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return home;
}

// Run stop_pidfile_daemon against `home` in a bash subprocess (the helper is
// bash, sourced by the demo scripts). FLEETDECK_ROOT must point at the repo
// so the helper can import the production takeover gate.
function runStop(home: string, waitSeconds = 2): Promise<StopResult> {
  return new Promise<StopResult>((resolve, reject) => {
    const child = spawn(
      'bash',
      [
        '-c',
        `FLEETDECK_ROOT=${JSON.stringify(REPO_ROOT)}; . ${JSON.stringify(HELPER)}; stop_pidfile_daemon "$1" "$2"`,
        'stop',
        home,
        String(waitSeconds),
      ],
      { stdio: ['ignore', 'pipe', 'pipe'] },
    );
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d: Buffer) => {
      stdout += d.toString();
    });
    child.stderr.on('data', (d: Buffer) => {
      stderr += d.toString();
    });
    child.once('error', reject);
    child.once('exit', (code) => {
      resolve({ code, stdout, stderr });
    });
  });
}

function isAlive(pid: number | undefined): boolean {
  if (pid == null) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

// A live node process that is NOT fleetd-shaped: the stand-in for an
// unrelated application whose PID a stale pidfile happens to name.
function spawnSleeper(t: TestContext): ChildProcess {
  const sleeper = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1e9)'], {
    stdio: 'ignore',
  });
  t.after(() => {
    try {
      sleeper.kill('SIGKILL');
    } catch {
      /* gone */
    }
  });
  return sleeper;
}

test('acceptance reset never signals a recycled legacy plain-PID pidfile entry (BUG-008)', async (t: TestContext) => {
  const home = scratchHome(t);
  const sleeper = spawnSleeper(t);
  await waitUntil(() => sleeper.pid != null, { label: 'sleeper pid' });

  // The pre-port fleetd.pid format: bare pid text, no JSON, no port. This is
  // the exact record the old `kill "$(cat pidfile)"` would have signalled.
  writeFileSync(path.join(home, 'fleetd.pid'), `${String(sleeper.pid)}\n`);

  const result = await runStop(home);
  assert.equal(
    result.code,
    1,
    `an unverifiable live pid must be reported, not signalled (stderr: ${result.stderr})`,
  );
  assert.match(
    result.stderr,
    /identity cannot be proven|cannot be verified|unowned|leaving the live process alone/,
    'the refusal must say WHY the pid was left alone',
  );
  assert.equal(
    isAlive(sleeper.pid),
    true,
    'the recycled pid (an unrelated process) must survive the acceptance reset untouched',
  );
});

test('acceptance reset refuses a live pidfile pid with a non-fleetd process shape', async (t: TestContext) => {
  const home = scratchHome(t);
  const sleeper = spawnSleeper(t);
  await waitUntil(() => sleeper.pid != null, { label: 'sleeper pid' });

  // A STRICT record (pid + port) — but the live process is a recycled
  // non-fleetd pid: no fleetd answers on that port, so /health cannot confirm
  // the pid and the pid must be treated as unowned even with a JSON pidfile.
  writeFileSync(
    path.join(home, 'fleetd.pid'),
    JSON.stringify({ pid: sleeper.pid, port: randomPort() }),
  );

  const result = await runStop(home);
  assert.equal(result.code, 1, 'an unproven live pid must abort the reset, not be killed');
  assert.equal(
    isAlive(sleeper.pid),
    true,
    'a live non-fleetd pid must survive even when the pidfile names it in strict JSON form',
  );
});

test('acceptance reset stops a verified daemon: strict pidfile + /health.pid match + fleetd shape', async (t: TestContext) => {
  const daemon = await startDaemon(); // strict JSON pidfile in daemon.home
  t.after(async () => {
    await daemon.stop();
  });
  const healthPid = ((await (await fetch(`${daemon.baseUrl}/health`)).json()) as { pid: number })
    .pid;

  const result = await runStop(daemon.home, 10);
  assert.equal(result.code, 0, `a verified fleetd must be stoppable (stderr: ${result.stderr})`);

  await waitUntil(() => !isAlive(healthPid), {
    timeoutMs: 5000,
    label: 'daemon exit after verified SIGTERM',
  });
  assert.equal(
    isAlive(healthPid),
    false,
    'the verified daemon must be signalled and actually exit',
  );
});

test('acceptance reset treats dead/missing pidfile entries as nothing to do', async (t: TestContext) => {
  const home = scratchHome(t);

  // Missing pidfile: success, no output.
  const missing = await runStop(home);
  assert.equal(missing.code, 0);

  // A pid that does not exist (dead before the reset ran): success, and the
  // reset must NOT abort — a stale pidfile for a dead process is the common
  // post-crash case and must not block the gate.
  writeFileSync(path.join(home, 'fleetd.pid'), '999999\n');
  const dead = await runStop(home);
  assert.equal(dead.code, 0, `a dead legacy pid must be a no-op (stderr: ${dead.stderr})`);

  // Unparseable pidfile contents: success (nothing identifiable to kill).
  writeFileSync(path.join(home, 'fleetd.pid'), 'not-a-pid\n');
  const garbage = await runStop(home);
  assert.equal(garbage.code, 0);
});
