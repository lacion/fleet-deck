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
    'archived', 'conflicts_cleared', 'feed_cleared', 'mail_expired', 'ok',
    'orphan_worktrees', 'questions_expired', 'questions_purged', 'windows_killed',
  ]);
  assert.equal(res.json.ok, true);
  assert.equal(res.json.archived, 1);
  assert.equal(res.json.mail_expired, 1);
  assert.equal(res.json.questions_expired, 0);
  assert.ok(Array.isArray(res.json.orphan_worktrees));
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.sessions.some(s => s.session_id === sid), false);
});

test('Clear wipes everything that is not alive: conflicts, the rail, the feed', async (t) => {
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); });

  // Two sessions in one repo argue over a file, then BOTH die.
  const dead1 = randomUUID();
  const dead2 = randomUUID();
  for (const sid of [dead1, dead2]) {
    await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd: process.cwd(), source: 'startup' });
    await postHook(daemon.baseUrl, 'PostToolUse', {
      session_id: sid, cwd: process.cwd(), tool_name: 'Edit',
      tool_input: { file_path: `${process.cwd()}/contested.js` },
    });
  }
  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.ok(state.conflicts.length >= 1, 'sanity: the radar raised a conflict');
  assert.ok(state.conflicts[0].callsigns?.length, 'a conflict must name callsigns, never raw uuids');
  assert.ok(state.ticker.length > 0, 'sanity: the feed has narration in it');

  // A THIRD session is still alive and arguing with nobody — it must survive.
  const alive = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: alive, cwd: process.cwd(), source: 'startup' });

  for (const sid of [dead1, dead2]) {
    await postHook(daemon.baseUrl, 'SessionEnd', { session_id: sid, cwd: process.cwd(), reason: 'done' });
  }

  const res = await postJson(`${daemon.baseUrl}/api/cleanup`, {});
  assert.equal(res.status, 200);
  assert.ok(res.json.conflicts_cleared >= 1, 'a conflict between two dead sessions is not news');

  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.conflicts.length, 0, 'the conflict banner must not outlive its sessions');
  assert.equal(state.questions.filter(q => q.status !== 'pending').length, 0, 'the rail keeps no ghosts');
  assert.ok(state.ticker.length <= 1, 'the feed is wiped (bar the line announcing the wipe)');
  assert.ok(state.sessions.some(s => s.session_id === alive), 'the living are never cleared');
  assert.equal(state.sessions.some(s => s.session_id === dead1), false, 'the dead are');
});
