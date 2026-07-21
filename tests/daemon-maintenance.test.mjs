import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync, chmodSync, readFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { openDb } from '../scripts/fleetd/db.mjs';
import { claudeTranscriptPath, createCore } from '../scripts/fleetd/derive.mjs';
import { capturePane, pasteText, sendEnter, typeKeys } from '../scripts/fleetd/spawn.mjs';

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
  const state = {
    windows: [], argv: null, calls: [], pasteOk: true, enterOk: true, killed: [],
  };
  const adapter = {
    spawnOverrideCmd: () => null,
    hasTmux: () => true,
    sessionName: p => `fleetdeck-${p}`,
    windowName: (p, callsign) => `fd${p}-${callsign}`,
    ensureSession: async p => `fleetdeck-${p}`,
    newWindow: async spec => {
      state.argv = spec.argv;
      const win = {
        session: `fleetdeck-${spec.port}`,
        window: `fd${spec.port}-${spec.callsign}`,
        window_id: '@1', pane_dead: false, pane_cmd: 'claude',
      };
      state.windows.push(win);
      return { session: win.session, window: win.window, window_id: win.window_id };
    },
    listScopedWindows: async () => state.windows,
    paneCurrentCommand: async target => {
      const win = state.windows.find(w => w.window_id === target || w.window === target);
      return win ? { dead: win.pane_dead, cmd: win.pane_cmd } : null;
    },
    pasteText: async (target, text) => {
      state.calls.push(['pasteText', target, text]);
      return state.pasteOk;
    },
    sendEnter: async target => {
      state.calls.push(['sendEnter', target]);
      return state.enterOk;
    },
    sendBringupEnter: async target => {
      state.calls.push(['sendBringupEnter', target]);
      return true;
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
  setEnv(t, { FLEETDECK_NUDGE_MS: 1_000_000, FLEETDECK_PANE_MAIL_GRACE_MS: 1_000_000, ...env });
  const db = openDb(':memory:');
  const core = createCore(db, { port: tmux.port, home, tmuxAdapter: tmux.adapter });
  t.after(() => db.close());
  return { db, core, ...tmux, home };
}

test('spawn argv is deterministic and registration watchdog stalls once, then a late hook revives it', async (t) => {
  const { db, core, state, port, home } = memoryCore(t, {
    env: { FLEETDECK_SPAWN_REGISTER_MS: 1 },
  });
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-watchdog-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  const out = await core.spawn({ cwd, model: 'sonnet', permission_mode: 'acceptEdits', prompt: 'do it' });
  assert.equal(out.status, 200);
  const scrub = [
    'CLAUDECODE', 'CLAUDE_CODE_SESSION_ID', 'CLAUDE_CODE_CHILD_SESSION',
    'CLAUDE_CODE_BRIDGE_SESSION_ID', 'CLAUDE_CODE_ENTRYPOINT', 'CLAUDE_CODE_EXECPATH',
    'CLAUDE_ENV_FILE', 'CLAUDE_PROJECT_DIR', 'CLAUDE_PLUGIN_ROOT', 'CLAUDE_PLUGIN_DATA',
    'CLAUDE_EFFORT', 'AI_AGENT', 'CODEX_COMPANION_TRANSCRIPT_PATH',
    'CODEX_COMPANION_SESSION_ID', 'FLEETDECK_AGENTS_CMD', 'FLEETDECK_SPAWN_CMD',
    'FLEETDECK_TERM_CMD',
    'TMUX', 'TMUX_PANE', 'FLEETDECK_TMUX_SOCKET',
    'FLEETDECK_AGENTS_POLL_MS', 'FLEETDECK_HOLD_MS', 'FLEETDECK_STALE_MS',
    'FLEETDECK_NUDGE_MS', 'FLEETDECK_WATCH_MAX_MS',
    'FLEETDECK_WATCH_POLL_MS', 'FLEETDECK_SPAWN_REGISTER_MS',
    'FLEETDECK_PANE_MAIL_GRACE_MS', 'FLEETDECK_PRESUME_DEAD_MS',
    'FLEETDECK_RETAIN_OFFLINE_MS',
    'FLEETDECK_RC_HARVEST_MS',
    'FLEETDECK_ADOPT_ARM_MS', 'FLEETDECK_ADOPT_DELAY_MS',
    'FLEETDECK_TEST_DAEMON_SCRIPT', 'FLEETDECK_VERSION_OVERRIDE',
    // 0.16.0: the daemon's bearer never leaks into a spawned pane — see the
    // identical note in spawn.test.mjs.
    'FLEETDECK_TOKEN',
    // 0.15.0 LLM gateway — see the identical note in spawn.test.mjs.
    'ANTHROPIC_BASE_URL', 'ANTHROPIC_AUTH_TOKEN', 'ANTHROPIC_API_KEY',
    'CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY',
  ];
  const prefix = ['env', ...scrub.flatMap(v => ['-u', v]), `FLEETDECK_PORT=${port}`, `FLEETDECK_HOME=${home}`];
  assert.deepEqual(state.argv.slice(0, prefix.length), prefix);
  assert.deepEqual(state.argv.slice(prefix.length), [
    'claude', '--session-id', out.body.session_id,
    // '--' terminates option parsing so a prompt that looks like a flag
    // (e.g. --dangerously-skip-permissions) is a positional, not a claude flag.
    '--model', 'sonnet', '--permission-mode', 'acceptEdits', '--', 'do it',
  ]);

  await new Promise(resolve => setTimeout(resolve, 5));
  await core.spawnLivenessTick();
  let card = core.snapshot().sessions.find(s => s.session_id === out.body.session_id);
  assert.equal(card.spawn.status, 'stalled');
  assert.equal(card.spawn.stalled, true);
  assert.equal(card.col, 'needsyou', 'a stalled spawn must land in the loud lane');
  assert.equal(card.notification_type, 'spawn_stalled');
  assert.match(card.note, /pane up but never registered.*window/);
  assert.ok(core.snapshot().ticker.some(x => /never phoned home/.test(x.msg)));
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM events WHERE hook_event = 'SpawnStalled'").get().n, 1);

  await core.spawnLivenessTick();
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM events WHERE hook_event = 'SpawnStalled'").get().n, 1,
    'stalled rows must not emit the watchdog event repeatedly');
  core.hookSessionStart({ session_id: out.body.session_id, cwd, source: 'startup' });
  card = core.snapshot().sessions.find(s => s.session_id === out.body.session_id);
  assert.equal(card.spawn.status, 'live', 'the first late hook must win over stalled');
});

