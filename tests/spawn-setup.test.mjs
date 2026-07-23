import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { chmodSync, mkdtempSync, rmSync } from 'node:fs';
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

const EXPECTED_WRAPPER = [
  'cmd=$FLEETDECK_SETUP_CMD; unset FLEETDECK_SETUP_CMD',
  'printf \'▶ fleetdeck setup: %s\\n\' "$cmd"',
  'sh -c "$cmd"; rc=$?',
  'if [ "$rc" -ne 0 ]; then printf \'✗ setup failed (exit %s) — claude not started\\n\' "$rc"; exit "$rc"; fi',
  'exec "$@"',
].join('\n');

const scratch = prefix => mkdtempSync(path.join(tmpdir(), prefix));
const tmuxOk = () => {
  try { execFileSync('tmux', ['-V'], { stdio: 'ignore' }); return true; } catch { return false; }
};
const tmux = (socket, args) => execFileSync('tmux', ['-L', socket, ...args], { encoding: 'utf8' }).trim();
const overrideEnv = record => ({
  FLEETDECK_SPAWN_CMD: FIXTURE,
  FLEETDECK_TEST_SPAWN_RECORD: record,
});

test('injected adapter receives setup through env and the fixed wrapper argv', async t => {
  const cwd = scratch('fd-setup-core-');
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  let launched;
  let window;
  const adapter = {
    spawnOverrideCmd: () => null,
    hasTmux: () => true,
    sessionName: p => `fleetdeck-${p}`,
    windowName: (p, callsign) => `fd${p}-${callsign}`,
    ensureSession: async () => 'fleetdeck-4711',
    newWindow: async spec => {
      launched = spec;
      window = {
        session: 'fleetdeck-4711', window: `fd4711-${spec.callsign}`,
        window_id: '@1', pane_dead: false, pane_cmd: 'sh',
      };
      return window;
    },
    listScopedWindows: async () => window ? [window] : [],
    paneCurrentCommand: async () => window
      ? { dead: window.pane_dead, cmd: window.pane_cmd }
      : null,
    killWindowVerified: async () => ({ ok: true }),
    capturePane: async () => '',
    sendBringupEnter: async () => true,
    fleetServerAbsent: async () => false,
  };
  const db = openDb(':memory:');
  t.after(() => db.close());
  const core = createCore(db, { port: 4711, home: '/tmp/fd-setup-core-home', tmuxAdapter: adapter });
  const setup = 'printf "%s" "$(id)"';
  const spawned = await core.spawn({ cwd, setup_cmd: setup, prompt: '--literal' });
  assert.equal(spawned.status, 200);
  assert.equal(launched.env.FLEETDECK_SETUP_CMD, setup);
  assert.equal(launched.argv.includes(setup), false);
  const sh = launched.argv.indexOf('sh');
  assert.deepEqual(launched.argv.slice(sh), [
    'sh', '-c', EXPECTED_WRAPPER, 'fleetdeck-setup',
    'claude', '--session-id', spawned.body.session_id, '--', '--literal',
  ]);
  const settings = core.setSettings({ repo_setup: { fleetdeck: setup } });
  assert.equal(settings.status, 200);
  assert.deepEqual(core.snapshot().settings.repo_setup, { fleetdeck: setup });
  assert.equal(core.setSettings({ repo_setup: { fleetdeck: 'x'.repeat(2001) } }).status, 400);

  await core.spawnLivenessTick();
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id=?').get(spawned.body.spawn_id).status, 'spawning',
    'a live sh setup phase is not mistaken for Claude having exited');
  window.pane_dead = true;
  await core.spawnLivenessTick();
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id=?').get(spawned.body.spawn_id).status, 'pane-dead',
    'pane_dead during setup condemns immediately without hysteresis');
  assert.equal(db.prepare('SELECT note FROM sessions WHERE session_id=?').get(spawned.body.session_id).note,
    'pane exited during setup/bring-up — open the terminal for the error');
});

test('setup_cmd body validation rejects bad types, size, controls, and shell combination', async t => {
  const cwd = scratch('fd-setup-validation-');
  const daemon = await startDaemon({ env: overrideEnv(path.join(cwd, 'records.jsonl')) });
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const cases = [
    [{ cwd, setup_cmd: 42 }, /must be a string/],
    [{ cwd, setup_cmd: 'x'.repeat(2001) }, /2000/],
    [{ cwd, setup_cmd: 'ok\u0000bad' }, /control/],
    [{ cwd, setup_cmd: 'ok\tbad' }, /control/],
    [{ kind: 'shell', cwd, setup_cmd: 'echo no' }, /Claude-only/],
  ];
  for (const [body, reason] of cases) {
    const res = await postJson(`${daemon.baseUrl}/api/spawn`, body);
    assert.equal(res.status, 400);
    assert.match(res.json?.reason || '', reason);
  }
});

