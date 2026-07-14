import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from '../scripts/fleetd/db.mjs';
import { claudeTranscriptPath } from '../scripts/fleetd/derive.mjs';
import { startDaemon } from './helpers/daemon.mjs';
import { getJson, postHook, postJson } from './helpers/http.mjs';

// tests/adopt.test.mjs — the daemon side of "Move to tmux". Adopt resumes a
// session the board did NOT spawn into a board-owned `claude --resume` pane,
// with an arm-then-adopt flow for still-live sessions. Modeled on
// tests/revive.test.mjs: a per-test random-port daemon over the
// FLEETDECK_SPAWN_CMD fixture, argv captured to a JSONL record, hook events
// POSTed over HTTP. FLEETDECK_ADOPT_DELAY_MS=0 so the armed auto-adopt fires on
// the next tick instead of after the 750 ms production grace.

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPAWN_CMD_FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.mjs');
try { chmodSync(SPAWN_CMD_FIXTURE, 0o755); } catch { /* best effort */ }

function scratch(prefix = 'fleetdeck-adopt-') {
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

const sleep = ms => new Promise(r => setTimeout(r, ms));

function findCard(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

// Boot a daemon over the spawn fixture with adopt's delay collapsed to 0 (the
// grace-window tests override it), and register full teardown (kill + the
// per-test tmux server + scratch dirs). The env is returned so restart tests
// can boot a second daemon against the SAME home with identical knobs.
async function boot(t, prefix, extraEnv = {}) {
  const daemonHome = scratch(`${prefix}-daemon-`);
  const userHome = scratch(`${prefix}-user-`);
  const cwd = scratch(`${prefix}-cwd-`);
  const record = path.join(userHome, 'spawn.jsonl');
  const env = {
    HOME: userHome,
    FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE,
    FLEETDECK_TEST_SPAWN_RECORD: record,
    FLEETDECK_ADOPT_DELAY_MS: '0',
    ...extraEnv,
  };
  const daemon = await startDaemon({ home: daemonHome, env });
  t.after(async () => {
    await daemon.stop({ keepHome: true });
    rmSync(daemonHome, { recursive: true, force: true });
    rmSync(userHome, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  });
  return { daemon, daemonHome, userHome, cwd, record, env };
}

function tickerHas(state, needle) {
  return (state.ticker ?? []).some(r => String(r.msg ?? '').includes(needle));
}

// A live source='hooks' card the board never spawned: one SessionStart hook for
// a fresh session id, no spawn row.
async function startLiveSession(daemon, cwd) {
  const sid = randomUUID();
  const started = await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd, source: 'startup' });
  return { sid, callsign: started.json.callsign };
}

// Drive a live hooks session all the way to a hook-PROVEN offline end.
async function endSession(daemon, sid, cwd, reason = 'logout') {
  await postHook(daemon.baseUrl, 'SessionEnd', { session_id: sid, cwd, reason });
}

test('adopt moves an ended session into a board-owned pane and its resume hook makes the card live', async (t) => {
  const { daemon, daemonHome, userHome, cwd, record } = await boot(t, 'fleetdeck-adopt-now');
  const { sid, callsign } = await startLiveSession(daemon, cwd);
  writeTranscript(userHome, cwd, sid);
  await endSession(daemon, sid, cwd);

  // Offline + hook-proven end + cwd + transcript → snapshot offers adopt NOW.
  let card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.col, 'offline');
  assert.equal(card.adopt.eligible, 'now');
  assert.equal(card.adopt.armed, false);
  assert.equal(card.spawn, undefined, 'a hooks session the board never spawned has no spawn descriptor');

  const adopted = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  assert.equal(adopted.status, 200, JSON.stringify(adopted.json));
  assert.equal(adopted.json.ok, true);
  assert.equal(adopted.json.adopted, true);
  assert.equal(adopted.json.session_id, sid);
  assert.equal(adopted.json.callsign, callsign);
  assert.ok(adopted.json.spawn_id, 'adopt attaches a new spawn row');

  // The launch is `claude --resume <sid>` behind the env wrapper — the SAME
  // session id, no --session-id, no bypass flag by default.
  const [spec] = await waitForRecords(record, 1);
  const claude = spec.argv.indexOf('claude');
  assert.ok(claude > 0, 'adopt keeps the env-wrapper prefix');
  assert.equal(spec.argv[0], 'env');
  assert.deepEqual(spec.argv.slice(claude, claude + 3), ['claude', '--resume', sid]);
  assert.equal(spec.argv.includes('--session-id'), false);
  assert.equal(spec.argv.includes('--dangerously-skip-permissions'), false);
  assert.equal(spec.argv.includes('--remote-control'), false, 'adopt never sets --remote-control in v1');

  // The spawn row: worktree_path stays NULL (adopt never creates a worktree),
  // skip_permissions 0, and it went live-eligible ('spawning').
  const row = withDb(daemonHome, db => db.prepare('SELECT * FROM spawns WHERE session_id = ? ORDER BY requested_at DESC LIMIT 1').get(sid));
  assert.equal(row.spawn_id, adopted.json.spawn_id);
  assert.equal(row.status, 'spawning');
  assert.equal(row.worktree_path, null);
  assert.equal(row.skip_permissions, 0);

  // The card returned to QUEUED with the move note; ended_at is LEFT for the
  // first resume hook to clear.
  card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.col, 'queued');
  assert.equal(card.note, 'moving to tmux…');
  assert.ok(card.endedAt, 'adopt leaves ended_at for the first hook');

  // The resume pane's first hook lands on the SAME card and makes it live.
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd, source: 'resume' });
  card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.spawn.status, 'live');
  assert.equal(card.endedAt, null);
});