test('revive reuses the env wrapper, kills a dead remnant, inserts a new row, and the resume hook marks it live', async (t) => {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-revive-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-revive-cwd-'));
  t.after(() => {
    rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const { db, core, state, port, home } = memoryCore(t, {
    env: { HOME: userHome },
  });
  // 0.16.0: unsupervised spawns need a fresh single-use arm token — the core
  // harness mints one directly (the HTTP arm route is bearer-gated).
  const original = await core.spawn({ cwd, dangerously_skip_permissions: true, arm_token: core.armUnsupervised() });
  const { spawn_id: oldId, session_id: sid } = original.body;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });
  db.prepare("UPDATE spawns SET status = 'gone' WHERE spawn_id = ?").run(oldId);
  db.prepare("UPDATE sessions SET col = 'offline', note = 'spawned pane window gone', ended_at = ?, archived_at = ? WHERE session_id = ?")
    .run(Date.now(), Date.now(), sid);
  const transcript = claudeTranscriptPath(cwd, sid, userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{}\n');

  // BUG 3: a gone row whose window still hosts a LIVE claude pane is now
  // ADOPTED by revive (resurrected in place, no duplicate) instead of a
  // dead-end 409 — that behavior has its own coverage in fleet-bugs.test.mjs.
  // This test exercises the RELAUNCH path, so first mark the remnant pane dead:
  // a dead remnant is killed and a fresh row launched.
  state.windows[0].pane_dead = true;
  const out = await core.revive(oldId);
  assert.equal(out.status, 200);
  assert.notEqual(out.body.spawn_id, oldId);
  assert.deepEqual(state.killed, [original.body.tmux.window]);
  const prefix = state.argv.slice(0, state.argv.indexOf('claude'));
  assert.deepEqual(prefix.slice(-2), [`FLEETDECK_PORT=${port}`, `FLEETDECK_HOME=${home}`]);
  assert.deepEqual(state.argv.slice(state.argv.indexOf('claude')),
    ['claude', '--resume', sid, '--dangerously-skip-permissions']);
  assert.equal(state.argv.includes('--session-id'), false);

  const rows = db.prepare('SELECT * FROM spawns WHERE session_id = ? ORDER BY requested_at, rowid').all(sid);
  assert.equal(rows.length, 2);
  assert.equal(rows[0].status, 'gone');
  assert.equal(rows[1].status, 'spawning');
  let session = db.prepare('SELECT * FROM sessions WHERE session_id = ?').get(sid);
  assert.equal(session.col, 'queued');
  assert.equal(session.note, 'reviving…');
  assert.equal(session.archived_at, null);
  assert.ok(session.ended_at);

  core.hookSessionStart({ session_id: sid, cwd, source: 'resume' });
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(out.body.spawn_id).status, 'live');
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(oldId).status, 'gone');
  session = db.prepare('SELECT * FROM sessions WHERE session_id = ?').get(sid);
  assert.equal(session.ended_at, null);
  assert.equal(session.col, 'queued');
  assert.equal(session.note, 'session resume');
});

