import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';

test('POST /api/cleanup archives offline sessions and expires their queued mail', async (t) => {
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); });
  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd: process.cwd(), source: 'startup' });
  await postHook(daemon.baseUrl, 'SessionEnd', { session_id: sid, cwd: process.cwd(), reason: 'done' });
  await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'ops', text: 'undeliverable' });

  const res = await postJson(`${daemon.baseUrl}/api/cleanup`, {});
  assert.equal(res.status, 200);
  assert.deepEqual(Object.keys(res.json).sort(), [
    'archived', 'mail_expired', 'ok', 'orphan_worktrees', 'questions_expired', 'windows_killed',
  ]);
  assert.equal(res.json.ok, true);
  assert.equal(res.json.archived, 1);
  assert.equal(res.json.mail_expired, 1);
  assert.equal(res.json.questions_expired, 0);
  assert.ok(Array.isArray(res.json.orphan_worktrees));
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.sessions.some(s => s.session_id === sid), false);
});
