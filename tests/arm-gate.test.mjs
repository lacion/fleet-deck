// tests/arm-gate.test.mjs
//
// 0.16.0 — the unsupervised-spawn arm gate. The board's red two-step used to
// be UI-only: any loopback process could curl an unsupervised spawn into
// being (a fleet-spawned agent's sandbox escape). Now an unsupervised body
// (dangerously_skip_permissions:true OR permission_mode:'bypassPermissions')
// must echo a fresh, single-use arm token minted by the token-gated
// POST /api/spawn/arm-unsupervised. Supervised spawns are untouched.

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { startDaemon } from './helpers/daemon.mjs';
import { postJson, getJson } from './helpers/http.mjs';
import { waitForSpecRecords } from './helpers/wait.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPAWN_CMD_FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.mjs');
try { chmodSync(SPAWN_CMD_FIXTURE, 0o755); } catch { /* best-effort */ }

function scratchDir() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-armgate-'));
}

async function arm(daemon) {
  const res = await postJson(`${daemon.baseUrl}/api/spawn/arm-unsupervised`, {}, { token: daemon.token });
  assert.equal(res.status, 200, 'arm endpoint answers');
  assert.ok(typeof res.json?.arm_token === 'string' && res.json.arm_token.length >= 16);
  return res.json.arm_token;
}

test('the arm endpoint itself is token-gated', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const bare = await postJson(`${daemon.baseUrl}/api/spawn/arm-unsupervised`, {});
  assert.equal(bare.status, 401, 'tokenless arm must 401');
});

test('unsupervised spawn without an arm token → 403, nothing created', async (t) => {
  const recordDir = scratchDir();
  const recordFile = path.join(recordDir, 'spawns.jsonl');
  const daemon = await startDaemon({ env: { FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE, FLEETDECK_TEST_SPAWN_RECORD: recordFile } });
  t.after(async () => { await daemon.stop(); rmSync(recordDir, { recursive: true, force: true }); });

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  for (const body of [
    { cwd, dangerously_skip_permissions: true },
    { cwd, permission_mode: 'bypassPermissions' },
  ]) {
    const res = await postJson(`${daemon.baseUrl}/api/spawn`, body);
    assert.equal(res.status, 403, `unarmed unsupervised spawn must 403 (${JSON.stringify(body)})`);
    assert.match(res.json?.reason ?? '', /arm/i);
  }

  // A stale/fabricated arm token is equally refused.
  const forged = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, dangerously_skip_permissions: true, arm_token: 'not-a-real-token' });
  assert.equal(forged.status, 403, 'fabricated arm token must 403');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.sessions.length, 0, 'no session was created by the refused spawns');
});

test('an armed unsupervised spawn works; the token is single-use', async (t) => {
  const recordDir = scratchDir();
  const recordFile = path.join(recordDir, 'spawns.jsonl');
  const daemon = await startDaemon({ env: { FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE, FLEETDECK_TEST_SPAWN_RECORD: recordFile } });
  t.after(async () => { await daemon.stop(); rmSync(recordDir, { recursive: true, force: true }); });

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  const token = await arm(daemon);
  const first = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, dangerously_skip_permissions: true, arm_token: token });
  assert.equal(first.status, 200, `armed spawn should succeed: ${JSON.stringify(first.json)}`);

  // Reuse → 403 (single-use).
  const second = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, dangerously_skip_permissions: true, arm_token: token });
  assert.equal(second.status, 403, 'arm token reuse must 403');

  // A fresh arm works again — and via the permission_mode route too.
  const token2 = await arm(daemon);
  const third = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd: scratchDir(), permission_mode: 'bypassPermissions', arm_token: token2 });
  assert.equal(third.status, 200, 'second armed spawn (permission_mode route) succeeds');
});

test('supervised spawns need no arm token', async (t) => {
  const recordDir = scratchDir();
  const recordFile = path.join(recordDir, 'spawns.jsonl');
  const daemon = await startDaemon({ env: { FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE, FLEETDECK_TEST_SPAWN_RECORD: recordFile } });
  t.after(async () => { await daemon.stop(); rmSync(recordDir, { recursive: true, force: true }); });

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, prompt: 'ordinary supervised work' });
  assert.equal(res.status, 200, `supervised spawn unaffected: ${JSON.stringify(res.json)}`);
});

test('adopt with dangerously_skip_permissions:true also requires the arm', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  // Register + genuinely end a session so adopt has a real target row.
  const { randomUUID } = await import('node:crypto');
  const { postHook } = await import('./helpers/http.mjs');
  const { loadFixture } = await import('./helpers/fixtures.mjs');
  const sid = randomUUID();
  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  await postHook(daemon.baseUrl, 'SessionEnd', loadFixture('session-end', { session_id: sid, cwd }), { token: daemon });

  const unarmed = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, { dangerously_skip_permissions: true });
  assert.equal(unarmed.status, 403, 'unarmed unsupervised adopt must 403');
});
