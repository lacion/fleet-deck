// Per-card dismiss (Item 3). dismissSession() is cleanup scoped to ONE offline
// card: archive it, expire its mail + questions, gone its non-terminal spawn
// rows, kill a dead remain-on-exit window, drop its file ledger. The behavioral
// depth is tested against an in-memory core with a fake tmux adapter (the
// tests/daemon-maintenance.test.mjs pattern — direct control over spawns, mail,
// questions, touches and windows, plus observable state.killed); the HTTP route
// + status mapping is proven end-to-end against a real daemon
// (tests/cleanup-api.test.mjs pattern).

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { openDb } from '../scripts/fleetd/db.mjs';
import { createCore } from '../scripts/fleetd/derive.mjs';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';

// ----------------------------------------------------- in-memory core harness
function setEnv(t, values) {
  const before = new Map(Object.keys(values).map(k => [k, process.env[k]]));
  for (const [k, v] of Object.entries(values)) process.env[k] = String(v);
  t.after(() => {
    for (const [k, v] of before) {
      if (v === undefined) delete process.env[k];
      else process.env[k] = v;
    }
  });
}

function fakeTmux(port = 4711) {
  const state = { windows: [], killed: [] };
  const adapter = {
    spawnOverrideCmd: () => null,
    hasTmux: () => true,
    sessionName: p => `fleetdeck-${p}`,
    windowName: (p, callsign) => `fd${p}-${callsign}`,
    ensureSession: async p => `fleetdeck-${p}`,
    listScopedWindows: async () => state.windows,
    paneCurrentCommand: async target => {
      const win = state.windows.find(w => w.window_id === target || w.window === target);
      return win ? { dead: win.pane_dead, cmd: win.pane_cmd } : null;
    },
    killWindowVerified: async name => {
      state.killed.push(name);
      return { ok: true, window_id: state.windows.find(w => w.window === name)?.window_id ?? '@1' };
    },
    launchOverride: () => {},
  };
  return { state, adapter, port };
}

function memoryCore(t, { env = {}, tmux = fakeTmux(), home = '/daemon-home' } = {}) {
  setEnv(t, { FLEETDECK_NUDGE_MS: 1_000_000, ...env });
  const db = openDb(':memory:');
  const core = createCore(db, { port: tmux.port, home, tmuxAdapter: tmux.adapter });
  t.after(() => db.close());
  return { db, core, ...tmux, home };
}

// Seed an offline card (source 'hooks', ended) plus, optionally, a piece of
// undelivered mail, a pending freeform question, and a file-ledger touch.
function seedOffline(db, sid, { callsign = `${sid}-1`, now = Date.now(), mail = false, question = false, touch = false } = {}) {
  db.prepare(`INSERT INTO sessions
    (session_id, callsign, col, note, events, started_at, last_seen, ended_at, source)
    VALUES (?, ?, 'offline', 'ended', 0, ?, ?, ?, 'hooks')`).run(sid, callsign, now, now, now);
  if (mail) {
    db.prepare(`INSERT INTO mail (to_session, from_id, text, at, delivered_at, expired_at)
      VALUES (?, 'ops', 'undeliverable', ?, NULL, NULL)`).run(sid, now);
  }
  if (question) {
    db.prepare(`INSERT INTO questions (session_id, kind, payload_json, status, created_at)
      VALUES (?, 'freeform', '{"text":"still there?"}', 'pending', ?)`).run(sid, now);
  }
  if (touch) {
    db.prepare(`INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at)
      VALUES ('repo', 'src/a.js', '/repo/src/a.js', ?, '/repo', ?)`).run(sid, now);
  }
}

const touchCount = (db, sid) => db.prepare('SELECT COUNT(*) AS n FROM file_touches WHERE session_id = ?').get(sid).n;
const spawnStatus = (db, id) => db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(id)?.status;