test('owned-pane mail honors watcher priority and unclaims all rows when paste fails', async (t) => {
  const { db, core, state } = memoryCore(t);
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mail-pane-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  let posted = await core.postMail({ to: sid, from: 'ops', text: 'line one\nline two' });
  assert.equal(posted.targets[0].route, 'pane');
  assert.equal(await core.tryOwnedPaneDelivery(sid), true);
  assert.deepEqual(state.calls, [
    ['pasteText', '@1', '[FLEETDECK MAIL from ops] line one\nline two'],
    ['sendEnter', '@1'],
  ]);
  assert.ok(db.prepare('SELECT delivered_at FROM mail ORDER BY id LIMIT 1').get().delivered_at);
  assert.equal(core.snapshot().mail_meta[sid].queued, 0);

  state.calls.length = 0;
  const unregister = core.addWatchWaiter(sid, () => {});
  // 'human' is a reserved sender (0.16.0) — postMail refuses it even in-process.
  posted = await core.postMail({ to: sid, from: 'operator', text: 'watcher first' });
  assert.equal(posted.targets[0].route, 'watcher');
  assert.equal(await core.tryOwnedPaneDelivery(sid), false);
  assert.deepEqual(state.calls, [], 'a registered waiter suppresses pane paste');
  unregister();
  core.drainMail(sid);

  state.pasteOk = false;
  await core.postMail({ to: sid, from: 'ops', text: 'retry me' });
  assert.equal(await core.tryOwnedPaneDelivery(sid), false);
  const failed = db.prepare("SELECT * FROM mail WHERE text = 'retry me'").get();
  assert.equal(failed.delivered_at, null, 'paste failure must put every claimed row back');
  assert.deepEqual(state.calls, [['pasteText', '@1', '[FLEETDECK MAIL from ops] retry me']]);
});

test('tmux input/capture helpers use isolated-socket argv without shell interpolation', async (t) => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fd-tmux-argv-'));
  const record = path.join(dir, 'argv.jsonl');
  const shim = path.join(dir, 'tmux');
  writeFileSync(shim, `#!/usr/bin/env node\nimport { appendFileSync } from 'node:fs';\nappendFileSync(process.env.FD_TMUX_RECORD, JSON.stringify(process.argv.slice(2)) + '\\n');\n`);
  chmodSync(shim, 0o755);
  setEnv(t, {
    PATH: `${dir}${path.delimiter}${process.env.PATH}`,
    FD_TMUX_RECORD: record,
    FLEETDECK_TMUX_SOCKET: 'fd-test-socket',
  });
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  assert.equal(await pasteText('@9', 'hello\nworld'), true);
  assert.equal(await sendEnter('@9'), true);
  assert.equal(await typeKeys('@9', '/rc fd4711-falcon'), true);
  assert.equal(await capturePane('fd4711-falcon'), '');
  const calls = readFileSync(record, 'utf8').trim().split('\n').map(JSON.parse);
  // pasteText now uses a per-call unique buffer (fdmail-<uuid>, H-R4) and deletes
  // it in a finally, so assert the argv shape and the buffer-name relationship
  // rather than a fixed name — the isolated socket + no-shell contract is intact.
  const [setBuf, pasteBuf, delBuf, ...rest] = calls;
  const bufName = setBuf[4];
  assert.match(bufName, /^fdmail-[0-9a-f-]+$/, 'set-buffer uses a unique fdmail-<uuid> buffer');
  assert.deepEqual(setBuf, ['-L', 'fd-test-socket', 'set-buffer', '-b', bufName, '--', 'hello\nworld']);
  assert.deepEqual(pasteBuf, ['-L', 'fd-test-socket', 'paste-buffer', '-p', '-d', '-b', bufName, '-t', '@9']);
  assert.deepEqual(delBuf, ['-L', 'fd-test-socket', 'delete-buffer', '-b', bufName]);
  assert.deepEqual(rest, [
    ['-L', 'fd-test-socket', 'send-keys', '-t', '@9', 'Enter'],
    ['-L', 'fd-test-socket', 'send-keys', '-t', '@9', '-l', '--', '/rc fd4711-falcon'],
    ['-L', 'fd-test-socket', 'capture-pane', '-p', '-t', 'fd4711-falcon'],
  ]);
});

