// tests/daemon-helper.test.mjs
//
// Regression for BUG-201: when startDaemon() allocates its own scratch
// FLEETDECK_HOME and the daemon never becomes healthy (syntax error, instant
// crash, port collision), the helper must not leak the unreturned directory —
// repeated failed runs would otherwise accumulate db/token/log/pid state in
// tmpdir. Caller-owned homes must survive a failed startup untouched (they
// hold the post-mortem evidence).
//
// Failure is induced TWO ways at once so the suite can never false-pass on a
// shared machine: the port is occupied by a local throwaway listener (so a
// foreign daemon happening to answer /health on that port is impossible —
// observed 2026-08-05 when several bug-bash worktrees ran concurrently) AND
// the spawned script is a stub that exits 1 on startup after leaving a marker
// file in FLEETDECK_HOME.

import test from 'node:test';
import assert from 'node:assert/strict';
import net from 'node:net';
import { existsSync, mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon, randomPort, REPO_ROOT } from './helpers/daemon.mjs';

const CRASH_STUB = path.join(REPO_ROOT, 'tests/fixtures/crash-daemon-stub.mjs');

// Bind 127.0.0.1 on a scratch-range port and hand back { port, close } so the
// test daemon can never win the election — and nothing else can be serving
// /health there either.
async function occupyPort() {
  for (let attempt = 0; attempt < 20; attempt++) {
    const port = randomPort();
    const server = net.createServer();
    server.unref(); // must never hold the test event loop open
    const taken = await new Promise(resolve => {
      server.once('error', () => resolve(true));
      server.listen(port, '127.0.0.1', () => resolve(false));
    });
    if (!taken) {
      return {
        port,
        // destroy(): server.close() waits for open connections, and the
        // failed health polls leave sockets TIME_WAIT-attached for seconds —
        // t.after would hang past the event-loop drain and cancel the test.
        close: () => { try { server.close(); } catch { /* already closed */ } },
      };
    }
  }
  throw new Error('could not find a free scratch-range port to occupy');
}

test('startDaemon removes its own scratch home when startup fails (BUG-201)', async (t) => {
  const blocker = await occupyPort();
  t.after(() => blocker.close());

  const before = new Set(readdirSync(tmpdir()).filter(n => n.startsWith('fleetdeck-test-')));
  await assert.rejects(
    startDaemon({ port: blocker.port, scriptPath: CRASH_STUB, healthTimeoutMs: 1500 }),
    /never became healthy/,
  );
  const leaked = readdirSync(tmpdir()).filter(n => n.startsWith('fleetdeck-test-') && !before.has(n));
  assert.deepEqual(leaked, [], `failed startup leaked scratch home(s): ${leaked.join(', ')}`);
});

test('startDaemon preserves a caller-owned home when startup fails', async (t) => {
  const blocker = await occupyPort();
  t.after(() => blocker.close());
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-home-owned-'));
  t.after(() => { rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  await assert.rejects(
    startDaemon({ port: blocker.port, home, scriptPath: CRASH_STUB, healthTimeoutMs: 1500 }),
    /never became healthy/,
  );
  assert.ok(existsSync(home), 'caller-owned home must survive a failed startup');
  assert.ok(existsSync(path.join(home, 'crashed.marker')), 'post-mortem evidence must be left in place');
});

test('startDaemon still cleans up its scratch home on successful startup via stop()', async (t) => {
  // Guards against the BUG-201 fix regressing the happy path: the default
  // home must still be returned and removed by stop().
  const daemon = await startDaemon({ port: randomPort() });
  t.after(async () => { await daemon.stop(); });
  assert.ok(existsSync(daemon.home), 'scratch home should exist while the daemon runs');
  await daemon.stop();
  assert.ok(!existsSync(daemon.home), 'stop() should still remove the scratch home');
});

test('startDaemon failure error still includes daemon output', async (t) => {
  const blocker = await occupyPort();
  t.after(() => blocker.close());

  const err = await startDaemon({ port: blocker.port, scriptPath: CRASH_STUB, healthTimeoutMs: 1500 })
    .then(() => null, e => e);
  assert.ok(err, 'startDaemon should reject');
  assert.match(err.message, /crash-daemon-stub: simulated startup crash/);
});