test('adopt of an unknown session is 404', async (t) => {
  const { daemon } = await boot(t, 'fleetdeck-adopt-404');
  const res = await postJson(`${daemon.baseUrl}/api/sessions/${randomUUID()}/adopt`, {});
  assert.equal(res.status, 404);
  assert.match(res.json.reason, /no such session/);
});

test('adopt on a live session arms it, snapshot shows the arm, and re-arming refreshes the deadline', async (t) => {
  const { daemon, daemonHome, cwd } = await boot(t, 'fleetdeck-adopt-arm');
  const { sid } = await startLiveSession(daemon, cwd);

  // A live hooks card offers ARM, not now.
  let card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.adopt.eligible, 'arm');
  assert.equal(card.adopt.armed, false);

  const armed = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  assert.equal(armed.status, 200);
  assert.equal(armed.json.armed, true);
  assert.ok(armed.json.expires_at > Date.now(), 'arm returns a future deadline');

  // Durable and snapshot-visible.
  const row1 = withDb(daemonHome, db => db.prepare('SELECT adopt_armed_until, adopt_armed_skip FROM sessions WHERE session_id = ?').get(sid));
  assert.ok(row1.adopt_armed_until > Date.now());
  assert.equal(row1.adopt_armed_skip, 0, 'safe default: no bypass stored');
  card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.adopt.armed, true);
  assert.equal(card.adopt.armed_skip, false);
  assert.equal(card.adopt.armed_until, armed.json.expires_at);

  await sleep(5);
  const rearmed = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  assert.equal(rearmed.status, 200);
  assert.equal(rearmed.json.armed, true);
  assert.ok(rearmed.json.expires_at >= armed.json.expires_at, 're-arm refreshes the deadline');
});

test('disarming an armed session clears the arm columns', async (t) => {
  const { daemon, daemonHome, cwd } = await boot(t, 'fleetdeck-adopt-disarm');
  const { sid } = await startLiveSession(daemon, cwd);
  await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, { dangerously_skip_permissions: true });
  let stored = withDb(daemonHome, db => db.prepare('SELECT adopt_armed_until, adopt_armed_skip FROM sessions WHERE session_id = ?').get(sid));
  assert.ok(stored.adopt_armed_until > Date.now());
  assert.equal(stored.adopt_armed_skip, 1);

  const disarmed = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, { disarm: true });
  assert.equal(disarmed.status, 200);
  assert.equal(disarmed.json.armed, false);
  stored = withDb(daemonHome, db => db.prepare('SELECT adopt_armed_until, adopt_armed_skip FROM sessions WHERE session_id = ?').get(sid));
  assert.equal(stored.adopt_armed_until, null);
  assert.equal(stored.adopt_armed_skip, null);
  const card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.adopt.armed, false);
  assert.equal(card.adopt.eligible, 'arm', 'still a live arm candidate after disarm');
});

