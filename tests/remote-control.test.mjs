import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { openDb } from '../scripts/fleetd/db.mjs';
import { claudeTranscriptPath, createCore } from '../scripts/fleetd/derive.mjs';

function setEnv(t, values) {
  const before = new Map(Object.keys(values).map(key => [key, process.env[key]]));
  for (const [key, value] of Object.entries(values)) process.env[key] = String(value);
  t.after(() => {
    for (const [key, value] of before) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  });
}

function fakeTmux(port = 4711) {
  const state = { windows: [], launches: [], calls: [], captureText: '', override: false, overrideSpec: null };
  const adapter = {
    spawnOverrideCmd: () => state.override ? '/fake-spawn-override' : null,
    hasTmux: () => true,
    sessionName: p => `fleetdeck-${p}`,
    windowName: (p, callsign) => `fd${p}-${callsign}`,
    ensureSession: async p => `fleetdeck-${p}`,
    newWindow: async spec => {
      state.launches.push(spec.argv);
      const window = `fd${spec.port}-${spec.callsign}`;
      const win = {
        session: `fleetdeck-${spec.port}`, window,
        window_id: `@${state.windows.length + 1}`, pane_dead: false, pane_cmd: 'claude',
      };
      state.windows.push(win);
      return { session: win.session, window, window_id: win.window_id };
    },
    listScopedWindows: async () => state.windows,
    paneCurrentCommand: async target => {
      const win = state.windows.find(item => item.window_id === target || item.window === target);
      return win ? { dead: win.pane_dead, cmd: win.pane_cmd } : null;
    },
    typeKeys: async (target, text) => {
      state.calls.push(['typeKeys', target, text]);
      return true;
    },
    sendEnter: async target => {
      state.calls.push(['sendEnter', target]);
      return true;
    },
    sendBringupEnter: async () => true,
    capturePane: async target => {
      state.calls.push(['capturePane', target]);
      return state.captureText;
    },
    pasteText: async () => true,
    killWindowVerified: async name => {
      const hit = state.windows.find(item => item.window === name);
      if (hit) hit.pane_dead = true;
      return { ok: true, window_id: hit?.window_id ?? '@0' };
    },
    launchOverride: (_cmd, spec) => { state.overrideSpec = spec; },
  };
  return { adapter, state, port };
}

test('legacy spawn tables migrate remote-control columns additively', (t) => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fd-rc-db-'));
  const file = path.join(dir, 'fleet.db');
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const legacy = new DatabaseSync(file);
  legacy.exec(`CREATE TABLE spawns (
    spawn_id TEXT PRIMARY KEY, session_id TEXT, callsign TEXT,
    tmux_session TEXT, tmux_window TEXT, cwd TEXT, worktree_path TEXT,
    requested_at INTEGER, status TEXT DEFAULT 'spawning',
    skip_permissions INTEGER DEFAULT 0
  );
  INSERT INTO spawns (spawn_id, session_id, callsign, requested_at, status)
    VALUES ('legacy', 'sid', 'otter', 1, 'gone')`);
  legacy.close();
  const db = openDb(file);
  t.after(() => db.close());
  const columns = db.prepare('PRAGMA table_info(spawns)').all().map(row => row.name);
  assert.ok(columns.includes('remote_control'));
  assert.ok(columns.includes('remote_url'));
  const row = db.prepare("SELECT remote_control, remote_url FROM spawns WHERE spawn_id = 'legacy'").get();
  assert.equal(row.remote_control, 0);
  assert.equal(row.remote_url, null);
});

test('spawn rejects non-boolean remote_control', async (t) => {
  const { core, cwd } = harness(t);
  const out = await core.spawn({ cwd, remote_control: 'yes' });
  assert.deepEqual(out, {
    status: 400,
    body: { ok: false, reason: 'remote_control must be a boolean' },
  });
});

function harness(t, { port = 4711 } = {}) {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-rc-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-rc-cwd-'));
  t.after(() => rmSync(userHome, { recursive: true, force: true }));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  setEnv(t, {
    HOME: userHome,
    FLEETDECK_NUDGE_MS: 1_000_000,
    FLEETDECK_PANE_MAIL_GRACE_MS: 1_000_000,
    FLEETDECK_RC_HARVEST_MS: 0,
  });
  const tmux = fakeTmux(port);
  const db = openDb(':memory:');
  const core = createCore(db, { port, home: '/fleet-home', tmuxAdapter: tmux.adapter });
  t.after(() => db.close());
  return { db, core, cwd, userHome, ...tmux };
}

