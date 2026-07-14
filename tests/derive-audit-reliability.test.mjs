// tests/derive-audit-reliability.test.mjs
//
// Regression tests for the derive.mjs audit-cleanup wave (owned by the
// derive.mjs agent). Each test names the audit finding it pins:
//   H-R1  removeWorktree must not rm -rf uncommitted work without force
//   H-R2  boot reconciliation must not tombstone the fleet on a tmux hiccup
//   H-R5  a stale spawn_id must not kill a newer revived session
//   H-R6  a launch failure must leave no orphan worktree/window/row
//   H-R7  revive validates eligibility BEFORE touching tmux
//   M-B2  a PreToolUse conflict whisper carries hookEventName:'PreToolUse'
//   M-B4  a corrupt conflicts row must not 500 /state
//   M-B5  a resurrected card leaves the offline column on tool activity
//   M-B6  ExitPlanMode question + plan are inserted atomically
//   M-B8  a guarded remote harvest never rejects
//   M-G1  the append-only ledgers are aged out; snapshot windows touches
//   R2-5  a stale-id kill during a revive's window creation is refused
//   R2-6  snapshot/cleanup drop a conflicts row whose sessions_json is
//         valid JSON of the wrong shape ('{}', 'null', a string)
//   R2-7  the per-card file cap keeps a card's NEWEST touches, not oldest
//   R2-8  removeWorktree's FINAL git-status guard refuses a TOCTOU-dirty tree
//
// These drive createCore() directly with an injected fake tmux adapter and an
// in-memory SQLite db — the same harness shape as daemon-maintenance.test.mjs,
// but self-contained (no edits to tests/helpers/*).

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { openDb } from '../scripts/fleetd/db.mjs';
import { claudeTranscriptPath, createCore } from '../scripts/fleetd/derive.mjs';

const HOUR = 3_600_000;

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

// A controllable stand-in for scripts/fleetd/spawn.mjs. Defaults model a
// reachable tmux with one live claude window created per newWindow(); override
// any method per test. state.killed / state.windows / state.calls are the
// observable surface.
function makeAdapter(port = 4711, overrides = {}) {
  const state = { windows: [], killed: [], calls: [], argv: null };
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
      const w = state.windows.find(x => x.window_id === target || x.window === target);
      return w ? { dead: w.pane_dead, cmd: w.pane_cmd } : null;
    },
    killWindowVerified: async name => {
      state.killed.push(name);
      return { ok: true, window_id: '@1' };
    },
    pasteText: async () => true,
    sendEnter: async () => true,
    typeKeys: async () => true,
    sendBringupEnter: async () => true,
    capturePane: async () => '',
    launchOverride: () => {},
    ...overrides,
  };
  return { state, adapter, port };
}

// createCore with the maintenance-timer knobs pinned high so no unref timer
// interferes with a synchronous assertion.
function memoryCore(t, { tmux = makeAdapter(), home = '/daemon-home', env = {} } = {}) {
  setEnv(t, { FLEETDECK_NUDGE_MS: 1_000_000, FLEETDECK_PANE_MAIL_GRACE_MS: 1_000_000, ...env });
  const db = openDb(':memory:');
  t.after(() => { try { db.close(); } catch { /* already closed */ } });
  const core = createCore(db, { port: tmux.port, home, tmuxAdapter: tmux.adapter });
  return { db, core, ...tmux, home };
}

function git(args, cwd) {
  return execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}

