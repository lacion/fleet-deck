// tests/election.test.mjs
//
// Daemon election rule: election = port bind. A second fleetd launched
// on a port that's already bound must lose (EADDRINUSE) and exit with code 3;
// the launcher is expected to poll /health and proceed as a client instead.

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
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