test('dismiss archives one offline card, expires its mail + questions, gones its spawn row, drops its ledger', async (t) => {
  const { db, core } = memoryCore(t);
  const now = Date.now();
  const sid = 'off-full';
  seedOffline(db, sid, { now, mail: true, question: true, touch: true });
  // A non-terminal spawn row whose window is simply absent from tmux (no kill
  // to do) — dismiss must still flip it 'gone' so it stops counting as active.
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
    VALUES ('sp-full', ?, 'off-full-1', 'fleetdeck-4711', 'fd4711-off-full-1', ?, 'live')`).run(sid, now);
  assert.equal(touchCount(db, sid), 1, 'sanity: the card starts with a file-ledger touch');

  const out = await core.dismissSession(sid);
  assert.equal(out.status, 200, JSON.stringify(out.body));
  assert.deepEqual(out.body, { ok: true, archived: 1, mail_expired: 1, questions_expired: 1, windows_killed: 0 });

  assert.equal(core.snapshot().sessions.some(s => s.session_id === sid), false, 'the card leaves the board');
  assert.ok(db.prepare('SELECT archived_at FROM sessions WHERE session_id = ?').get(sid).archived_at, 'archived');
  assert.ok(db.prepare('SELECT expired_at FROM mail WHERE to_session = ?').get(sid).expired_at, 'mail expired');
  assert.equal(db.prepare('SELECT status FROM questions WHERE session_id = ?').get(sid).status, 'expired', 'question expired');
  assert.equal(spawnStatus(db, 'sp-full'), 'gone', 'the non-terminal spawn row is goned');
  assert.equal(touchCount(db, sid), 0, 'the file ledger is dropped so the radar cannot argue with a corpse');
});

test('dismiss kills a dead remain-on-exit window this card owns', async (t) => {
  const tmux = fakeTmux();
  const { db, core, state } = memoryCore(t, { tmux });
  const now = Date.now();
  const sid = 'off-deadpane';
  seedOffline(db, sid, { now });
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
    VALUES ('sp-dead', ?, 'off-deadpane-1', 'fleetdeck-4711', 'fd4711-off-deadpane-1', ?, 'pane-dead')`).run(sid, now);
  // The window still exists in tmux but its pane is dead (remain-on-exit) —
  // the exact case cleanup kills at retention.mjs:193-198.
  state.windows.push({
    session: 'fleetdeck-4711', window: 'fd4711-off-deadpane-1', window_id: '@9', pane_dead: true, pane_cmd: 'claude',
  });

  const out = await core.dismissSession(sid);
  assert.equal(out.status, 200, JSON.stringify(out.body));
  assert.equal(out.body.windows_killed, 1, 'the dead window is killed');
  assert.deepEqual(state.killed, ['fd4711-off-deadpane-1'], 'and killed by its exact scoped name');
  // A terminal ('pane-dead') spawn row is left terminal, never flipped to gone.
  assert.equal(spawnStatus(db, 'sp-dead'), 'pane-dead');
});

