import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from '../scripts/fleetd/db.mjs';
import { claudeTranscriptPath, mungeClaudeProjectCwd } from '../scripts/fleetd/derive.mjs';
import { startDaemon } from './helpers/daemon.mjs';
import { getJson, postHook, postJson } from './helpers/http.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPAWN_CMD_FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.mjs');
try { chmodSync(SPAWN_CMD_FIXTURE, 0o755); } catch { /* best effort */ }

function scratch(prefix = 'fleetdeck-revive-') {
  return mkdtempSync(path.join(tmpdir(), prefix));
}

function withDb(home, fn) {
  const db = openDb(path.join(home, 'fleetd.db'));
  try { return fn(db); } finally { db.close(); }
}

function writeTranscript(userHome, cwd, sid) {
  const file = claudeTranscriptPath(cwd, sid, userHome);
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, '{"type":"summary"}\n');
  return file;
}

function records(file) {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8').split('\n').filter(Boolean).map(line => JSON.parse(line).parsed);
}

async function waitForRecords(file, count) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const out = records(file);
    if (out.length >= count) return out;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw new Error(`fixture did not record ${count} launches`);
}

function findCard(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

test('Claude transcript cwd munging replaces every slash and dot with a dash', () => {
  assert.equal(mungeClaudeProjectCwd('/home/me/code/fleet.deck/.worktree'),
    '-home-me-code-fleet-deck--worktree');
  assert.equal(claudeTranscriptPath('/home/me/code/fleet.deck', 'session-1', '/users/me'),
    path.join('/users/me', '.claude/projects/-home-me-code-fleet-deck/session-1.jsonl'));
});

test('revive resumes the same session into a new spawn row and its resume hook makes the card live', async (t) => {
  const daemonHome = scratch('fleetdeck-revive-daemon-');
  const userHome = scratch('fleetdeck-revive-user-');
  const cwd = scratch('fleetdeck-revive-cwd-');
  const record = path.join(userHome, 'spawn.jsonl');
  const daemon = await startDaemon({
    home: daemonHome,
    env: {
      HOME: userHome,
      FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE,
      FLEETDECK_TEST_SPAWN_RECORD: record,
    },
  });
  t.after(async () => {
    await daemon.stop({ keepHome: true });
    rmSync(daemonHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // 0.16.0: an unsupervised spawn body must echo a fresh single-use arm token.
  const arm = (await postJson(`${daemon.baseUrl}/api/spawn/arm-unsupervised`, {}, { token: daemon.token })).json.arm_token;
  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, {
    cwd, dangerously_skip_permissions: true, arm_token: arm,
  });
  assert.equal(spawned.status, 200);
  const { spawn_id: oldId, session_id: sid, callsign } = spawned.json;
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd, source: 'startup' }, { token: daemon });
  writeTranscript(userHome, cwd, sid);
  withDb(daemonHome, db => {
    db.prepare("UPDATE spawns SET status = 'gone' WHERE spawn_id = ?").run(oldId);
    db.prepare("UPDATE sessions SET col = 'offline', note = 'spawned pane window gone', ended_at = ?, archived_at = ? WHERE session_id = ?")
      .run(Date.now(), Date.now(), sid);
  });

  const revived = await postJson(`${daemon.baseUrl}/api/spawn/${oldId}/revive`, {});
  assert.equal(revived.status, 200, JSON.stringify(revived.json));
  assert.equal(revived.json.ok, true);
  assert.equal(revived.json.session_id, sid);
  assert.equal(revived.json.callsign, callsign);
  assert.notEqual(revived.json.spawn_id, oldId);

  const launchRecords = await waitForRecords(record, 2);
  const spec = launchRecords.find(item => item.revive_of === oldId);
  assert.ok(spec, 'override spec identifies the terminal row being revived');
  const claude = spec.argv.indexOf('claude');
  assert.ok(claude > 0, 'revive keeps the env-wrapper prefix');
  assert.equal(spec.argv[0], 'env');
  assert.deepEqual(spec.argv.slice(claude, claude + 3), ['claude', '--resume', sid]);
  assert.equal(spec.argv.includes('--session-id'), false);
  assert.equal(spec.argv.includes('--dangerously-skip-permissions'), true);

  const rows = withDb(daemonHome, db => db.prepare('SELECT * FROM spawns WHERE session_id = ? ORDER BY requested_at').all(sid));
  assert.equal(rows.length, 2);
  assert.equal(rows.find(row => row.spawn_id === oldId).status, 'gone');
  assert.equal(rows.find(row => row.spawn_id === revived.json.spawn_id).status, 'spawning');
  assert.equal(rows.find(row => row.spawn_id === revived.json.spawn_id).skip_permissions, 1);
  let card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.col, 'queued');
  assert.equal(card.note, 'reviving…');
  assert.ok(card.endedAt, 'revive leaves ended_at for the first hook');
  assert.equal(withDb(daemonHome, db => db.prepare('SELECT archived_at FROM sessions WHERE session_id = ?').get(sid).archived_at), null);

  await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd, source: 'resume' }, { token: daemon });
  card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.spawn.status, 'live');
  assert.equal(card.endedAt, null);
  assert.equal(card.col, 'queued');
  assert.equal(card.note, 'session resume');
});

