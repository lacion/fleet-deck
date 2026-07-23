import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { chmodSync, existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from '../scripts/fleetd/db.mjs';
import { createCore } from '../scripts/fleetd/derive.mjs';
import { startDaemon } from './helpers/daemon.mjs';
import { getJson, postJson } from './helpers/http.mjs';
import { waitForSpecRecords, waitUntil } from './helpers/wait.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.mjs');
try { chmodSync(FIXTURE, 0o755); } catch { /* best effort */ }

const scratch = prefix => mkdtempSync(path.join(tmpdir(), prefix));
const tmuxOk = () => {
  try { execFileSync('tmux', ['-V'], { stdio: 'ignore' }); return true; } catch { return false; }
};
const tmux = (socket, args) => execFileSync('tmux', ['-L', socket, ...args], { encoding: 'utf8' }).trim();

function overrideEnv(record) {
  return {
    FLEETDECK_SPAWN_CMD: FIXTURE,
    FLEETDECK_TEST_SPAWN_RECORD: record,
  };
}

function fakeAdapter(port = 4711) {
  const state = { windows: [], launches: [] };
  return {
    state,
    adapter: {
      spawnOverrideCmd: () => null,
      hasTmux: () => true,
      sessionName: p => `fleetdeck-${p}`,
      windowName: (p, callsign) => `fd${p}-${callsign}`,
      ensureSession: async p => `fleetdeck-${p}`,
      newWindow: async spec => {
        state.launches.push(spec);
        const shell = spec.argv.at(-1);
        const win = {
          session: `fleetdeck-${spec.port}`,
          window: `fd${spec.port}-${spec.callsign}`,
          window_id: '@1',
          pane_dead: false,
          pane_cmd: shell,
        };
        state.windows.push(win);
        return win;
      },
      listScopedWindows: async () => state.windows,
      paneCurrentCommand: async target => {
        const win = state.windows.find(w => w.window_id === target || w.window === target);
        return win ? { dead: win.pane_dead, cmd: win.pane_cmd } : null;
      },
      exactWindowTarget: (_p, window) => window,
      killWindowVerified: async name => {
        const win = state.windows.find(w => w.window === name);
        if (!win) return { gone: true };
        state.windows = state.windows.filter(w => w !== win);
        return { ok: true, window_id: win.window_id };
      },
      capturePane: async () => '',
      sendBringupEnter: async () => true,
      typeKeys: async () => true,
      sendEnter: async () => true,
      pasteText: async () => true,
      fleetServerAbsent: async () => false,
    },
  };
}

test('shell spawn validation rejects Claude/repo fields loudly', async t => {
  const cwd = scratch('fd-shell-validation-');
  const record = path.join(cwd, 'records.jsonl');
  const daemon = await startDaemon({ env: overrideEnv(record) });
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const cases = [
    [{ kind: 'other', cwd }, /kind/],
    [{ kind: 'shell', cwd, repo: 'org/repo' }, /repo/],
    [{ kind: 'shell', cwd, prompt: 'do it' }, /prompt/],
    [{ kind: 'shell', cwd, model: 'sonnet' }, /model/],
    [{ kind: 'shell', cwd, dangerously_skip_permissions: true }, /dangerously_skip_permissions/],
  ];
  for (const [body, reason] of cases) {
    const res = await postJson(`${daemon.baseUrl}/api/spawn`, body);
    assert.equal(res.status, 400);
    assert.match(res.json?.reason || '', reason);
  }
});

