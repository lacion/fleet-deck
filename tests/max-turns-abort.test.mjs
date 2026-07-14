// tests/max-turns-abort.test.mjs
//
// Sharp edge: a --max-turns abort skips the Stop hook entirely. SessionEnd
// must still land and must be the only thing cleanup
// ever keys off of. This test never sends a Stop for the session at all.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';

test('SessionEnd tombstones a session even when Stop was never sent (max-turns abort shape)', async (t) => {
  const daemon = await startDaemon();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }));
  await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { session_id: sid, cwd }, {
    tool_input: { file_path: path.join(cwd, 'util.js'), old_string: 'a', new_string: 'b' },
  }));

  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  let card = state.sessions.find(s => s.session_id === sid);
  assert.notEqual(card.col, 'offline', 'sanity: session should not already be offline before SessionEnd');

  // No Stop is ever sent -- this is the point of the test.
  const endRes = await postHook(daemon.baseUrl, 'SessionEnd', loadFixture('session-end', { session_id: sid, cwd }, {
    reason: 'other',
  }));
  assert.deepEqual(endRes.json, {}, 'SessionEnd should respond {}');

  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = state.sessions.find(s => s.session_id === sid);
  assert.ok(card, 'session should still be present (tombstoned, not deleted)');
  assert.equal(card.col, 'offline', 'SessionEnd alone must derive col=offline, with no Stop ever having fired');
  assert.ok(card.endedAt, 'SessionEnd alone must stamp endedAt');
});