test('revive refusals cover unknown/live/missing cwd/missing transcript/active sibling', async (t) => {
  const daemonHome = scratch('fleetdeck-revive-refuse-daemon-');
  const userHome = scratch('fleetdeck-revive-refuse-user-');
  const cwd = scratch('fleetdeck-revive-refuse-cwd-');
  const record = path.join(userHome, 'spawn.jsonl');
  const daemon = await startDaemon({
    home: daemonHome,
    env: {
      HOME: userHome,
      FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE,
      FLEETDECK_TEST_SPAWN_RECORD: record,
    },
  });
  t.after(async () => {
    await daemon.stop({ keepHome: true });
    rmSync(daemonHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  let res = await postJson(`${daemon.baseUrl}/api/spawn/${randomUUID()}/revive`, {});
  assert.equal(res.status, 404);
  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd });
  assert.equal(spawned.status, 200);
  const { spawn_id: oldId, session_id: sid, callsign } = spawned.json;

  withDb(daemonHome, db => db.prepare("UPDATE spawns SET status = 'live' WHERE spawn_id = ?").run(oldId));
  res = await postJson(`${daemon.baseUrl}/api/spawn/${oldId}/revive`, {});
  assert.equal(res.status, 409, 'a live row is not revivable');
  withDb(daemonHome, db => db.prepare("UPDATE spawns SET status = 'gone', worktree_path = ? WHERE spawn_id = ?")
    .run(path.join(cwd, 'deleted-worktree'), oldId));
  res = await postJson(`${daemon.baseUrl}/api/spawn/${oldId}/revive`, {});
  assert.equal(res.status, 410);
  assert.match(res.json.reason, /cwd/);

  withDb(daemonHome, db => db.prepare('UPDATE spawns SET worktree_path = NULL WHERE spawn_id = ?').run(oldId));
  res = await postJson(`${daemon.baseUrl}/api/spawn/${oldId}/revive`, {});
  assert.equal(res.status, 410);
  assert.match(res.json.reason, /transcript/);
  writeTranscript(userHome, cwd, sid);

  const sibling = randomUUID();
  withDb(daemonHome, db => db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, requested_at, status)
    VALUES (?, ?, ?, ?, ?, ?, ?, 'live')`).run(sibling, sid, callsign,
    spawned.json.tmux.session, spawned.json.tmux.window, cwd, Date.now() + 1));
  res = await postJson(`${daemon.baseUrl}/api/spawn/${oldId}/revive`, {});
  assert.equal(res.status, 409);
  assert.match(res.json.reason, /active spawn/);
  withDb(daemonHome, db => db.prepare("UPDATE spawns SET status = 'gone' WHERE spawn_id = ?").run(sibling));

  // ...and with every refusal cleared, a revive goes through EVEN THOUGH other
  // agents are already live. Revive used to count against FLEETDECK_MAX_SPAWNED
  // and would 409 here; there is no cap any more, so a busy fleet is not a
  // reason to refuse to bring a session back.
  const otherCwd = scratch('fleetdeck-revive-busy-cwd-');
  t.after(() => rmSync(otherCwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const other = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd: otherCwd });
  assert.equal(other.status, 200);
  res = await postJson(`${daemon.baseUrl}/api/spawn/${oldId}/revive`, {});
  assert.equal(res.status, 200, 'a live fleet must not block a revive — the cap is gone');
  assert.equal(res.json?.ok, true);
});

test('snapshot spawn.revivable follows terminal status, cwd, and transcript existence', async (t) => {
  const daemonHome = scratch('fleetdeck-revive-state-daemon-');
  const userHome = scratch('fleetdeck-revive-state-user-');
  const cwd = scratch('fleetdeck-revive-state-cwd-');
  const daemon = await startDaemon({
    home: daemonHome,
    env: { HOME: userHome, FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE },
  });
  t.after(async () => {
    await daemon.stop({ keepHome: true });
    rmSync(daemonHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd });
  const { spawn_id: spawnId, session_id: sid } = spawned.json;
  let card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.spawn.revivable, false, 'active status is never revivable');
  withDb(daemonHome, db => db.prepare("UPDATE spawns SET status = 'gone' WHERE spawn_id = ?").run(spawnId));
  card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.spawn.revivable, false, 'missing transcript keeps the flag false');
  const transcript = writeTranscript(userHome, cwd, sid);
  card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.spawn.revivable, true);
  const absentWorktree = path.join(cwd, 'removed-worktree');
  withDb(daemonHome, db => db.prepare('UPDATE spawns SET worktree_path = ? WHERE spawn_id = ?').run(absentWorktree, spawnId));
  card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.spawn.revivable, false, 'missing effective cwd flips the flag false');
  withDb(daemonHome, db => db.prepare('UPDATE spawns SET worktree_path = NULL WHERE spawn_id = ?').run(spawnId));
  card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.spawn.revivable, true, 'restoring the effective cwd flips the flag true');
  rmSync(transcript);
  card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.spawn.revivable, false);
});