test('override shell is live immediately, source shell, snapshot kind shell, and mail-safe', async t => {
  const cwd = scratch('fd-shell-override-');
  const record = path.join(cwd, 'records.jsonl');
  const daemon = await startDaemon({ env: overrideEnv(record) });
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { kind: 'shell', cwd });
  assert.equal(spawned.status, 200);
  const spec = (await waitForSpecRecords(record, 1)).at(-1).parsed;
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = state.sessions.find(s => s.session_id === spawned.json.session_id);
  assert.equal(card.source, 'shell');
  assert.equal(card.col, 'idle');
  assert.equal(card.note, 'shell');
  assert.equal(card.spawn.kind, 'shell');
  assert.equal(card.spawn.status, 'live');
  assert.equal(spec.kind, 'shell');
  assert.equal(spec.argv.includes('--session-id'), false);
  assert.equal(spec.argv.at(-1), (process.env.SHELL || '').trim()
    || (existsSync('/bin/bash') ? 'bash' : 'sh'));

  const all = await postJson(`${daemon.baseUrl}/mail`, { to: 'all', from: 'ops', text: 'safe fanout' }, { token: daemon.token });
  assert.equal(all.status, 200);
  assert.equal(all.json.delivered, 0);
  const repo = await postJson(`${daemon.baseUrl}/mail`, {
    to: `repo:${card.repo_name}`,
    from: 'ops',
    text: 'safe repo fanout',
  }, { token: daemon.token });
  assert.equal(repo.status, 200);
  assert.equal(repo.json.delivered, 0);
  const direct = await postJson(`${daemon.baseUrl}/mail`, { to: card.callsign, from: 'ops', text: 'unsafe direct' }, { token: daemon.token });
  assert.equal(direct.status, 409);
  assert.equal(direct.json.reason, `${card.callsign} is a shell pane — mail would be typed into a shell`);

  const revive = await postJson(`${daemon.baseUrl}/api/spawn/${spawned.json.spawn_id}/revive`, {});
  assert.equal(revive.status, 410);
  assert.match(revive.json.reason, /no conversation to resume/);
  const remote = await postJson(`${daemon.baseUrl}/api/spawn/${spawned.json.spawn_id}/rc`, {});
  assert.equal(remote.status, 409);
  assert.match(remote.json.reason, /unavailable for shell/);
});

test('shell kill needs no force; liveness accepts any running command; retention ignores silence', async t => {
  const cwd = scratch('fd-shell-core-');
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const db = openDb(':memory:');
  t.after(() => db.close());
  const tmuxState = fakeAdapter();
  const core = createCore(db, { port: 4711, home: '/tmp/fd-shell-core-home', tmuxAdapter: tmuxState.adapter });

  const first = await core.spawn({ kind: 'shell', cwd });
  assert.equal(first.status, 200);
  const sid = first.body.session_id;
  const spawnId = first.body.spawn_id;
  tmuxState.state.windows[0].pane_cmd = 'vim';
  await core.spawnLivenessTick();
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id=?').get(spawnId).status, 'live');

  db.prepare('UPDATE sessions SET last_seen=? WHERE session_id=?').run(1, sid);
  await core.retentionSweep(Date.now() + 24 * 3_600_000);
  assert.equal(db.prepare('SELECT col FROM sessions WHERE session_id=?').get(sid).col, 'idle');

  const killed = await core.spawnKill(spawnId, false);
  assert.equal(killed.status, 200);
  assert.equal(killed.body.status, 'killed');
});

test('real tmux keeps a healthy bash shell and condemns it after exit', { skip: !tmuxOk() && 'tmux unavailable' }, async t => {
  const cwd = scratch('fd-shell-real-');
  const daemon = await startDaemon({
    env: {
      SHELL: '/bin/bash',
      FLEETDECK_AGENTS_POLL_MS: '100',
      FLEETDECK_NUDGE_MS: '60000',
    },
  });
  const socket = `fleetdeck-test-${daemon.port}`;
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true });
  });

  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { kind: 'shell', cwd });
  assert.equal(spawned.status, 200);
  await new Promise(resolve => setTimeout(resolve, 450));
  let card = (await getJson(`${daemon.baseUrl}/state`)).json.sessions
    .find(s => s.session_id === spawned.json.session_id);
  assert.equal(card.spawn.status, 'live', 'a bare bash shell is healthy for kind=shell');

  const target = `=${spawned.json.tmux.session}:=${spawned.json.tmux.window}`;
  tmux(socket, ['send-keys', '-t', target, 'exit', 'Enter']);
  card = await waitUntil(async () => {
    const s = (await getJson(`${daemon.baseUrl}/state`)).json.sessions
      .find(row => row.session_id === spawned.json.session_id);
    return s?.spawn?.status === 'pane-dead' ? s : null;
  }, { timeoutMs: 5000, label: 'shell pane condemnation after exit' });
  assert.equal(card.col, 'offline');
  assert.match(card.note, /shell pane exited/);
});