test('remote spawn persists intent, exposes snapshot state, and orders argv before prompt', async (t) => {
  const { db, core, cwd, state, port } = harness(t, { port: 4788 });
  const prompt = 'inspect remote telemetry';
  const out = await core.spawn({
    cwd, prompt, model: 'sonnet', dangerously_skip_permissions: true, remote_control: true,
  });
  assert.equal(out.status, 200);
  const row = db.prepare('SELECT * FROM spawns WHERE spawn_id = ?').get(out.body.spawn_id);
  assert.equal(row.remote_control, 1);
  assert.equal(row.remote_url, null);
  const argv = state.launches[0];
  const remoteAt = argv.indexOf('--remote-control');
  // Named by CALLSIGN — this is the string the human reads on claude.ai.
  assert.deepEqual(argv.slice(remoteAt, remoteAt + 2), ['--remote-control', out.body.callsign]);
  assert.ok(remoteAt > argv.indexOf('--dangerously-skip-permissions'));
  assert.ok(remoteAt < argv.indexOf(prompt));
  assert.deepEqual(core.snapshot().sessions.find(s => s.session_id === out.body.session_id).spawn.remote,
    { enabled: true, url: null });
});

test('spawn override spec carries remote-control intent and full argv', async (t) => {
  const { core, cwd, state } = harness(t);
  state.override = true;
  const out = await core.spawn({ cwd, prompt: 'remote prompt', remote_control: true });
  assert.equal(out.status, 200);
  assert.equal(state.overrideSpec.remote_control, true);
  const remoteAt = state.overrideSpec.argv.indexOf('--remote-control');
  assert.deepEqual(state.overrideSpec.argv.slice(remoteAt, remoteAt + 2),
    ['--remote-control', out.body.callsign]);
  assert.ok(remoteAt < state.overrideSpec.argv.indexOf('remote prompt'));
});

