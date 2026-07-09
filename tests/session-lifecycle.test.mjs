// tests/session-lifecycle.test.mjs
//
// Derived-columns rule: columns are derived from hook telemetry, never
// self-reported. Walks one session
// through every Phase-1 event and asserts the board column + note at each
// step, matching the spike's applyEvent transition table:
//   SessionStart -> queued
//   UserPromptSubmit -> working (+ task capture)
//   edit-tool PostToolUse -> editing note
//   Bash "npm test" / pytest -> verifying
//   Notification -> needsyou
//   Stop -> idle
//   SessionEnd -> offline + endedAt

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

test('telemetry derivation walks queued -> working -> editing -> verifying -> needsyou -> idle -> offline', async (t) => {
  const daemon = await startDaemon();
  const scratchCwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(async () => {
    await daemon.stop();
    rmSync(scratchCwd, { recursive: true, force: true });
  });

  const sid = randomUUID();
  const tokens = { session_id: sid, cwd: scratchCwd };

  // SessionStart -> queued
  const startRes = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', tokens));
  assert.equal(startRes.status, 200, 'SessionStart should 200');
  assert.ok(startRes.json?.callsign, 'SessionStart response must include a callsign');

  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  let card = findSession(state, sid);
  assert.ok(card, 'session should appear in /state after SessionStart');
  assert.equal(card.col, 'queued', 'SessionStart should derive col=queued');

  // UserPromptSubmit -> working (+ task capture)
  const prompt = 'Add an exported function slugify(s) to util.js (lowercase, trim, spaces to dashes, strip punctuation).';
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', tokens, { prompt }));
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.equal(card.col, 'working', 'UserPromptSubmit should derive col=working');
  assert.ok(card.task && prompt.startsWith(card.task) || card.task === prompt.slice(0, card.task.length),
    'task should capture (a prefix of) the prompt');

  // Edit-tool PostToolUse -> editing note
  const filePath = path.join(scratchCwd, 'util.js');
  await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', tokens, {
    tool_name: 'Edit',
    tool_input: { file_path: filePath, old_string: 'a', new_string: 'b' },
  }));
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.match(card.note, /util\.js/, 'editing note should mention the touched file');
  assert.equal(card.lastTool, 'Edit');

  // Bash "npm test" -> verifying
  await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-bash', tokens, {
    tool_name: 'Bash',
    tool_input: { command: 'npm test' },
  }));
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.equal(card.col, 'verifying', 'npm test should derive col=verifying');

  // Bash "pytest" also -> verifying (regex covers both invocations independently)
  await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-bash', tokens, {
    tool_name: 'Bash',
    tool_input: { command: 'pytest -q' },
  }));
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.equal(card.col, 'verifying', 'pytest should derive col=verifying');

  // Notification -> needsyou
  await postHook(daemon.baseUrl, 'Notification', loadFixture('notification', tokens, {
    message: 'Claude needs your permission to use Bash',
  }));
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.equal(card.col, 'needsyou', 'Notification should derive col=needsyou');

  // Stop -> idle (no mail pending, so no block)
  const stopRes = await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', tokens));
  assert.deepEqual(stopRes.json, {}, 'Stop with no mail should return {} (no block)');
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.equal(card.col, 'idle', 'Stop should derive col=idle');

  // SessionEnd -> offline + endedAt
  const endRes = await postHook(daemon.baseUrl, 'SessionEnd', loadFixture('session-end', tokens));
  assert.deepEqual(endRes.json, {}, 'SessionEnd should respond {}');
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.equal(card.col, 'offline', 'SessionEnd should derive col=offline');
  assert.ok(card.endedAt, 'SessionEnd should stamp endedAt');
});
