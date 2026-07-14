// tests/mail-and-blocking.test.mjs
//
// Stop endpoint / mailbox sharp edges:
//   - at most one mailbox block per session per turn, enforced server-side
//     (never trust stop_hook_active)
//   - UserPromptSubmit drains mail as additionalContext
//   - POST /mail targeting: session_id/callsign, "all", "repo:<name>"

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import { makeRepoWithWorktree } from './helpers/gitrepo.mjs';

function findSession(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

test('one block per turn: Stop blocks once on pending mail, then passes, then blocks again after a new turn + new mail', async (t) => {
  const daemon = await startDaemon();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }));

  const mailRes = await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'tester', text: 'please wrap up soon' });
  assert.equal(mailRes.status, 200, 'POST /mail should 200');

  // First Stop: mail is pending -> block, exactly once.
  const stop1 = await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: sid, cwd }));
  assert.equal(stop1.json?.decision, 'block', 'Stop with pending mail should block');
  assert.match(stop1.json?.reason ?? '', /\[FLEETDECK MAIL\]/, 'block reason should carry the FLEETDECK MAIL marker');

  // Immediate second Stop in the same turn: must NOT block again (server-side
  // one-block-per-turn guard; must not rely on stop_hook_active).
  const stop2 = await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: sid, cwd }));
  assert.deepEqual(stop2.json, {}, 'immediate second Stop in the same turn must return {} (no repeat block)');

  // New turn boundary via UserPromptSubmit clears the blocked_this_turn flag.
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }, {
    prompt: 'continue',
  }));

  // Fresh mail arrives mid-turn.
  await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'tester', text: 'second message' });

  // Stop should be able to block again now that a new turn has started.
  const stop3 = await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: sid, cwd }));
  assert.equal(stop3.json?.decision, 'block', 'Stop should block again after a new turn + new mail');
  assert.match(stop3.json?.reason ?? '', /\[FLEETDECK MAIL\]/);
});

test('UserPromptSubmit drains pending mail as additionalContext', async (t) => {
  const daemon = await startDaemon();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
  await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'ops', text: 'ping from the board' });

  const res = await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }));
  const hso = res.json?.hookSpecificOutput;
  assert.ok(hso, 'UserPromptSubmit with pending mail should return hookSpecificOutput');
  assert.equal(hso.hookEventName, 'UserPromptSubmit');
  assert.match(hso.additionalContext, /^\[FLEETDECK\]/);
  assert.ok(hso.additionalContext.includes('ping from the board'), 'delivered context should carry the mail text');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.mail_pending?.[sid] ?? 0, 0, 'mailbox should be drained after delivery');
});

test('GET /mail?session=<sid> drains the mailbox directly', async (t) => {
  const daemon = await startDaemon();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
  await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'skill', text: 'direct drain check' });

  const drained = await getJson(`${daemon.baseUrl}/mail?session=${encodeURIComponent(sid)}`);
  assert.equal(drained.status, 200);
  assert.ok(Array.isArray(drained.json?.mail), 'GET /mail should return a mail array');
  assert.equal(drained.json.mail.length, 1, 'the pending message should be present');
  assert.ok(drained.json.mail.some(m => m.text === 'direct drain check'), 'drained mail should carry the original text');

  // A second GET should come back empty -- it's a drain, not a peek.
  const second = await getJson(`${daemon.baseUrl}/mail?session=${encodeURIComponent(sid)}`);
  assert.equal((second.json?.mail || []).length, 0, 'GET /mail should drain the box, not just read it');
});

test('POST /mail targeting: session/callsign, "all", "repo:<name>"', async (t) => {
  const daemon = await startDaemon();
  const repoA = makeRepoWithWorktree({ repoName: 'fleet-repo-a' });
  const repoB = makeRepoWithWorktree({ repoName: 'fleet-repo-b' });
  t.after(async () => { await daemon.stop(); repoA.cleanup(); repoB.cleanup(); });

  const sidX = randomUUID(); // repoA
  const sidY = randomUUID(); // repoA
  const sidZ = randomUUID(); // repoB

  const regX = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidX, cwd: repoA.root }));
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidY, cwd: repoA.root }));
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidZ, cwd: repoB.root }));

  const callsignX = regX.json?.callsign;
  assert.ok(callsignX, 'registration should hand back a callsign to target by');

  const pendingOf = (state, sid) => state.mail_pending?.[sid] ?? 0;

  // Target by callsign: only X should gain mail.
  await postJson(`${daemon.baseUrl}/mail`, { to: callsignX, from: 'human', text: 'to X only' });
  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(pendingOf(state, sidX), 1, 'callsign-targeted mail should land on X');
  assert.equal(pendingOf(state, sidY), 0, 'Y should be untouched by callsign targeting');
  assert.equal(pendingOf(state, sidZ), 0, 'Z should be untouched by callsign targeting');

  // Target "all": every registered session gains one more.
  await postJson(`${daemon.baseUrl}/mail`, { to: 'all', from: 'human', text: 'to everyone' });
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(pendingOf(state, sidX), 2, '"all" should add one more to X');
  assert.equal(pendingOf(state, sidY), 1, '"all" should reach Y');
  assert.equal(pendingOf(state, sidZ), 1, '"all" should reach Z (different repo, still "all")');

  // Target "repo:<name>": only sessions in repoA gain mail; repoB untouched.
  const repoTarget = `repo:${repoA.repoName}`;
  await postJson(`${daemon.baseUrl}/mail`, { to: repoTarget, from: 'human', text: 'to repo A' });
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(pendingOf(state, sidX), 3, `"${repoTarget}" should reach X`);
  assert.equal(pendingOf(state, sidY), 2, `"${repoTarget}" should reach Y`);
  assert.equal(pendingOf(state, sidZ), 1, `"${repoTarget}" must not reach Z (repo B)`);
});