test('dismiss leaves a LIVE window alone (only dead panes are killed)', async (t) => {
  const tmux = fakeTmux();
  const { db, core, state } = memoryCore(t, { tmux });
  const now = Date.now();
  const sid = 'off-livepane';
  seedOffline(db, sid, { now });
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
    VALUES ('sp-live', ?, 'off-livepane-1', 'fleetdeck-4711', 'fd4711-off-livepane-1', ?, 'live')`).run(sid, now);
  state.windows.push({
    session: 'fleetdeck-4711', window: 'fd4711-off-livepane-1', window_id: '@3', pane_dead: false, pane_cmd: 'claude',
  });

  const out = await core.dismissSession(sid);
  assert.equal(out.status, 200);
  assert.equal(out.body.windows_killed, 0, 'a window whose pane is still a live claude is never killed here');
  assert.deepEqual(state.killed, []);
});

test('dismiss refuses a card that is not offline (409)', async (t) => {
  const { db, core } = memoryCore(t);
  const now = Date.now();
  db.prepare(`INSERT INTO sessions
    (session_id, callsign, col, note, events, started_at, last_seen, source)
    VALUES ('live-card', 'live-1', 'working', 'busy', 0, ?, ?, 'hooks')`).run(now, now);

  const out = await core.dismissSession('live-card');
  assert.equal(out.status, 409);
  assert.match(out.body.reason, /working.*not offline/);
  assert.equal(db.prepare("SELECT archived_at FROM sessions WHERE session_id = 'live-card'").get().archived_at, null,
    'a refused dismiss must not archive anything');
});

test('dismiss 404s an unknown session id', async (t) => {
  const { core } = memoryCore(t);
  const out = await core.dismissSession(randomUUID());
  assert.equal(out.status, 404);
  assert.match(out.body.reason, /no such session/);
});

test('a second dismiss is a 409 already-dismissed, not a double archive', async (t) => {
  const { db, core } = memoryCore(t);
  seedOffline(db, 'off-twice');
  const first = await core.dismissSession('off-twice');
  assert.equal(first.status, 200);
  const at = db.prepare("SELECT archived_at FROM sessions WHERE session_id = 'off-twice'").get().archived_at;

  const second = await core.dismissSession('off-twice');
  assert.equal(second.status, 409);
  assert.match(second.body.reason, /already dismissed/);
  assert.equal(db.prepare("SELECT archived_at FROM sessions WHERE session_id = 'off-twice'").get().archived_at, at,
    'the archive timestamp is not rewritten by a second dismiss');
});

test('dismiss refuses a card whose spawn is stalled (consistency with archiveCandidates)', async (t) => {
  const { db, core } = memoryCore(t);
  const now = Date.now();
  const sid = 'off-stalled';
  seedOffline(db, sid, { now });
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
    VALUES ('sp-stalled', ?, 'off-stalled-1', 'fleetdeck-4711', 'fd4711-off-stalled-1', ?, 'stalled')`).run(sid, now);

  const out = await core.dismissSession(sid);
  assert.equal(out.status, 409);
  assert.match(out.body.reason, /stalled/);
  assert.equal(db.prepare('SELECT archived_at FROM sessions WHERE session_id = ?').get(sid).archived_at, null,
    'a stalled card is left entirely untouched');
  assert.equal(spawnStatus(db, 'sp-stalled'), 'stalled', 'and its spawn row is not goned');
});

// -------------------------------------------------------- HTTP route wiring
test('POST /api/sessions/:id/dismiss dismisses an offline card end-to-end', async (t) => {
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); });
  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd: process.cwd(), source: 'startup' }, { token: daemon.token });
  await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'ops', text: 'undeliverable' }, { token: daemon.token });
  await postHook(daemon.baseUrl, 'SessionEnd', { session_id: sid, cwd: process.cwd(), reason: 'done' }, { token: daemon.token });

  const res = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/dismiss`, {});
  assert.equal(res.status, 200, res.text);
  assert.deepEqual(Object.keys(res.json).sort(), ['archived', 'mail_expired', 'ok', 'questions_expired', 'windows_killed']);
  assert.equal(res.json.ok, true);
  assert.equal(res.json.archived, 1);
  assert.equal(res.json.mail_expired, 1);

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.sessions.some(s => s.session_id === sid), false, 'the dismissed card is gone from /state');

  // A second dismiss is a clean 409, not a 500 or a double archive.
  const again = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/dismiss`, {});
  assert.equal(again.status, 409);
  assert.match(again.json.reason, /already dismissed/);
});

test('POST /api/sessions/:id/dismiss 404s an unknown id and 409s a live card', async (t) => {
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); });

  const unknown = await postJson(`${daemon.baseUrl}/api/sessions/${randomUUID()}/dismiss`, {});
  assert.equal(unknown.status, 404);
  assert.match(unknown.json.reason, /no such session/);

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd: process.cwd(), source: 'startup' }, { token: daemon.token });
  const live = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/dismiss`, {});
  assert.equal(live.status, 409, live.text);
  assert.match(live.json.reason, /not offline/);
});