test('retention presumes dead, archives, expires mail, hides archived rows, and resurrects late hooks', (t) => {
  setEnv(t, { FLEETDECK_NUDGE_MS: 1_000_000, FLEETDECK_PANE_MAIL_GRACE_MS: 1_000_000 });
  const db = openDb(':memory:');
  t.after(() => db.close());
  const now = Date.now();
  const insert = db.prepare(`INSERT INTO sessions
    (session_id, callsign, col, note, events, started_at, last_seen, ended_at, source)
    VALUES (?, ?, ?, ?, 0, ?, ?, ?, 'hooks')`);
  insert.run('silent', 'silent-1', 'idle', 'waiting', now - 4 * 3_600_000, now - 4 * 3_600_000, null);
  insert.run('old-offline', 'old-1', 'offline', 'ended', now - 30 * 3_600_000, now - 30 * 3_600_000, now - 30 * 3_600_000);
  db.prepare(`INSERT INTO mail (to_session, from_id, text, at, delivered_at, expired_at)
    VALUES ('old-offline', 'ops', 'old', ?, NULL, NULL)`).run(now);
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
    VALUES ('sp-old', 'old-offline', 'old-1', 'fleetdeck-4711', 'fd4711-old-1', ?, 'live')`).run(now - 30 * 3_600_000);

  const core = createCore(db, { port: 4711, home: '/home', tmuxAdapter: fakeTmux().adapter });
  const silent = db.prepare("SELECT * FROM sessions WHERE session_id = 'silent'").get();
  assert.equal(silent.col, 'offline');
  assert.ok(silent.ended_at);
  assert.match(silent.note, /^presumed ended \(silent .+h\)$/);
  assert.ok(db.prepare("SELECT archived_at FROM sessions WHERE session_id = 'old-offline'").get().archived_at);
  assert.ok(db.prepare("SELECT expired_at FROM mail WHERE to_session = 'old-offline'").get().expired_at);
  assert.equal(db.prepare("SELECT status FROM spawns WHERE spawn_id = 'sp-old'").get().status, 'gone');
  assert.equal(core.snapshot().sessions.some(s => s.session_id === 'old-offline'), false);

  core.applyEvent({ session_id: 'silent', hook_event_name: 'UserPromptSubmit', prompt: 'still here' });
  const revived = db.prepare("SELECT * FROM sessions WHERE session_id = 'silent'").get();
  assert.equal(revived.ended_at, null);
  assert.equal(revived.archived_at, null);
  assert.equal(revived.col, 'working');
});

test('cleanup archives offline rows, expires mail, kills eligible dead panes, and only lists worktrees', async (t) => {
  const tmux = fakeTmux();
  const { db, core, state } = memoryCore(t, { tmux });
  const now = Date.now();
  db.prepare(`INSERT INTO sessions
    (session_id, callsign, col, note, events, started_at, last_seen, ended_at, source)
    VALUES ('offline', 'off-1', 'offline', 'ended', 0, ?, ?, ?, 'hooks')`).run(now, now, now);
  db.prepare(`INSERT INTO mail (to_session, from_id, text, at, delivered_at, expired_at)
    VALUES ('offline', 'ops', 'pending', ?, NULL, NULL)`).run(now);
  // One worktree still on disk (must be listed), one already hand-removed
  // (must be silently dropped — cleanup only nags about real chores).
  const wt = mkdtempSync(path.join(tmpdir(), 'fd-wt-off-'));
  t.after(() => rmSync(wt, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, worktree_path, requested_at, status)
    VALUES ('sp-off', 'offline', 'off-1', 'fleetdeck-4711', 'fd4711-off-1', ?, ?, 'pane-dead')`).run(wt, now);
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, worktree_path, requested_at, status)
    VALUES ('sp-gone', 'offline', 'off-1', 'fleetdeck-4711', 'fd4711-off-gone', '/tmp/fd-wt-off-already-removed', ?, 'gone')`).run(now);
  state.windows.push({
    session: 'fleetdeck-4711', window: 'fd4711-off-1', window_id: '@7', pane_dead: true, pane_cmd: 'claude',
  });

  const out = await core.cleanup();
  assert.deepEqual(out, {
    ok: true, archived: 1, mail_expired: 1, questions_expired: 0, questions_purged: 0,
    conflicts_cleared: 0, feed_cleared: out.feed_cleared, windows_killed: 1,
    orphan_worktrees: [wt],
  });
  assert.ok(out.feed_cleared >= 0, 'Clear wipes the feed too');
  assert.deepEqual(state.killed, ['fd4711-off-1']);
  assert.equal(core.snapshot().sessions.some(s => s.session_id === 'offline'), false);
});
