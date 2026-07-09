// tests/restart-durability.test.mjs
//
// SQLite schema rule: fleetd.db lives under FLEETDECK_HOME and
// must survive a process restart. Kill the daemon, start a fresh process
// against the SAME FLEETDECK_HOME, and confirm /state still has the sessions.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon, randomPort } from './helpers/daemon.mjs';
import { postHook, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';

test('restart durability: same FLEETDECK_HOME after kill+restart still has the sessions', async (t) => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(() => {
    rmSync(home, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  });

  const sid = randomUUID();

  const first = await startDaemon({ port: randomPort(), home });
  try {
    await postHook(first.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
    await postHook(first.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }));
    const stateBefore = (await getJson(`${first.baseUrl}/state`)).json;
    const cardBefore = stateBefore.sessions.find(s => s.session_id === sid);
    assert.ok(cardBefore, 'session should be present before restart');
    assert.equal(cardBefore.col, 'working');
  } finally {
    // kill, but keep the SQLite files in `home` for the next process
    await first.stop({ keepHome: true });
  }

  const second = await startDaemon({ port: randomPort(), home });
  t.after(async () => { await second.stop({ keepHome: false }); });

  const stateAfter = (await getJson(`${second.baseUrl}/state`)).json;
  const cardAfter = stateAfter.sessions.find(s => s.session_id === sid);
  assert.ok(cardAfter, 'session should still be present after restart against the same FLEETDECK_HOME');
  assert.equal(cardAfter.session_id, sid);
});
