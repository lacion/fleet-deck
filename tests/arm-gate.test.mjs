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

test('a cased permission_mode variant cannot sneak past the gate', async (t) => {
  const recordDir = scratchDir();
  const recordFile = path.join(recordDir, 'spawns.jsonl');
  const daemon = await startDaemon({ env: { FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE, FLEETDECK_TEST_SPAWN_RECORD: recordFile } });
  t.after(async () => { await daemon.stop(); rmSync(recordDir, { recursive: true, force: true }); });

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  // Claude Code accepts the mode case-insensitively, so 'BypassPermissions'
  // must be gated exactly like the lowercase spelling — an exact-string gate
  // would wave it through as supervised while the pane boots unsupervised.
  for (const variant of ['BypassPermissions', 'BYPASSPERMISSIONS', 'bypassPermissions']) {
    const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, permission_mode: variant });
    assert.equal(res.status, 403, `cased variant ${variant} must 403 without an arm`);
  }

  // And WITH an arm, the canonical spelling is what reaches argv.
  const armToken = await arm(daemon);
  const ok = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, permission_mode: 'BypassPermissions', arm_token: armToken });
  assert.equal(ok.status, 200, JSON.stringify(ok.json));
  const { waitForSpecRecords } = await import('./helpers/wait.mjs');
  const records = await waitForSpecRecords(recordFile, 1);
  const argv = records[0].parsed?.argv ?? [];
  const idx = argv.indexOf('--permission-mode');
  assert.ok(idx !== -1, 'permission-mode flag present');
  assert.equal(argv[idx + 1], 'bypassPermissions', 'canonical spelling reaches argv');
});

test('unknown permission_mode values 400', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, permission_mode: 'yolo' });
  assert.equal(res.status, 400);
  assert.match(res.json?.reason ?? '', /unknown permission_mode/);
});

test('revive of an unsupervised lineage requires a fresh arm', async (t) => {
  const recordDir = scratchDir();
  const recordFile = path.join(recordDir, 'spawns.jsonl');
  const daemon = await startDaemon({ env: { FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE, FLEETDECK_TEST_SPAWN_RECORD: recordFile } });
  t.after(async () => { await daemon.stop(); rmSync(recordDir, { recursive: true, force: true }); });

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  // Spawn unsupervised (armed), let the row go terminal, then try to revive.
  const token1 = await arm(daemon);
  const spawn = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, dangerously_skip_permissions: true, arm_token: token1 });
  assert.equal(spawn.status, 200, JSON.stringify(spawn.json));
  const spawnId = spawn.json.spawn_id;

  // Take the row terminal directly (a fixture spawn has no tmux window to
  // kill — kill would 410 'window already gone').
  const { openDb } = await import('../scripts/fleetd/db.mjs');
  const db = openDb(path.join(daemon.home, 'fleetd.db'));
  db.prepare("UPDATE spawns SET status = 'gone' WHERE spawn_id = ?").run(spawnId);
  db.close();

  // The bypass from the adversarial review: the 60s single-use arm must NOT
  // become a permanent replayable capability via revive.
  const unarmed = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/revive`, {});
  assert.equal(unarmed.status, 403, 'revive of an unsupervised row must 403 without a fresh arm');
  assert.match(unarmed.json?.reason ?? '', /arm/i);

  // With a fresh arm the revive proceeds past the gate (it may fail later on
  // resume evidence — the fixture transcript doesn't exist — but NOT with 403).
  const token2 = await arm(daemon);
  const armed = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/revive`, { arm_token: token2 });
  assert.notEqual(armed.status, 403, `armed revive must pass the gate (got ${armed.status}: ${JSON.stringify(armed.json)})`);
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
