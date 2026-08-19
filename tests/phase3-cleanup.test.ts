// tests/phase3-cleanup.test.ts
//
// Regression for BUG-007: demo/run-accept-phase3.sh used to leave the
// detached scratch fleetd (elected by the sessions' SessionStart hook)
// running after the gate exited — its only EXIT trap killed the isolated
// tmux server. The gate must now stop the daemon it started and must fail
// cleanup loudly when the listener cannot be verified gone.
//
// These tests never execute the real gate (it spends Claude usage). They
// copy the script, truncate it right after the cleanup trap is armed, and
// append a scratch-daemon simulation; `set -u` then aborts the truncated
// script, which is exactly what drives the EXIT trap.

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const SCRIPT = path.join(ROOT, 'demo', 'run-accept-phase3.sh');

interface TruncatedGateResult {
  code: number | null;
  stdout: string;
  stderr: string;
  home: string;
  port: number;
  stubPid: number | null;
}

function scratch(t: TestContext, prefix: string): string {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  t.after(() => {
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return dir;
}

async function freePort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address();
  assert.ok(address && typeof address !== 'string', 'ephemeral TCP listener has an address');
  const port = address.port;
  await new Promise<void>((resolve, reject) => {
    server.close((error) => {
      if (error) reject(error);
      else resolve();
    });
  });
  return port;
}

function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function isOwnedStub(pid: number): boolean {
  const probe = spawnSync('ps', ['-p', String(pid), '-o', 'command='], { encoding: 'utf8' });
  return (
    probe.status === 0 &&
    probe.stdout.includes('stub-daemon.mjs') &&
    probe.stdout.includes('p3test-stub')
  );
}

// Wait until fn() is truthy (or throw). Small bounded poll for server boot.
async function waitFor(
  fn: () => Promise<boolean>,
  { tries = 100, stepMs = 100 }: { tries?: number; stepMs?: number } = {},
): Promise<boolean> {
  for (let i = 0; i < tries; i += 1) {
    const value = await fn();
    if (value) return value;
    await new Promise<void>((resolve) => {
      setTimeout(() => {
        resolve();
      }, stepMs);
    });
  }
  throw new Error('condition not met in time');
}

// The trap-arming line must stay the FINAL trap the script installs, so the
// truncation below reproduces the real cleanup path.
function trapLine(source: string): string {
  const lines = source.split('\n');
  const index = lines.findIndex((line) => line.trim() === 'trap cleanup EXIT');
  assert.notEqual(index, -1, 'run-accept-phase3.sh must arm `trap cleanup EXIT`');
  const trapText = lines[index] ?? '';
  assert.equal(
    source.slice(source.indexOf(trapText)).indexOf('trap '),
    0,
    'no later trap may override the cleanup trap',
  );
  return lines.slice(0, index + 1).join('\n') + '\n';
}

// Launch a truncated copy of the gate whose EXIT trap runs against a stub
// daemon in `home`. `daemonJs` is a small node program standing in for
// fleetd: it writes the strict pid record the gate verifies and serves
// /health on FLEETDECK_PORT.
async function runTruncatedGate(
  t: TestContext,
  { daemonJs }: { daemonJs: string },
): Promise<TruncatedGateResult> {
  const home = scratch(t, 'fleetdeck-p3-home-');
  // This file runs beside many daemon suites. A PID-derived fixed port can be
  // occupied by an unrelated concurrent fixture, making the first assertion
  // fail and its surviving stub poison the following two cases.
  const port = await freePort();
  writeFileSync(path.join(home, 'stub-daemon.mjs'), daemonJs);

  const gate = path.join(scratch(t, 'fleetdeck-p3-gate-'), 'gate.sh');
  const prefix = trapLine(readFileSync(SCRIPT, 'utf8')).replace(
    /^export FLEETDECK_TMUX_SOCKET="fdaccept-\$\$"$/m,
    'export FLEETDECK_TMUX_SOCKET="fdp3test-$$"',
  );
  writeFileSync(
    gate,
    prefix +
      `
# ---- simulated run body: a SessionStart hook elected our stub daemon
node "$SCRATCH_HOME/stub-daemon.mjs" "$SCRATCH_HOME" "$FLEETDECK_PORT" p3test-stub &
echo $! > "$SCRATCH_HOME/stub-shell.pid"
echo "gate body done (pidfile may not exist yet — trap must cope)"
exit 3
`,
  );
  const child = spawn('bash', [gate], {
    env: {
      ...process.env,
      PATH: process.env['PATH'],
      FLEETDECK_HOME_OVERRIDE: home,
      FLEETDECK_PORT: String(port),
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (chunk: Buffer) => {
    stdout += chunk.toString();
  });
  child.stderr.on('data', (chunk: Buffer) => {
    stderr += chunk.toString();
  });
  const code = await new Promise<number | null>((resolve) => {
    child.once('exit', (c) => {
      resolve(c);
    });
  });
  // The gate correctly refuses to SIGKILL an unproven process; the stub is OUR
  // child (marker arg checked above), so the test reaps what the gate spared —
  // otherwise the node --test process would wait on it forever.
  let stubPid: number | null = null;
  try {
    stubPid = Number(readFileSync(path.join(home, 'stub-shell.pid'), 'utf8').trim());
  } catch {
    /* pidfile may not have been written yet */
  }
  t.after(async () => {
    // Assertion failures must not leave this test-owned background stub on the
    // shared runner. Verify the marker argv before signalling the exact PID.
    if (
      stubPid == null ||
      !Number.isInteger(stubPid) ||
      !pidAlive(stubPid) ||
      !isOwnedStub(stubPid)
    ) {
      return;
    }
    try {
      process.kill(stubPid, 'SIGKILL');
    } catch {
      return;
    }
    await waitFor(async () => !pidAlive(stubPid), { tries: 200, stepMs: 10 });
  });
  return { code, stdout, stderr, home, port, stubPid };
}

// A cooperative fleetd stand-in: strict pid record, /health with matching pid,
// and a graceful SIGTERM exit like the real daemon's tested shutdown.
const COOPERATIVE_DAEMON = `
// Marker arg so the test can tell its own stub from anything else on the box.
if (process.argv[4] !== 'p3test-stub') process.exit(1);
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
const [home, port] = process.argv.slice(2);
fs.writeFileSync(path.join(home, 'fleetd.pid'), JSON.stringify({ pid: process.pid, port: Number(port) }));
const server = http.createServer((req, res) => {
  if (req.url === '/health') {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ ok: true, pid: process.pid }));
  } else {
    res.writeHead(404); res.end();
  }
});
server.listen(Number(port), '127.0.0.1');
process.on('SIGTERM', () => { server.close(() => process.exit(0)); setTimeout(() => process.exit(0), 200).unref(); });
`;

// A daemon that ignores SIGTERM (a wedged fleetd): cleanup must NOT kill -9 it
// and must NOT pretend the gate is clean.
const STUBBORN_DAEMON = `
if (process.argv[4] !== 'p3test-stub') process.exit(1);
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
const [home, port] = process.argv.slice(2);
fs.writeFileSync(path.join(home, 'fleetd.pid'), JSON.stringify({ pid: process.pid, port: Number(port) }));
const server = http.createServer((req, res) => {
  res.writeHead(200, { 'content-type': 'application/json' });
  res.end(JSON.stringify({ ok: true, pid: process.pid }));
});
server.listen(Number(port), '127.0.0.1');
process.on('SIGTERM', () => {});
`;

// A listener that is NOT this run's daemon: nothing in the scratch home proves
// ownership, so cleanup must leave it alone — and refuse to claim success.
const FOREIGN_LISTENER = `
if (process.argv[4] !== 'p3test-stub') process.exit(1);
import http from 'node:http';
const port = Number(process.argv[3]);
http.createServer((req, res) => {
  res.writeHead(200, { 'content-type': 'application/json' });
  res.end(JSON.stringify({ ok: true, pid: process.pid }));
}).listen(port, '127.0.0.1');
process.on('SIGTERM', () => {});
`;

test('phase3 gate EXIT trap stops the detached scratch daemon it started', async (t: TestContext) => {
  const { code, port } = await runTruncatedGate(t, { daemonJs: COOPERATIVE_DAEMON });
  assert.equal(code, 3, 'cleanup must preserve the gate body exit code when teardown succeeds');
  await waitFor(async () => {
    try {
      await fetch(`http://127.0.0.1:${port}/health`, { signal: AbortSignal.timeout(200) });
      return false;
    } catch {
      return true;
    }
  }).catch(() =>
    assert.fail(`stub daemon still listening on :${port} after the gate exited — BUG-007 leak`),
  );
});

test('phase3 gate fails cleanup when the scratch daemon survives SIGTERM', async (t: TestContext) => {
  const { code, stderr, port, stubPid } = await runTruncatedGate(t, { daemonJs: STUBBORN_DAEMON });
  assert.equal(code, 1, 'a surviving daemon must fail the run, not exit quietly');
  assert.match(stderr, /CLEANUP FAILED: scratch daemon not verified stopped/);
  // The gate correctly refused to kill -9; the test owns the stubborn stub.
  try {
    if (stubPid != null) process.kill(stubPid, 'SIGKILL');
  } catch {
    /* already gone */
  }
  try {
    await fetch(`http://127.0.0.1:${port}/health`, { signal: AbortSignal.timeout(250) });
    assert.fail('stub should be dead after SIGKILL');
  } catch (err) {
    if (err != null && (err as { code?: string }).code === 'ERR_ASSERTION') throw err;
  }
});

test('phase3 gate never signals a foreign listener on its port', async (t: TestContext) => {
  const { code, stderr, port, stubPid } = await runTruncatedGate(t, { daemonJs: FOREIGN_LISTENER });
  assert.equal(code, 1, 'an unprovable listener must fail cleanup, not be claimed as stopped');
  assert.match(stderr, /CLEANUP FAILED/);
  // The foreign process must still be alive: no pidfile in the scratch home,
  // so the trap had no verified target and must not have signalled anything.
  const health = (await (
    await fetch(`http://127.0.0.1:${port}/health`, { signal: AbortSignal.timeout(250) })
  ).json()) as { pid: number };
  assert.equal(health.pid, stubPid);
  if (stubPid != null) process.kill(stubPid, 'SIGKILL');
});