test('an armed session auto-adopts on SessionEnd and carries the bypass flag through to the resume argv', async (t) => {
  const { daemon, daemonHome, userHome, cwd, record } = await boot(t, 'fleetdeck-adopt-bypass');
  const { sid } = await startLiveSession(daemon, cwd);
  writeTranscript(userHome, cwd, sid);
  const armed = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, { dangerously_skip_permissions: true });
  assert.equal(armed.json.armed, true);

  // The CLI exits — the deferred adopt fires (ADOPT_DELAY_MS=0).
  await endSession(daemon, sid, cwd);
  const [spec] = await waitForRecords(record, 1);
  const claude = spec.argv.indexOf('claude');
  assert.deepEqual(spec.argv.slice(claude, claude + 3), ['claude', '--resume', sid]);
  assert.equal(spec.argv.includes('--dangerously-skip-permissions'), true, 'the arm-time bypass choice survived to SessionEnd');

  // The arm is one-shot: both columns are cleared, and the spawn row records
  // the bypass.
  const cleared = withDb(daemonHome, db => db.prepare('SELECT adopt_armed_until, adopt_armed_skip FROM sessions WHERE session_id = ?').get(sid));
  assert.equal(cleared.adopt_armed_until, null);
  assert.equal(cleared.adopt_armed_skip, null);
  const row = withDb(daemonHome, db => db.prepare('SELECT skip_permissions FROM spawns WHERE session_id = ? ORDER BY requested_at DESC LIMIT 1').get(sid));
  assert.equal(row.skip_permissions, 1);
});

test('an armed session without the bypass flag resumes with no --dangerously-skip-permissions (safe default)', async (t) => {
  const { daemon, userHome, cwd, record } = await boot(t, 'fleetdeck-adopt-safe');
  const { sid } = await startLiveSession(daemon, cwd);
  writeTranscript(userHome, cwd, sid);
  await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  await endSession(daemon, sid, cwd);
  const [spec] = await waitForRecords(record, 1);
  const claude = spec.argv.indexOf('claude');
  assert.deepEqual(spec.argv.slice(claude, claude + 3), ['claude', '--resume', sid]);
  assert.equal(spec.argv.includes('--dangerously-skip-permissions'), false, 'default is supervised');
});

test('a /clear on an armed session keeps it live and armed and spawns no pane', async (t) => {
  const { daemon, daemonHome, userHome, cwd, record } = await boot(t, 'fleetdeck-adopt-clear');
  const { sid } = await startLiveSession(daemon, cwd);
  writeTranscript(userHome, cwd, sid);
  const armed = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  assert.equal(armed.json.armed, true);

  // /clear is NOT an end: the SessionEnd(reason:'clear') early-return keeps the
  // card live and the arm intact, and fires no adopt.
  await postHook(daemon.baseUrl, 'SessionEnd', { session_id: sid, cwd, reason: 'clear' });
  await sleep(400); // an ADOPT_DELAY_MS=0 adopt would have recorded by now

  assert.equal(records(record).length, 0, 'no pane launched by a /clear');
  const stored = withDb(daemonHome, db => db.prepare('SELECT adopt_armed_until FROM sessions WHERE session_id = ?').get(sid));
  assert.ok(stored.adopt_armed_until > Date.now(), 'the arm survives /clear');
  const card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.endedAt, null, 'the card stays live after /clear');
  assert.equal(card.adopt.armed, true);
});

test('adopt refuses a presumed-dead session and tells you to arm it instead', async (t) => {
  const { daemon, daemonHome, userHome, cwd } = await boot(t, 'fleetdeck-adopt-presumed');
  const { sid } = await startLiveSession(daemon, cwd);
  writeTranscript(userHome, cwd, sid);
  // Retention's silence guess: offline, ended, end_reason='presumed'.
  withDb(daemonHome, db => db.prepare("UPDATE sessions SET col='offline', ended_at=?, end_reason='presumed' WHERE session_id=?").run(Date.now(), sid));

  // The snapshot does NOT offer adopt-now for a presumed-dead card.
  const card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.adopt.eligible, null);

  const res = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  assert.equal(res.status, 409);
  assert.match(res.json.reason, /presumed dead|arm it instead/i);
});