test('revive inherits remote control after resume args', async (t) => {
  const { db, core, cwd, userHome, state } = harness(t);
  const original = await core.spawn({ cwd, remote_control: true });
  const { spawn_id, session_id } = original.body;
  const transcript = claudeTranscriptPath(cwd, session_id, userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{}\n');
  db.prepare("UPDATE spawns SET status = 'killed' WHERE spawn_id = ?").run(spawn_id);
  state.windows[0].pane_dead = true;

  const revived = await core.revive(spawn_id);
  assert.equal(revived.status, 200);
  const argv = state.launches.at(-1);
  const resumeAt = argv.indexOf('--resume');
  const remoteAt = argv.indexOf('--remote-control');
  assert.deepEqual(argv.slice(resumeAt, resumeAt + 2), ['--resume', session_id]);
  assert.deepEqual(argv.slice(remoteAt, remoteAt + 2), ['--remote-control', original.body.callsign]);
  assert.ok(remoteAt > resumeAt + 1);
  const row = db.prepare('SELECT * FROM spawns WHERE spawn_id = ?').get(revived.body.spawn_id);
  assert.equal(row.remote_control, 1);
  assert.equal(row.remote_url, null, 'the old link died with the old session');
});

test('revive honors a remote_control override in the body, both ways', async (t) => {
  const { db, core, cwd, userHome, state } = harness(t);
  // Spawned WITHOUT remote control; the human asks for it on the way back.
  const original = await core.spawn({ cwd });
  const { spawn_id, session_id } = original.body;
  const transcript = claudeTranscriptPath(cwd, session_id, userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{}\n');
  db.prepare("UPDATE spawns SET status = 'gone' WHERE spawn_id = ?").run(spawn_id);
  state.windows[0].pane_dead = true;

  const on = await core.revive(spawn_id, { remote_control: true });
  assert.equal(on.status, 200);
  const argv = state.launches.at(-1);
  assert.deepEqual(argv.slice(argv.indexOf('--remote-control'), argv.indexOf('--remote-control') + 2),
    ['--remote-control', original.body.callsign]);
  assert.equal(db.prepare('SELECT remote_control FROM spawns WHERE spawn_id = ?')
    .get(on.body.spawn_id).remote_control, 1);

  // ...and the reverse: a remote-enabled row revived with the wish switched off.
  db.prepare("UPDATE spawns SET status = 'gone' WHERE spawn_id = ?").run(on.body.spawn_id);
  state.windows.at(-1).pane_dead = true;
  const off = await core.revive(on.body.spawn_id, { remote_control: false });
  assert.equal(off.status, 200);
  assert.equal(state.launches.at(-1).includes('--remote-control'), false);
  assert.equal(db.prepare('SELECT remote_control FROM spawns WHERE spawn_id = ?')
    .get(off.body.spawn_id).remote_control, 0);
});

test('enableRemote types literal /rc, harvests URL, updates snapshot, and is idempotent with URL', async (t) => {
  const { db, core, cwd, state, port } = harness(t, { port: 4799 });
  const spawned = await core.spawn({ cwd });
  const { spawn_id, session_id, callsign } = spawned.body;

  let out = await core.enableRemote(spawn_id);
  assert.equal(out.status, 409);
  assert.match(out.body.reason, /spawning, not live/);

  db.prepare("UPDATE spawns SET status = 'live' WHERE spawn_id = ?").run(spawn_id);
  db.prepare("UPDATE sessions SET col = 'working' WHERE session_id = ?").run(session_id);
  out = await core.enableRemote(spawn_id);
  assert.equal(out.status, 409);
  assert.match(out.body.reason, /working/);

  db.prepare("UPDATE sessions SET col = 'idle' WHERE session_id = ?").run(session_id);
  state.captureText = 'Remote Control\nhttps://claude.ai/code/session_abc?source=rc\n';
  out = await core.enableRemote(spawn_id);
  assert.deepEqual(out, {
    status: 200,
    body: { ok: true, enabled: true, url: 'https://claude.ai/code/session_abc?source=rc', pending: false },
  });
  assert.deepEqual(state.calls, [
    ['typeKeys', '@1', `/rc ${callsign}`],
    ['sendEnter', '@1'],
    ['capturePane', `fd${port}-${callsign}`],
  ]);
  assert.equal(db.prepare('SELECT remote_control FROM spawns WHERE spawn_id = ?').get(spawn_id).remote_control, 1);
  assert.equal(core.snapshot().sessions.find(s => s.session_id === session_id).spawn.remote.url,
    'https://claude.ai/code/session_abc?source=rc');

  const before = state.calls.length;
  const again = await core.enableRemote(spawn_id);
  assert.deepEqual(again.body, {
    ok: true, enabled: true, url: 'https://claude.ai/code/session_abc?source=rc', pending: false,
  });
  assert.equal(state.calls.length, before, 'stored URL makes repeated enable idempotent');
});

test('enableRemote remains enabled with a null URL when capture has no link', async (t) => {
  const { db, core, cwd, state } = harness(t);
  const spawned = await core.spawn({ cwd });
  db.prepare("UPDATE spawns SET status = 'live' WHERE spawn_id = ?").run(spawned.body.spawn_id);
  db.prepare("UPDATE sessions SET col = 'queued' WHERE session_id = ?").run(spawned.body.session_id);
  state.captureText = 'Remote Control enabled\n[QR-only panel]\n';

  const out = await core.enableRemote(spawned.body.spawn_id);
  assert.deepEqual(out.body, { ok: true, enabled: true, url: null, pending: false });
  const row = db.prepare('SELECT remote_control, remote_url FROM spawns WHERE spawn_id = ?').get(spawned.body.spawn_id);
  assert.equal(row.remote_control, 1);
  assert.equal(row.remote_url, null);
  assert.deepEqual(core.snapshot().sessions.find(s => s.session_id === spawned.body.session_id).spawn.remote,
    { enabled: true, url: null });
});

test('first registration hook schedules one best-effort harvest for born-remote spawn', async (t) => {
  const { db, core, cwd, state } = harness(t);
  state.captureText = 'open https://claude.ai/code/born_remote now';
  const spawned = await core.spawn({ cwd, remote_control: true });
  core.applyEvent({ session_id: spawned.body.session_id, hook_event_name: 'Notification' });
  await new Promise(resolve => setTimeout(resolve, 20));
  const row = db.prepare('SELECT status, remote_url FROM spawns WHERE spawn_id = ?').get(spawned.body.spawn_id);
  assert.equal(row.status, 'live');
  assert.equal(row.remote_url, 'https://claude.ai/code/born_remote');
  assert.equal(state.calls.filter(call => call[0] === 'capturePane').length, 1);
});
