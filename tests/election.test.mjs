// tests/election.test.mjs
//
// Daemon election rule: election = port bind. A second fleetd launched
// on a port that's already bound must lose (EADDRINUSE) and exit with code 3;
// the launcher is expected to poll /health and proceed as a client instead.

import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon, spawnRaw, randomPort } from './helpers/daemon.mjs';

test('a second daemon on the same port loses the election and exits with code 3', async (t) => {
  const port = randomPort();
  const homeA = mkdtempSync(path.join(tmpdir(), 'fleetdeck-home-a-'));
  const homeB = mkdtempSync(path.join(tmpdir(), 'fleetdeck-home-b-'));
  t.after(() => {
    rmSync(homeA, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(homeB, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const winner = await startDaemon({ port, home: homeA });
  t.after(async () => { await winner.stop(); });

  const loser = spawnRaw({ port, home: homeB });
  t.after(async () => { await loser.kill(); });

  const code = await loser.waitForExit(10000);
  assert.equal(code, 3, `loser should exit 3 on EADDRINUSE, got ${code}. stderr: ${loser.stderr}`);

  // The winner should be completely unaffected.
  const health = await fetch(`${winner.baseUrl}/health`);
  assert.equal(health.status, 200, 'the winning daemon should remain healthy after the collision');
});

// BUG-121 regression: a malformed FLEETDECK_PORT made server.listen throw
// synchronously AFTER the pid guard claimed HOME — the async server 'error'
// handler never ran, so the stale pidfile (recording a garbage port) wedged
// every supervised restart until it was removed by hand. The daemon must
// refuse a bad port BEFORE touching HOME: exit 1, a clear reason on stderr,
// and no pidfile written.
test('a malformed FLEETDECK_PORT refuses to start before claiming HOME (no stale pidfile)', async (t) => {
  for (const bad of ['banana', '4711.5', '-1', '70000']) {
    const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-badport-'));
    t.after(() => { rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

    const refused = spawnRaw({ port: bad, home });
    t.after(async () => { await refused.kill(); });

    const code = await refused.waitForExit(10000);
    assert.equal(code, 1, `FLEETDECK_PORT='${bad}' should exit 1, got ${code}. stderr: ${refused.stderr}`);
    assert.match(refused.stderr, /FLEETDECK_PORT/, `stderr should name the offending variable. stderr: ${refused.stderr}`);
    assert.equal(existsSync(path.join(home, 'fleetd.pid')), false, `FLEETDECK_PORT='${bad}' must not leave a pidfile behind`);
  }
});
