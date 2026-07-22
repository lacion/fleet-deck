// tests/conflict.test.mjs
//
// File-conflict rule (F4): two sessions editing
// the same repo-relative file collide; the second editor gets a whisper via
// PostToolUse additionalContext naming the rival, and the rival gets mail.
// A session that has already ended still counts as a rival within the
// 30-minute window ("dirty files outlive their authors").

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';

function findSession(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

async function registerAndGetCallsign(daemon, sid, cwd) {
  const res = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  return res.json?.callsign;
}

test('second session touching the same file gets a whisper naming the rival; rival gets mail', async (t) => {
  const daemon = await startDaemon();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sidA = randomUUID();
  const sidB = randomUUID();
  const callsignA = await registerAndGetCallsign(daemon, sidA, cwd);
  await registerAndGetCallsign(daemon, sidB, cwd);
  assert.ok(callsignA, 'session A should get a callsign');

  const filePath = path.join(cwd, 'util.js');

  // A touches the file first: no rivals yet, no whisper.
  const firstTouch = await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { token: daemon, session_id: sidA, cwd }, {
    tool_name: 'Edit',
    tool_input: { file_path: filePath, old_string: 'a', new_string: 'b' },
  }), { token: daemon });
  assert.deepEqual(firstTouch.json, {}, 'first touch on a file should not produce a whisper');

  // B touches the same file: expect a whisper naming A.
  const secondTouch = await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { token: daemon, session_id: sidB, cwd }, {
    tool_name: 'Edit',
    tool_input: { file_path: filePath, old_string: 'b', new_string: 'c' },
  }), { token: daemon });
  const hso = secondTouch.json?.hookSpecificOutput;
  assert.ok(hso, 'second touch should produce hookSpecificOutput (whisper)');
  assert.equal(hso.hookEventName, 'PostToolUse');
  assert.match(hso.additionalContext, /^\[FLEETDECK\]/, 'whisper should be tagged [FLEETDECK]');
  assert.match(hso.additionalContext, /⚠/, 'whisper should carry the warning glyph');
  assert.ok(hso.additionalContext.includes(callsignA), 'whisper should name the rival by callsign');

  // Rival (A) should have mail waiting.
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.ok(state.mail_pending, '/state should expose mail_pending');
  assert.ok((state.mail_pending[sidA] ?? 0) >= 1, 'rival session A should have pending mail after the conflict');

  // The conflict ledger should record the file with both sessions, severity
  // "warning" because both cwds are the same worktree.
  assert.ok(Array.isArray(state.conflicts) && state.conflicts.length >= 1, '/state.conflicts should record the collision');
  const entry = state.conflicts.find(c => (c.rel_path || c.file || '').includes('util.js'));
  assert.ok(entry, 'conflict entry for util.js should exist');
  assert.equal(entry.severity, 'warning', 'same-worktree conflict should be severity=warning');
});

test('a session that already ended still counts as a rival within the window', async (t) => {
  const daemon = await startDaemon();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sidC = randomUUID();
  const sidD = randomUUID();
  const callsignC = await registerAndGetCallsign(daemon, sidC, cwd);
  await registerAndGetCallsign(daemon, sidD, cwd);

  const filePath = path.join(cwd, 'test.js');

  await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { token: daemon, session_id: sidC, cwd }, {
    tool_name: 'Write',
    tool_input: { file_path: filePath, content: 'x' },
  }), { token: daemon });

  // C leaves the fleet (SessionEnd is the tombstone).
  await postHook(daemon.baseUrl, 'SessionEnd', loadFixture('session-end', { session_id: sidC, cwd }), { token: daemon });
  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(findSession(state, sidC).col, 'offline', 'C should be tombstoned before D touches the file');

  // D touches the same file shortly after: C's dirty edit still counts.
  //
  // NOTE: this exercises the "just ended" edge of the 30-minute window, not
  // the expiry boundary itself -- there is no way to fast-forward the
  // daemon's real clock from a black-box HTTP test, and waiting 30 real
  // minutes is not practical for a test suite. The expiry boundary itself is
  // untested here; flag this if fleetd exposes a way to shrink the window
  // for tests (e.g. an env override), which would make it testable.
  const res = await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { token: daemon, session_id: sidD, cwd }, {
    tool_name: 'MultiEdit',
    tool_input: { file_path: filePath, edits: [{ old_string: 'x', new_string: 'y' }] },
  }), { token: daemon });
  const hso = res.json?.hookSpecificOutput;
  assert.ok(hso, 'D should still get a whisper about the now-offline C');
  assert.ok(hso.additionalContext.includes(callsignC), 'whisper should name the ended rival by callsign');

  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.ok((state.mail_pending[sidC] ?? 0) >= 1, 'the ended rival should still receive mail');
});