test('adopt refuses a session that already has an active spawn row (board-owned never adopts)', async (t) => {
  const { daemon, daemonHome, userHome, cwd } = await boot(t, 'fleetdeck-adopt-active');
  const { sid, callsign } = await startLiveSession(daemon, cwd);
  writeTranscript(userHome, cwd, sid);
  await endSession(daemon, sid, cwd);
  // Attach a live spawn row directly (as if a pane were already coming up).
  withDb(daemonHome, db => db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, requested_at, status)
    VALUES (?, ?, ?, ?, ?, ?, ?, 'live')`).run(randomUUID(), sid, callsign,
    `fleetdeck-${daemon.port}`, `fd${daemon.port}-${callsign}`, cwd, Date.now()));

  const res = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  assert.equal(res.status, 409);
  assert.match(res.json.reason, /board-owned/);
});

test('adopt refuses a DEAD board-owned lineage too — revive owns it, and the snapshot never offers both buttons', async (t) => {
  const { daemon, daemonHome, userHome, cwd } = await boot(t, 'fleetdeck-adopt-deadrow');
  const { sid, callsign } = await startLiveSession(daemon, cwd);
  writeTranscript(userHome, cwd, sid);
  await endSession(daemon, sid, cwd);
  // A pane-dead lineage: the board once owned a pane for this session. Its cwd
  // and transcript still exist, so spawn.revivable is TRUE — the card must
  // offer ⟲ revive, never ALSO ⇥ move-to-tmux (a second lineage would fight
  // the first over the window name and worktree bookkeeping).
  withDb(daemonHome, db => db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, requested_at, status)
    VALUES (?, ?, ?, ?, ?, ?, ?, 'pane-dead')`).run(randomUUID(), sid, callsign,
    `fleetdeck-${daemon.port}`, `fd${daemon.port}-${callsign}`, cwd, Date.now()));

  const card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.spawn.revivable, true, 'the dead lineage is the revive path');
  assert.equal(card.adopt.eligible, null, 'never both buttons');

  const res = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  assert.equal(res.status, 409);
  assert.match(res.json.reason, /board-owned/);
});

test('an unstamped end (NULL end_reason, e.g. a pre-0.7.0 row) is never adopt-now-eligible', async (t) => {
  const { daemon, daemonHome, userHome, cwd } = await boot(t, 'fleetdeck-adopt-null-reason');
  const { sid } = await startLiveSession(daemon, cwd);
  writeTranscript(userHome, cwd, sid);
  // Simulate a pre-upgrade offline row: ended_at set, end_reason never stamped.
  // NULL is "no proof", not "proven": adopt-now must refuse (the CLI might
  // still be running — resuming it would duplicate a billed session).
  withDb(daemonHome, db => db.prepare("UPDATE sessions SET col='offline', ended_at=?, end_reason=NULL WHERE session_id=?").run(Date.now(), sid));

  const card = findCard((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.adopt.eligible, null);

  const res = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  assert.equal(res.status, 409);
  assert.match(res.json.reason, /hook-proven|arm it instead/i);
});

test('a deferred adopt whose session came back live CANCELS the move — it never re-arms', async (t) => {
  const { daemon, daemonHome, userHome, cwd, record } = await boot(t, 'fleetdeck-adopt-resurrect', { FLEETDECK_ADOPT_DELAY_MS: '250' });
  const { sid } = await startLiveSession(daemon, cwd);
  writeTranscript(userHome, cwd, sid);
  await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  // The CLI exits… and the session is resumed by hand INSIDE the grace window
  // (the resurrection race). The one-shot arm must not become a standing order.
  await endSession(daemon, sid, cwd);
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd, source: 'resume' });
  await sleep(600); // let the 250 ms deferred adopt fire and decide

  assert.equal(records(record).length, 0, 'no pane was launched');
  const stored = withDb(daemonHome, db => db.prepare('SELECT adopt_armed_until, adopt_armed_skip FROM sessions WHERE session_id = ?').get(sid));
  assert.equal(stored.adopt_armed_until, null, 'the arm was consumed, not renewed');
  assert.equal(stored.adopt_armed_skip, null);
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(findCard(state, sid).adopt.armed, false);
  assert.ok(tickerHas(state, 'canceled'), 'the cancel is said once in the ticker');
});

test('a disarm landing inside the deferred grace window genuinely cancels the move', async (t) => {
  const { daemon, userHome, cwd, record } = await boot(t, 'fleetdeck-adopt-lategrace', { FLEETDECK_ADOPT_DELAY_MS: '250' });
  const { sid } = await startLiveSession(daemon, cwd);
  writeTranscript(userHome, cwd, sid);
  await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  await endSession(daemon, sid, cwd);
  // The human clicks the still-rendered armed chip to cancel, beating the timer.
  const disarmed = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, { disarm: true });
  assert.equal(disarmed.json.armed, false);
  await sleep(600);

  assert.equal(records(record).length, 0, 'the cancel won — no pane launched');
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(tickerHas(state, '✗ move-to-tmux failed'), false, 'a stood-down deferred adopt is not a failure');
});