// A throwaway repo with one commit; ready for `git worktree add`.
function initRepo(t, name = 'repo') {
  const base = mkdtempSync(path.join(tmpdir(), 'fd-derive-git-'));
  const root = path.join(base, name);
  mkdirSync(root, { recursive: true });
  git(['init', '-q', '-b', 'main'], root);
  git(['config', 'user.email', 't@fleetdeck.local'], root);
  git(['config', 'user.name', 'Fleet Deck Tests'], root);
  writeFileSync(path.join(root, 'a.txt'), 'one\n');
  git(['add', '-A'], root);
  git(['commit', '-qm', 'base'], root);
  t.after(() => rmSync(base, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  return { base, root };
}

function ownWorktree(db, { sessionId, callsign = 'otter', spawnId, cwd, worktreePath }) {
  const now = Date.now();
  db.prepare(`INSERT INTO sessions
    (session_id, callsign, cwd, branch, col, note, events, started_at, last_seen, ended_at, archived_at, source)
    VALUES (?, ?, ?, 'wt', 'offline', 'test', 0, ?, ?, ?, ?, 'spawned')`)
    .run(sessionId, callsign, worktreePath, now, now, now, now);
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
    VALUES (?, ?, ?, 'fleetdeck-4711', ?, ?, ?, ?, 'gone')`)
    .run(spawnId, sessionId, callsign, `fd4711-${callsign}`, cwd, worktreePath, now);
}

// ---------------------------------------------------------------------------
// H-R1
// ---------------------------------------------------------------------------

test('H-R1: the rmSync fall-through preserves uncommitted work when force is not set, but still cleans a clean-but-locked worktree', async (t) => {
  const { db, core } = memoryCore(t);
  const { base, root } = initRepo(t, 'repo');

  // A clean worktree that git nonetheless REFUSES to `worktree remove` without
  // --force (it is locked). Reaching the fall-through, the new guard reads a
  // fresh `git status --porcelain`, finds it empty, and removes it — the benign
  // half-removed/locked recovery must still work.
  const cleanWt = path.join(base, 'repo--fd-clean');
  git(['worktree', 'add', '-q', '-b', 'fd/clean', cleanWt], root);
  git(['worktree', 'lock', cleanWt], root);
  ownWorktree(db, { sessionId: 's-clean', callsign: 'clean', spawnId: 'sp-clean', cwd: root, worktreePath: cleanWt });

  const okRes = await core.removeWorktree({ path: cleanWt }); // no force
  assert.equal(okRes.status, 200, `a clean (if locked) worktree is removable without force: ${JSON.stringify(okRes.body)}`);
  assert.equal(existsSync(cleanWt), false, 'the clean worktree is gone from disk');

  // A DIRTY worktree with uncommitted, unignored work. A request that never
  // set force must NOT destroy it — the file survives and a 409 is returned.
  const dirtyWt = path.join(base, 'repo--fd-dirty');
  git(['worktree', 'add', '-q', '-b', 'fd/dirty', dirtyWt], root);
  writeFileSync(path.join(dirtyWt, 'precious.txt'), 'UNCOMMITTED — do not delete\n');
  ownWorktree(db, { sessionId: 's-dirty', callsign: 'dirty', spawnId: 'sp-dirty', cwd: root, worktreePath: dirtyWt });

  const refused = await core.removeWorktree({ path: dirtyWt }); // no force
  assert.equal(refused.status, 409, 'a dirty worktree without force must be refused');
  assert.equal(refused.body.ok, false);
  assert.equal(existsSync(path.join(dirtyWt, 'precious.txt')), true, 'uncommitted work is never destroyed without force');

  // ...and force still removes it.
  const forced = await core.removeWorktree({ path: dirtyWt, force: true });
  assert.equal(forced.status, 200, `force removes the dirty worktree: ${JSON.stringify(forced.body)}`);
  assert.equal(existsSync(dirtyWt), false, 'force:true does remove the dirty worktree from disk');
});

// ---------------------------------------------------------------------------
// H-R2
// ---------------------------------------------------------------------------

test('H-R2: boot reconciliation leaves active rows UNKNOWN when tmux is unreachable, and tombstones them when it is reachable', async (t) => {
  function seedActiveSpawn(db) {
    const now = Date.now();
    db.prepare(`INSERT INTO sessions (session_id, callsign, col, note, events, started_at, last_seen, source)
      VALUES ('s1', 'a1', 'working', 'running', 1, ?, ?, 'spawned')`).run(now, now);
    db.prepare(`INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
      VALUES ('sp1', 's1', 'a1', 'fleetdeck-4711', 'fd4711-a1', ?, 'live')`).run(now);
  }

  // Unreachable: listScopedWindows returns [] (as it does on a tmux timeout)
  // AND the reachability probe (ensureSession) throws. Nothing is tombstoned.
  {
    const tmux = makeAdapter(4711, {
      listScopedWindows: async () => [],
      ensureSession: async () => { throw new Error('tmux timed out'); },
    });
    const { db, core } = memoryCore(t, { tmux });
    seedActiveSpawn(db);
    await core.reconcileSpawns();
    assert.equal(db.prepare("SELECT status FROM spawns WHERE spawn_id='sp1'").get().status, 'live',
      'tmux unreachable → the live row stays live (unknown), never gone');
    assert.equal(db.prepare("SELECT col FROM sessions WHERE session_id='s1'").get().col, 'working',
      'tmux unreachable → the card is not tombstoned offline');
    assert.ok(core.snapshot().ticker.some(x => /tmux unreachable at restart/.test(x.msg)),
      'the skip is announced on the feed');
  }

  // Reachable but the fleet owns no windows: the empty list is authoritative,
  // so the stale row IS reconciled to gone + offline (the existing contract).
  {
    const tmux = makeAdapter(4711, {
      listScopedWindows: async () => [],
      ensureSession: async () => 'fleetdeck-4711', // resolves ⇒ reachable
    });
    const { db, core } = memoryCore(t, { tmux });
    seedActiveSpawn(db);
    await core.reconcileSpawns();
    assert.equal(db.prepare("SELECT status FROM spawns WHERE spawn_id='sp1'").get().status, 'gone',
      'tmux reachable + no windows → the row is reconciled to gone');
    assert.equal(db.prepare("SELECT col FROM sessions WHERE session_id='s1'").get().col, 'offline',
      'tmux reachable + no windows → the card is tombstoned offline');
  }
});

// ---------------------------------------------------------------------------
// H-R5
// ---------------------------------------------------------------------------

test('H-R5: killing by a stale (historical) spawn_id is refused; the newest owner is killable', async (t) => {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-hr5-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-hr5-cwd-'));
  t.after(() => { rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
  const { db, core, state } = memoryCore(t, { env: { HOME: userHome } });

  const first = await core.spawn({ cwd });
  const { spawn_id: oldId, session_id: sid } = first.body;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  // The session dies; its row is terminal and the pane is dead — the exact
  // precondition for a revive that reuses the same tmux_window.
  db.prepare("UPDATE spawns SET status='gone' WHERE spawn_id=?").run(oldId);
  db.prepare("UPDATE sessions SET col='offline', ended_at=?, archived_at=? WHERE session_id=?")
    .run(Date.now(), Date.now(), sid);
  const transcript = claudeTranscriptPath(cwd, sid, userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{}\n');
  state.windows[0].pane_dead = true; // dead remnant on the reused window name

  const revived = await core.revive(oldId);
  assert.equal(revived.status, 200, JSON.stringify(revived.body));
  const newId = revived.body.spawn_id;
  assert.notEqual(newId, oldId);
  state.killed.length = 0; // ignore the revive's own remnant kill

  // Killing via the OLD (historical) id must be refused even under force — the
  // window now belongs to the revived row.
  const stale = await core.spawnKill(oldId, true);
  assert.equal(stale.status, 409, 'a historical spawn_id must not kill the reused window');
  assert.equal(stale.body.current_spawn_id, newId, 'the refusal names the current owner');
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id=?').get(newId).status, 'spawning',
    'the newer, live row is untouched by the stale kill');
  assert.deepEqual(state.killed, [], 'no tmux window was killed by the stale request');

  // Killing via the current id proceeds.
  const good = await core.spawnKill(newId, true);
  assert.equal(good.status, 200, JSON.stringify(good.body));
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id=?').get(newId).status, 'killed');
  assert.deepEqual(state.killed, [revived.body.tmux.window]);
});

// ---------------------------------------------------------------------------
// H-R6
// ---------------------------------------------------------------------------

test('H-R6: a tmux launch failure leaves NO orphan — the worktree, window, and row are all cleaned', async (t) => {
  const { root, base } = initRepo(t, 'repo');
  const tmux = makeAdapter(4711, {
    ensureSession: async () => 'fleetdeck-4711',
    newWindow: async () => { throw new Error('tmux new-window boom'); },
    listScopedWindows: async () => [],
  });
  const { db, core } = memoryCore(t, { tmux });

  const res = await core.spawn({ cwd: root, worktree: true });
  assert.equal(res.status, 500, `a failed launch fails loud: ${JSON.stringify(res.body)}`);

  const rows = db.prepare('SELECT * FROM spawns').all();
  assert.equal(rows.length, 1, 'the provisional row exists (durable-before-external-ops)');
  assert.equal(rows[0].status, 'gone', 'the provisional row was settled terminal, not left provisioning/spawning');

  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM spawns WHERE status IN ('provisioning','spawning','stalled','live')").get().n, 0,
    'no active/provisioning row survives a launch failure');

  const wt = path.join(base, `repo--fd-${rows[0].callsign}`);
  assert.equal(existsSync(wt), false, 'the partial worktree was removed — no orphan on disk');
  assert.equal(tmux.state.killed.includes(`fd4711-${rows[0].callsign}`), true, 'the (partial) window was killed by name');

  const card = core.snapshot().sessions.find(s => s.session_id === rows[0].session_id);
  assert.equal(card.col, 'offline', 'the card is tombstoned');
  assert.match(card.note, /spawn failed/);

  // git itself must no longer know about the worktree (prune ran).
  const list = git(['worktree', 'list', '--porcelain'], root);
  assert.equal(list.includes(wt), false, 'git worktree list no longer references the cleaned worktree');
});

// ---------------------------------------------------------------------------
// H-R7
// ---------------------------------------------------------------------------

test('H-R7: revive checks cwd/transcript BEFORE touching tmux, and refuses to kill a live non-claude pane', async (t) => {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-hr7-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-hr7-cwd-'));
  t.after(() => { rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  // A live pane running vim occupies the deterministic window name.
  const tmux = makeAdapter(4711, {
    listScopedWindows: async () => [{ session: 'fleetdeck-4711', window: 'fd4711-otter', window_id: '@9', pane_dead: false, pane_cmd: 'vim' }],
  });
  const { db, core, state } = memoryCore(t, { tmux, env: { HOME: userHome } });
  const now = Date.now();
  db.prepare(`INSERT INTO sessions (session_id, callsign, cwd, col, note, events, started_at, last_seen, ended_at, source)
    VALUES ('s7', 'otter', ?, 'offline', 'ended', 0, ?, ?, ?, 'spawned')`).run(cwd, now, now, now);
  db.prepare(`INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, requested_at, status)
    VALUES ('sp7', 's7', 'otter', 'fleetdeck-4711', 'fd4711-otter', ?, ?, 'gone')`).run(cwd, now);

  // Case 1: transcript missing → 410 WITHOUT having killed the vim pane.
  const missing = await core.revive('sp7');
  assert.equal(missing.status, 410, 'a missing transcript still refuses');
  assert.match(missing.body.reason, /transcript/);
  assert.deepEqual(state.killed, [], 'eligibility is checked BEFORE tmux — the unrelated pane is not killed');

  // Case 2: transcript present, but the window hosts a LIVE non-claude pane →
  // refuse (409) rather than destroying it.
  const transcript = claudeTranscriptPath(cwd, 's7', userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{}\n');
  const refuse = await core.revive('sp7');
  assert.equal(refuse.status, 409, 'a live non-claude pane is a refusal, not a kill');
  assert.match(refuse.body.reason, /live 'vim' pane/);
  assert.deepEqual(state.killed, [], 'the live non-claude pane is never killed');
});

// ---------------------------------------------------------------------------
// M-B2
// ---------------------------------------------------------------------------

test('M-B2: a PreToolUse conflict whisper declares hookEventName:"PreToolUse"', (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mb2-cwd-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const { core } = memoryCore(t);
  const file = path.join(cwd, 'util.js');

  core.hookSessionStart({ session_id: 'A', cwd, source: 'startup' });
  core.hookSessionStart({ session_id: 'B', cwd, source: 'startup' });
  // A edits first (no rival yet).
  core.hookPostToolUse({ session_id: 'A', hook_event_name: 'PostToolUse', cwd, tool_name: 'Edit', tool_input: { file_path: file } });
  // B edits the same file via a PreToolUse hook → whisper.
  const out = core.hookPostToolUse({ session_id: 'B', hook_event_name: 'PreToolUse', cwd, tool_name: 'Edit', tool_input: { file_path: file } });
  assert.ok(out.hookSpecificOutput, 'the second editor gets a whisper');
  assert.equal(out.hookSpecificOutput.hookEventName, 'PreToolUse',
    'the whisper must carry the caller\'s real event name so a PreToolUse client keeps it');

  // A PostToolUse conflict still declares PostToolUse (no regression).
  core.hookSessionStart({ session_id: 'C', cwd, source: 'startup' });
  const post = core.hookPostToolUse({ session_id: 'C', hook_event_name: 'PostToolUse', cwd, tool_name: 'Edit', tool_input: { file_path: file } });
  assert.equal(post.hookSpecificOutput.hookEventName, 'PostToolUse');
});

// ---------------------------------------------------------------------------
// M-B4
// ---------------------------------------------------------------------------

test('M-B4: a corrupt conflicts row does not 500 /state — it is dropped, good rows survive', (t) => {
  const db = openDb(':memory:');
  t.after(() => db.close());
  const now = Date.now();
  db.prepare('INSERT INTO conflicts (at, repo_id, rel_path, severity, sessions_json) VALUES (?, ?, ?, ?, ?)')
    .run(now, 'r', 'bad.js', 'warning', '{ this is not json');
  db.prepare('INSERT INTO conflicts (at, repo_id, rel_path, severity, sessions_json) VALUES (?, ?, ?, ?, ?)')
    .run(now, 'r', 'good.js', 'warning', JSON.stringify(['sX', 'sY']));

  const core = createCore(db, { port: 4711, home: '/h', tmuxAdapter: makeAdapter().adapter });
  let snap;
  assert.doesNotThrow(() => { snap = core.snapshot(); }, 'a corrupt row must not throw out of snapshot()');
  assert.equal(snap.conflicts.length, 1, 'the corrupt conflict is dropped, the good one survives');
  assert.equal(snap.conflicts[0].rel_path, 'good.js');
  assert.deepEqual(snap.conflicts[0].sessions, ['sX', 'sY']);
});

// ---------------------------------------------------------------------------
// M-B5
// ---------------------------------------------------------------------------

for (const event of ['PreToolUse', 'PostToolUse']) {
  test(`M-B5: a resurrecting ${event} lifts an offline card out of the offline column`, (t) => {
    const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mb5-cwd-'));
    t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
    const { db, core } = memoryCore(t);

    core.applyEvent({ session_id: 's', hook_event_name: 'SessionStart', cwd, source: 'startup' });
    core.applyEvent({ session_id: 's', hook_event_name: 'SessionEnd', cwd });
    let card = db.prepare("SELECT * FROM sessions WHERE session_id='s'").get();
    assert.equal(card.col, 'offline', 'precondition: the card is tombstoned offline');
    assert.ok(card.ended_at);

    // A late tool hook proves the process is alive again.
    core.applyEvent({ session_id: 's', hook_event_name: event, cwd, tool_name: 'Read' });
    card = db.prepare("SELECT * FROM sessions WHERE session_id='s'").get();
    assert.equal(card.ended_at, null, 'resurrection clears ended_at');
    assert.equal(card.col, 'working', `a ${event} resurrection re-derives a live lane, not offline`);
  });
}

// ---------------------------------------------------------------------------
// M-B6
// ---------------------------------------------------------------------------

test('M-B6: an ExitPlanMode plan-persist failure rolls the question row back and fails the hook open', (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mb6-cwd-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const { db, core } = memoryCore(t);

  // Force the plan insert to throw at runtime by removing its table AFTER the
  // prepared statements were compiled.
  db.exec('DROP TABLE plans');
  const before = db.prepare('SELECT COUNT(*) AS n FROM questions').get().n;

  const ev = { session_id: 's', cwd, tool_name: 'ExitPlanMode', tool_input: { plan: '# Plan\n\nstep 1\n' } };
  const row = core.hookHoldQuestion(ev, 'PermissionRequest');

  assert.equal(row, null, 'the hook fails OPEN (no relay) when the plan cannot be persisted');
  const after = db.prepare('SELECT COUNT(*) AS n FROM questions').get().n;
  assert.equal(after, before, 'the question row is rolled back — no held question without its linked plan');
});

test('M-B6: on the happy path both the question row and its plan row persist and are linked', (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mb6ok-cwd-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const { db, core } = memoryCore(t);

  const planMd = '# Add caching\n\n1. do it\n';
  const ev = { session_id: 's', cwd, tool_name: 'ExitPlanMode', tool_input: { plan: planMd } };
  const row = core.hookHoldQuestion(ev, 'PermissionRequest');

  assert.ok(row && row.id, 'the question row is created');
  const plan = db.prepare('SELECT * FROM plans WHERE question_id = ?').get(row.id);
  assert.ok(plan, 'a plan row exists, linked by question_id');
  assert.equal(plan.plan_md, planMd, 'plan markdown is captured byte-identical');
  assert.equal(plan.status, 'proposed');
});

// ---------------------------------------------------------------------------
// M-B8
// ---------------------------------------------------------------------------

test('M-B8: a remote harvest whose capture throws resolves cleanly (no unhandled rejection) via enableRemote', async (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mb8-cwd-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const tmux = makeAdapter(4711, {
    capturePane: async () => { throw new Error('capture-pane exploded'); },
  });
  const { db, core } = memoryCore(t, { tmux, env: { FLEETDECK_RC_HARVEST_MS: 0 } });

  const spawned = await core.spawn({ cwd, remote_control: true });
  const { spawn_id, session_id } = spawned.body;
  core.hookSessionStart({ session_id, cwd, source: 'startup' }); // → live
  db.prepare("UPDATE sessions SET col='idle' WHERE session_id=?").run(session_id);

  // enableRemote awaits the (guarded) harvest; a throwing capturePane must not
  // reject the request.
  const res = await core.enableRemote(spawn_id);
  assert.equal(res.status, 200, `a throwing capture must not fail enableRemote: ${JSON.stringify(res.body)}`);
  assert.equal(res.body.ok, true);
  assert.equal(res.body.url, null, 'no URL was harvested, but the flow completed cleanly');
});

// ---------------------------------------------------------------------------
// M-G1
// ---------------------------------------------------------------------------

test('M-G1: retentionSweep ages file_touches/commands/conflicts/settled-mail past the ledger window; pending mail is spared', (t) => {
  setEnv(t, { FLEETDECK_NUDGE_MS: 1_000_000, FLEETDECK_PANE_MAIL_GRACE_MS: 1_000_000 });
  const db = openDb(':memory:');
  t.after(() => db.close());
  const now = Date.now();
  const old = now - 48 * HOUR; // older than the default 24h ledger window
  const recent = now - 60_000;

  db.prepare('INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?,?,?,?,?,?)')
    .run('r', 'old.js', '/x/old.js', 's', null, old);
  db.prepare('INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?,?,?,?,?,?)')
    .run('r', 'new.js', '/x/new.js', 's', null, recent);
  db.prepare('INSERT INTO commands (at, text, parsed_json) VALUES (?, ?, ?)').run(old, 'old cmd', '{}');
  db.prepare('INSERT INTO commands (at, text, parsed_json) VALUES (?, ?, ?)').run(recent, 'new cmd', '{}');
  db.prepare('INSERT INTO conflicts (at, repo_id, rel_path, severity, sessions_json) VALUES (?,?,?,?,?)')
    .run(old, 'r', 'old.js', 'warning', '[]');
  db.prepare('INSERT INTO conflicts (at, repo_id, rel_path, severity, sessions_json) VALUES (?,?,?,?,?)')
    .run(recent, 'r', 'new.js', 'warning', '[]');
  db.prepare('INSERT INTO mail (to_session, from_id, text, at, delivered_at) VALUES (?,?,?,?,?)')
    .run('nobody', 'ops', 'delivered old', old, old); // settled (delivered) → prunable
  db.prepare('INSERT INTO mail (to_session, from_id, text, at, delivered_at) VALUES (?,?,?,?,NULL)')
    .run('nobody', 'ops', 'pending old', old); // pending → must survive

  // Boot runs retentionSweep once, which ages the old rows.
  createCore(db, { port: 4711, home: '/h', tmuxAdapter: makeAdapter().adapter });

  assert.equal(db.prepare('SELECT COUNT(*) AS n FROM file_touches').get().n, 1, 'only the recent touch survives');
  assert.equal(db.prepare("SELECT rel_path FROM file_touches").get().rel_path, 'new.js');
  assert.equal(db.prepare('SELECT COUNT(*) AS n FROM commands').get().n, 1, 'only the recent command survives');
  assert.equal(db.prepare('SELECT COUNT(*) AS n FROM conflicts').get().n, 1, 'only the recent conflict survives');
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM mail WHERE text='delivered old'").get().n, 0, 'settled old mail is pruned');
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM mail WHERE text='pending old'").get().n, 1, 'pending mail is NEVER age-pruned');
});

test('M-G1: snapshot windows the per-card file list to the ledger window', (t) => {
  const { db, core } = memoryCore(t);
  const now = Date.now();
  db.prepare(`INSERT INTO sessions (session_id, callsign, col, note, events, started_at, last_seen, source)
    VALUES ('sf', 'af', 'working', 'x', 1, ?, ?, 'hooks')`).run(now, now);
  // one recent touch, one ancient touch (older than 24h)
  db.prepare('INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?,?,?,?,?,?)')
    .run('r', 'recent.js', '/x/recent.js', 'sf', null, now - 60_000);
  db.prepare('INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?,?,?,?,?,?)')
    .run('r', 'ancient.js', '/x/ancient.js', 'sf', null, now - 48 * HOUR);

  const card = core.snapshot().sessions.find(s => s.session_id === 'sf');
  assert.deepEqual(card.files, ['/x/recent.js'], 'the snapshot only shows touches inside the ledger window');
});

// ---------------------------------------------------------------------------
// M-P8 (correctness of the cached-statement keying)
// ---------------------------------------------------------------------------

test('M-P8: cached updateSession statements apply the right columns across distinct shapes', (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mp8-cwd-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const { db, core } = memoryCore(t);

  // Drive a session through several event kinds — each builds a different
  // updateSession column shape. A mis-keyed cache would bind the wrong value
  // to the wrong column and corrupt the row.
  core.applyEvent({ session_id: 'p', hook_event_name: 'SessionStart', cwd, source: 'startup', model: 'sonnet' });
  core.applyEvent({ session_id: 'p', hook_event_name: 'UserPromptSubmit', cwd, prompt: 'do the thing' });
  core.applyEvent({ session_id: 'p', hook_event_name: 'PostToolUse', cwd, tool_name: 'Bash', tool_input: { command: 'ls' } });
  core.applyEvent({ session_id: 'p', hook_event_name: 'Stop', cwd });

  const card = db.prepare("SELECT * FROM sessions WHERE session_id='p'").get();
  assert.equal(card.col, 'idle', 'Stop → idle');
  assert.equal(card.model, 'sonnet', 'the launch model survived every later shape');
  assert.equal(card.last_tool, 'Bash');
  assert.equal(card.task, 'do the thing');
  assert.equal(card.source, 'hooks');
  assert.ok(card.events >= 4);
});

// ---------------------------------------------------------------------------
// R2-5
// ---------------------------------------------------------------------------

test('R2-5: a stale-id force-kill arriving during a revive\'s window creation is refused; the revived pane survives', async (t) => {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-r25-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-r25-cwd-'));
  t.after(() => { rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  // A newWindow we can PAUSE mid-flight. revive() inserts its provisional row,
  // then awaits newWindow — exactly the gap in which a stale kill used to slip
  // in and destroy the just-created pane. Gating newWindow lets us fire the
  // kill while the revive is parked there.
  const tmux = makeAdapter(4711);
  const { state } = tmux;
  let releaseNewWindow;
  const gate = new Promise(resolve => { releaseNewWindow = resolve; });
  let sawNewWindow;
  const started = new Promise(resolve => { sawNewWindow = resolve; });
  tmux.adapter.newWindow = async spec => {
    sawNewWindow();
    await gate;
    const win = { session: `fleetdeck-${spec.port}`, window: `fd${spec.port}-${spec.callsign}`,
      window_id: '@1', pane_dead: false, pane_cmd: 'claude' };
    state.windows.push(win);
    return { session: win.session, window: win.window, window_id: win.window_id };
  };

  const { db, core } = memoryCore(t, { tmux, env: { HOME: userHome } });

  // Seed a terminal (pane-dead) board spawn whose window name a revive reuses,
  // plus the resume transcript so revive is eligible. NO live remnant window is
  // registered, so revive skips its own remnant kill — state.killed then
  // reflects ONLY what the stale kill attempts.
  const sid = randomUUID();
  const oldId = randomUUID();
  const now = Date.now();
  db.prepare(`INSERT INTO sessions (session_id, callsign, cwd, col, note, events, started_at, last_seen, ended_at, archived_at, source)
    VALUES (?, 'otter', ?, 'offline', 'ended', 0, ?, ?, ?, ?, 'spawned')`).run(sid, cwd, now, now, now, now);
  db.prepare(`INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, requested_at, status)
    VALUES (?, ?, 'otter', 'fleetdeck-4711', 'fd4711-otter', ?, ?, 'pane-dead')`).run(oldId, sid, cwd, now - 10_000);
  const transcript = claudeTranscriptPath(cwd, sid, userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{}\n');

  // Start the revive but do NOT await it — it parks inside newWindow, AFTER the
  // provisional row exists and owns the reused window.
  const revivePromise = core.revive(oldId);
  await started;

  // A forced kill via the OLD (historical) id lands mid-revive. It must be
  // refused: the reused window now belongs to the new provisional row.
  const stale = await core.spawnKill(oldId, true);
  assert.equal(stale.status, 409, 'a stale-id kill during the revive window creation must be refused');
  assert.notEqual(stale.body.current_spawn_id, oldId, 'the refusal must not name the stale id as owner');
  assert.deepEqual(state.killed, [], 'the stale kill killed no tmux window — the revived pane is untouched');
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id=?').get(oldId).status, 'pane-dead',
    'the historical row is not flipped to killed by the refused request');

  // Let the revive finish; the pane comes up and the row goes live-eligible.
  releaseNewWindow();
  const revived = await revivePromise;
  assert.equal(revived.status, 200, JSON.stringify(revived.body));
  const newId = revived.body.spawn_id;
  assert.notEqual(newId, oldId);
  assert.equal(stale.body.current_spawn_id, newId, 'the refusal named the new revive row as the current owner');
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id=?').get(newId).status, 'spawning',
    'the revived row is spawning (live-eligible), not left provisioning or dead');

  // The refused kill never reached its tombstone updateSession; revive's own
  // update is what stands (queued/reviving), not "pane killed from the board".
  const card = db.prepare('SELECT * FROM sessions WHERE session_id=?').get(sid);
  assert.equal(card.col, 'queued', 'the stale kill did not tombstone the reviving card offline');
  assert.match(card.note, /reviving/, 'revive\'s state stands; the refused kill left no mark');

  // Sanity: the CURRENT id can still kill it.
  const good = await core.spawnKill(newId, true);
  assert.equal(good.status, 200, JSON.stringify(good.body));
  assert.deepEqual(state.killed, ['fd4711-otter'], 'only the current-id kill reaches tmux');
});

// ---------------------------------------------------------------------------
// R2-6
// ---------------------------------------------------------------------------

test('R2-6: a conflicts row that is valid JSON of the WRONG shape is dropped by snapshot() and cleanup(), never thrown on', async (t) => {
  const db = openDb(':memory:');
  t.after(() => db.close());
  const now = Date.now();
  const ins = db.prepare('INSERT INTO conflicts (at, repo_id, rel_path, severity, sessions_json) VALUES (?, ?, ?, ?, ?)');
  ins.run(now, 'r', 'obj.js', 'warning', '{}');    // an object, not an array
  ins.run(now, 'r', 'null.js', 'warning', 'null'); // null → null.map / null.length used to throw
  ins.run(now, 'r', 'str.js', 'warning', '"sX"');  // a bare string
  ins.run(now, 'r', 'num.js', 'warning', '42');    // a number
  ins.run(now, 'r', 'good.js', 'warning', JSON.stringify(['sX', 'sY'])); // the one well-formed row

  const core = createCore(db, { port: 4711, home: '/h', tmuxAdapter: makeAdapter().adapter });

  let snap;
  assert.doesNotThrow(() => { snap = core.snapshot(); }, 'wrong-shape rows must not throw out of snapshot()');
  assert.equal(snap.conflicts.length, 1, 'only the well-formed conflict survives the snapshot');
  assert.equal(snap.conflicts[0].rel_path, 'good.js');
  assert.deepEqual(snap.conflicts[0].sessions, ['sX', 'sY']);

  // cleanup() walks the same rows; its guard used to run `null.length` and throw.
  let res;
  await assert.doesNotReject(async () => { res = await core.cleanup(); }, 'wrong-shape rows must not throw out of cleanup()');
  assert.ok(res.ok);
  assert.equal(db.prepare('SELECT COUNT(*) AS n FROM conflicts').get().n, 0,
    'every conflict (wrong-shape + the dead-session good one) is cleared, none survives as a crash');
});

// ---------------------------------------------------------------------------
// R2-7
// ---------------------------------------------------------------------------

test('R2-7: the per-card file cap keeps a card\'s NEWEST touches, not its oldest', (t) => {
  const { db, core } = memoryCore(t);
  const now = Date.now();
  db.prepare(`INSERT INTO sessions (session_id, callsign, col, note, events, started_at, last_seen, source)
    VALUES ('sn', 'an', 'working', 'x', 1, ?, ?, 'hooks')`).run(now, now);

  // 60 distinct files, f0 (oldest touch) … f59 (newest), all inside the ledger
  // window. The cap is 50, so the 10 OLDEST (f0…f9) must be the ones dropped.
  const ins = db.prepare('INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?,?,?,?,?,?)');
  for (let i = 0; i < 60; i++) {
    ins.run('r', `f${i}.js`, `/x/f${i}.js`, 'sn', null, now - (60 - i) * 1000);
  }

  const card = core.snapshot().sessions.find(s => s.session_id === 'sn');
  assert.equal(card.files.length, 50, 'the per-card list is capped at 50');
  assert.equal(card.files[0], '/x/f59.js', 'the newest touch is listed first');
  assert.ok(card.files.includes('/x/f59.js') && card.files.includes('/x/f10.js'), 'the newest 50 survive');
  assert.ok(!card.files.includes('/x/f0.js') && !card.files.includes('/x/f9.js'),
    'the 10 OLDEST are dropped — the pre-fix code kept these and dropped the newest');
});

// ---------------------------------------------------------------------------
// R2-8 — the FINAL git-status guard in removeWorktree (gate 2), reached only
// after `git worktree remove` fails. The existing H-R1 dirty case returns at
// gate 1 (inspect verdict 'has-work') and never reaches gate 2, so deleting the
// guard would still pass it. A tiny `git` shim on PATH lets an uncommitted
// write land BETWEEN the inspector's clean read and the actual removal — the
// exact TOCTOU the guard exists to catch — deterministically, with real git
// answering every other call.
// ---------------------------------------------------------------------------

function realGitPath() {
  // Resolved from the UNSHIMMED PATH (call before installing the shim).
  return execFileSync('sh', ['-c', 'command -v git'], { encoding: 'utf8' }).trim();
}

// A `git` that passes everything through to real git, except it runs one side
// effect right before `git worktree remove`:
//   FD_SHIM_MODE=dirty → drop an uncommitted file into the worktree, then let
//     real git run (it now refuses the dirty removal → gate 2 re-reads dirty).
//   FD_SHIM_MODE=break → remove the worktree's .git pointer and report failure,
//     so gate 2's own `git status` errors (the benign half-removed recovery).
function writeGitShim(t) {
  const dir = mkdtempSync(path.join(tmpdir(), 'fd-gitshim-'));
  const shim = path.join(dir, 'git');
  writeFileSync(shim,
    '#!/usr/bin/env bash\n'
    + 'case " $* " in\n'
    + '  *" worktree remove "*)\n'
    + '    if [ "$FD_SHIM_MODE" = dirty ]; then\n'
    + '      printf \'UNCOMMITTED via TOCTOU\\n\' > "$FD_SHIM_TARGET/precious.txt"\n'
    + '    elif [ "$FD_SHIM_MODE" = break ]; then\n'
    + '      rm -f "$FD_SHIM_TARGET/.git"\n'
    + '      exit 1\n'
    + '    fi\n'
    + '    ;;\n'
    + 'esac\n'
    + 'exec "$FD_REAL_GIT" "$@"\n',
    { mode: 0o755 });
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  return dir;
}

test('R2-8: a write landing between inspect and removal is refused by the final git-status guard — uncommitted work survives', async (t) => {
  const { db, core } = memoryCore(t);
  const { base, root } = initRepo(t, 'repo');

  // A CLEAN worktree: the inspector reads verdict 'safe' and passes gate 1.
  const wt = path.join(base, 'repo--fd-toctou');
  git(['worktree', 'add', '-q', '-b', 'fd/toctou', wt], root);
  ownWorktree(db, { sessionId: 's-toctou', callsign: 'toctou', spawnId: 'sp-toctou', cwd: root, worktreePath: wt });

  const shimDir = writeGitShim(t);
  setEnv(t, { PATH: `${shimDir}:${process.env.PATH}`, FD_REAL_GIT: realGitPath(), FD_SHIM_MODE: 'dirty', FD_SHIM_TARGET: wt });

  const res = await core.removeWorktree({ path: wt }); // no force
  assert.equal(res.status, 409, `the final-status guard refuses the now-dirty tree: ${JSON.stringify(res.body)}`);
  assert.equal(res.body.verdict, 'has-work');
  assert.equal(existsSync(wt), true, 'the worktree still exists — it was NOT rm -rf-ed');
  assert.equal(existsSync(path.join(wt, 'precious.txt')), true, 'the uncommitted file the guard protected survives');
});

test('R2-8: when git can no longer read the tree at the final status check, removal falls through to rmSync (half-removed recovery)', async (t) => {
  const { db, core } = memoryCore(t);
  const { base, root } = initRepo(t, 'repo');

  const wt = path.join(base, 'repo--fd-broken');
  git(['worktree', 'add', '-q', '-b', 'fd/broken', wt], root);
  ownWorktree(db, { sessionId: 's-broken', callsign: 'broken', spawnId: 'sp-broken', cwd: root, worktreePath: wt });

  const shimDir = writeGitShim(t);
  setEnv(t, { PATH: `${shimDir}:${process.env.PATH}`, FD_REAL_GIT: realGitPath(), FD_SHIM_MODE: 'break', FD_SHIM_TARGET: wt });

  const res = await core.removeWorktree({ path: wt }); // no force
  assert.equal(res.status, 200, `an unreadable tree falls through to rmSync: ${JSON.stringify(res.body)}`);
  assert.equal(res.body.removed, true);
  assert.equal(existsSync(wt), false, 'the daemon removed the directory itself when git could not');
});