test('setup wrapper is fixed, Claude argv stays positional, and setup text rides env only', async t => {
  const cwd = scratch('fd-setup-override-');
  const record = path.join(cwd, 'records.jsonl');
  const daemon = await startDaemon({ env: overrideEnv(record) });
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const hostile = 'printf \'"quoted"; $HOME $(id) `whoami`\\n\'';
  const prompt = '--dangerously-skip-permissions; $(touch never)';
  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, {
    cwd,
    setup_cmd: hostile,
    prompt,
    model: 'sonnet',
    permission_mode: 'acceptEdits',
  });
  assert.equal(spawned.status, 200);
  const spec = (await waitForSpecRecords(record, 1)).at(-1).parsed;
  assert.equal(spec.setup_cmd, hostile);
  assert.equal(spec.env.FLEETDECK_SETUP_CMD, hostile);
  assert.equal(spec.gateway_env, null);
  assert.equal(spec.argv.some(arg => arg === hostile || arg.includes(hostile)), false,
    'user setup text must never enter pane argv or shell source');

  const sh = spec.argv.indexOf('sh');
  assert.ok(sh > 0);
  assert.deepEqual(spec.argv.slice(sh), [
    'sh', '-c', EXPECTED_WRAPPER, 'fleetdeck-setup',
    'claude', '--session-id', spawned.json.session_id,
    '--model', 'sonnet',
    '--permission-mode', 'acceptEdits',
    '--', prompt,
  ]);
  assert.equal(spec.argv.at(-2), '--');
  assert.equal(spec.argv.at(-1), prompt);
  assert.equal(spec.argv.includes('-u')
    && spec.argv.some((arg, i) => arg === 'FLEETDECK_SETUP_CMD' && spec.argv[i - 1] === '-u'), false,
  'the launch keeps its deliberate setup env until the fixed wrapper unsets it');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = state.sessions.find(s => s.session_id === spawned.json.session_id);
  assert.equal(card.spawn.setup_cmd, hostile);
});

test('repo_setup validates, persists, and rides settings broadcasts', async t => {
  const home = scratch('fd-setup-settings-home-');
  const first = await startDaemon({ home });
  t.after(async () => {
    if (first.proc.exitCode == null) await first.stop({ keepHome: true });
    rmSync(home, { recursive: true, force: true });
  });

  const value = { fleetdeck: 'super code\npython -m venv .venv' };
  const saved = await postJson(`${first.baseUrl}/api/settings`, { repo_setup: value });
  assert.equal(saved.status, 200);
  assert.deepEqual(saved.json.settings.repo_setup, value);
  const state = await getJson(`${first.baseUrl}/state`);
  assert.deepEqual(state.json.settings.repo_setup, value);

  for (const body of [
    { repo_setup: [] },
    { repo_setup: { fleetdeck: 7 } },
    { repo_setup: { fleetdeck: 'x'.repeat(2001) } },
    { repo_setup: Object.fromEntries(Array.from({ length: 51 }, (_, i) => [`repo-${i}`, 'true'])) },
  ]) {
    const rejected = await postJson(`${first.baseUrl}/api/settings`, body);
    assert.equal(rejected.status, 400);
  }

  await first.stop({ keepHome: true });
  const second = await startDaemon({ port: first.port, home });
  t.after(async () => { await second.stop({ keepHome: true }); });
  const restored = await getJson(`${second.baseUrl}/api/settings`);
  assert.deepEqual(restored.json.settings.repo_setup, value);
});

test('real tmux setup failure stays visible, condemns immediately, and never starts Claude', { skip: !tmuxOk() && 'tmux unavailable' }, async t => {
  const cwd = scratch('fd-setup-fail-');
  const daemon = await startDaemon({
    env: {
      FLEETDECK_AGENTS_POLL_MS: '100',
      FLEETDECK_NUDGE_MS: '60000',
      FLEETDECK_SETUP_REGISTER_MS: '1000',
    },
  });
  const socket = `fleetdeck-test-${daemon.port}`;
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true });
  });

  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, setup_cmd: 'exit 7' });
  assert.equal(spawned.status, 200);
  const card = await waitUntil(async () => {
    const s = (await getJson(`${daemon.baseUrl}/state`)).json.sessions
      .find(row => row.session_id === spawned.json.session_id);
    return s?.spawn?.status === 'pane-dead' ? s : null;
  }, { timeoutMs: 5000, label: 'setup failure condemnation' });
  assert.equal(card.note, 'pane exited during setup/bring-up — open the terminal for the error');

  const target = `=${spawned.json.tmux.session}:=${spawned.json.tmux.window}`;
  // -S - : include scrollback. tmux moves earlier lines (the ▶ banner) into
  // history on the detached pane by the time it dies; the visible screen keeps
  // the ✗ failure line, which is the human-facing guarantee — the banner is
  // asserted from history.
  const screen = tmux(socket, ['capture-pane', '-p', '-S', '-', '-t', target]);
  assert.match(screen, /fleetdeck setup: exit 7/);
  assert.match(screen, /setup failed \(exit 7\) — claude not started/);
  assert.doesNotMatch(screen, /claude: .*not found/);
});

test('real tmux long-running setup is not condemned while sh/setup binary runs', { skip: !tmuxOk() && 'tmux unavailable' }, async t => {
  const cwd = scratch('fd-setup-sleep-');
  const daemon = await startDaemon({
    env: {
      FLEETDECK_AGENTS_POLL_MS: '100',
      FLEETDECK_NUDGE_MS: '60000',
      FLEETDECK_SETUP_REGISTER_MS: '250',
    },
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true });
  });

  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, setup_cmd: 'sleep 30' });
  assert.equal(spawned.status, 200);
  await new Promise(resolve => setTimeout(resolve, 750));
  const card = (await getJson(`${daemon.baseUrl}/state`)).json.sessions
    .find(s => s.session_id === spawned.json.session_id);
  assert.notEqual(card.spawn.status, 'pane-dead');
  assert.ok(card.spawn.status === 'spawning' || card.spawn.status === 'stalled');
  if (card.spawn.status === 'stalled') assert.match(card.note, /setup may still be running/);
});