test('a manual adopt-now click racing the deferred timer wins silently — one pane, no false failure line', async (t) => {
  const { daemon, userHome, cwd, record } = await boot(t, 'fleetdeck-adopt-race', { FLEETDECK_ADOPT_DELAY_MS: '250' });
  const { sid } = await startLiveSession(daemon, cwd);
  writeTranscript(userHome, cwd, sid);
  await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  await endSession(daemon, sid, cwd);
  // Manual click beats the 250 ms timer: it consumes the arm and launches.
  const manual = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  assert.equal(manual.status, 200, JSON.stringify(manual.json));
  assert.equal(manual.json.adopted, true);
  await waitForRecords(record, 1);
  await sleep(600); // the deferred adopt fires into the new lineage and stands down

  assert.equal(records(record).length, 1, 'exactly one pane — the deferred call stood down');
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(tickerHas(state, '✗ move-to-tmux failed'), false, 'a benign 409 race is not shouted as a failure');
});

test('an armed move orphaned by a daemon death inside the grace window is completed by the next boot\'s sweep', async (t) => {
  // A huge delay stands in for "the daemon died before the timer fired": the
  // arm is durable, the timer is not.
  const { daemon, daemonHome, userHome, cwd, record, env } = await boot(t, 'fleetdeck-adopt-orphan', { FLEETDECK_ADOPT_DELAY_MS: '600000' });
  const { sid } = await startLiveSession(daemon, cwd);
  writeTranscript(userHome, cwd, sid);
  await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  await endSession(daemon, sid, cwd);

  // The arm survives the end (consumed by the adopt, not the trigger) — and
  // the daemon dies before its far-future timer can fire.
  const stored = withDb(daemonHome, db => db.prepare('SELECT adopt_armed_until FROM sessions WHERE session_id = ?').get(sid));
  assert.ok(stored.adopt_armed_until > Date.now(), 'the arm is still durable after SessionEnd');
  await daemon.stop({ keepHome: true });
  assert.equal(records(record).length, 0, 'nothing launched before the death');

  // Next boot: the retention sweep finds the orphaned arm (ended, hook-proven,
  // unexpired, no lineage) and completes the human's move.
  const revived = await startDaemon({ home: daemonHome, env });
  t.after(() => revived.stop({ keepHome: true }));
  const [spec] = await waitForRecords(record, 1);
  const claude = spec.argv.indexOf('claude');
  assert.deepEqual(spec.argv.slice(claude, claude + 3), ['claude', '--resume', sid]);
  const cleared = withDb(daemonHome, db => db.prepare('SELECT adopt_armed_until, adopt_armed_skip FROM sessions WHERE session_id = ?').get(sid));
  assert.equal(cleared.adopt_armed_until, null, 'the sweep-fired adopt consumed the arm');
  assert.equal(cleared.adopt_armed_skip, null);
});

test('adopt 410s when the resume transcript or the cwd is gone', async (t) => {
  const { daemon, userHome, cwd } = await boot(t, 'fleetdeck-adopt-410');
  const { sid } = await startLiveSession(daemon, cwd);
  await endSession(daemon, sid, cwd);

  // No transcript on disk yet → 410 transcript (cwd still exists).
  let res = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  assert.equal(res.status, 410);
  assert.match(res.json.reason, /transcript/);

  // Write the transcript, then remove the cwd → 410 cwd (checked first).
  writeTranscript(userHome, cwd, sid);
  rmSync(cwd, { recursive: true, force: true });
  res = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  assert.equal(res.status, 410);
  assert.match(res.json.reason, /cwd/);
});

test('adopt validates the request body', async (t) => {
  const { daemon, cwd } = await boot(t, 'fleetdeck-adopt-validate');
  const { sid } = await startLiveSession(daemon, cwd);
  let res = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, { dangerously_skip_permissions: 'yes' });
  assert.equal(res.status, 400);
  assert.match(res.json.reason, /dangerously_skip_permissions must be a boolean/);
  res = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, { disarm: 'please' });
  assert.equal(res.status, 400);
  assert.match(res.json.reason, /disarm must be a boolean/);
});
