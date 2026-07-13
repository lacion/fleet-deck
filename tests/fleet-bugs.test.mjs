// tests/fleet-bugs.test.mjs
//
// Four interrelated production reliability bugs that, on a real machine, made
// "the board lose every live terminal today". All exercised at the core level
// with the fake tmux adapter so pane liveness is scriptable:
//
//   BUG 1  hookSessionEnd condemned a LIVE pane on /clear (reason='clear').
//   BUG 2  retention presumed a LIVE idle spawned agent dead after 3h silence,
//          archived it, and goned its spawn — without ever asking tmux.
//   BUG 3  'pane-dead'/'gone' were a ONE-WAY DOOR (never re-checked), so BUG 1/2
//          were unrecoverable and revive() deadlocked on the very-alive pane.
//   BUG 4  mail silently truncated at 500 chars and told the sender nothing.

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
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

// A scriptable tmux: push windows, then flip pane_dead / pane_cmd to simulate a
// pane dying, a bare shell remnant, or a live claude at its prompt.
function fakeTmux(port = 4711) {
  const state = { windows: [], argv: null, killed: [] };
  const adapter = {
    spawnOverrideCmd: () => null,
    hasTmux: () => true,
    sessionName: p => `fleetdeck-${p}`,
    windowName: (p, callsign) => `fd${p}-${callsign}`,
    ensureSession: async p => `fleetdeck-${p}`,
    newWindow: async spec => {
      state.argv = spec.argv;
      const win = {
        session: `fleetdeck-${spec.port}`, window: `fd${spec.port}-${spec.callsign}`,
        window_id: `@${state.windows.length + 1}`, pane_dead: false, pane_cmd: 'claude',
      };
      state.windows.push(win);
      return { session: win.session, window: win.window, window_id: win.window_id };
    },
    listScopedWindows: async () => state.windows,
    paneCurrentCommand: async target => {
      const win = state.windows.find(w => w.window_id === target || w.window === target);
      return win ? { dead: win.pane_dead, cmd: win.pane_cmd } : null;
    },
    pasteText: async () => true,
    sendEnter: async () => true,
    sendBringupEnter: async () => true,
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

function cardOf(core, sid) {
  return core.snapshot().sessions.find(s => s.session_id === sid);
}

// ---------------------------------------------------------------------------
// BUG 1 — /clear must NOT condemn a live pane or tombstone the card
// ---------------------------------------------------------------------------

test('BUG 1: SessionEnd(reason="clear") keeps the pane and the card LIVE', async (t) => {
  const { core, db } = memoryCore(t);
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-clear-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' }); // pane live claude, spawn → live
  core.applyEvent({ session_id: sid, hook_event_name: 'UserPromptSubmit', cwd, prompt: 'do work' });
  assert.equal(cardOf(core, sid).col, 'working', 'sanity: mid-turn before /clear');
  assert.equal(cardOf(core, sid).spawn.status, 'live');

  // The human runs /clear — Claude Code fires SessionEnd(reason='clear') but
  // the session stays live (same session_id, pane keeps running claude).
  core.hookSessionEnd({ session_id: sid, cwd, reason: 'clear' });

  const card = cardOf(core, sid);
  assert.notEqual(card.col, 'offline', 'a /clear must NOT tombstone the card');
  assert.equal(card.endedAt, null, 'a /clear must NOT stamp ended_at');
  assert.equal(card.spawn.status, 'live', 'a /clear must NOT condemn the pane to pane-dead — the terminal stays');
  assert.equal(db.prepare('SELECT status FROM spawns WHERE session_id = ?').get(sid).status, 'live');
});

test('BUG 1 control: a real SessionEnd (reason="other") still ends the session and pane', async (t) => {
  const { core } = memoryCore(t);
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-end-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });
  assert.equal(cardOf(core, sid).spawn.status, 'live');

  core.hookSessionEnd({ session_id: sid, cwd, reason: 'other' });

  const card = cardOf(core, sid);
  assert.equal(card.col, 'offline', 'a real end tombstones the card');
  assert.ok(card.endedAt, 'a real end stamps ended_at');
  assert.equal(card.spawn.status, 'pane-dead', 'a real end marks the pane dead (window kept for scrollback)');
});

// ---------------------------------------------------------------------------
// BUG 2 — retention must ask tmux before condemning a SPAWNED session
// ---------------------------------------------------------------------------

test('BUG 2: a silent-but-pane-alive spawn is NOT presumed dead/goned; a pane-dead one IS', async (t) => {
  const tmux = fakeTmux();
  const { db, core, state } = memoryCore(t, { tmux });
  const now = Date.now();
  const silent = now - 4 * HOUR; // > PRESUME_DEAD_MS (default 3h)

  const seedSession = (sid, callsign) => db.prepare(`INSERT INTO sessions
    (session_id, callsign, col, note, events, started_at, last_seen, source)
    VALUES (?, ?, 'idle', 'waiting at its prompt', 0, ?, ?, 'hooks')`).run(sid, callsign, silent, silent);
  const seedSpawn = (spawnId, sid, callsign, window) => db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
    VALUES (?, ?, ?, 'fleetdeck-4711', ?, ?, 'live')`).run(spawnId, sid, callsign, window, silent);

  // ALIVE: idle 4h, but its pane is a live claude in tmux (the "3.1h alive
  // spawn got goned" report).
  seedSession('alive', 'alive-1');
  seedSpawn('sp-alive', 'alive', 'alive-1', 'fd4711-alive-1');
  state.windows.push({ session: 'fleetdeck-4711', window: 'fd4711-alive-1', window_id: '@a', pane_dead: false, pane_cmd: 'claude' });

  // DEAD: idle 4h, and tmux says its pane is dead.
  seedSession('dead', 'dead-1');
  seedSpawn('sp-dead', 'dead', 'dead-1', 'fd4711-dead-1');
  state.windows.push({ session: 'fleetdeck-4711', window: 'fd4711-dead-1', window_id: '@d', pane_dead: true, pane_cmd: 'claude' });

  await core.retentionSweep(now);

  // Alive agent: left live, last_seen refreshed, spawn never goned.
  const alive = db.prepare("SELECT * FROM sessions WHERE session_id = 'alive'").get();
  assert.equal(alive.ended_at, null, 'a tmux-alive spawn must NOT be presumed dead');
  assert.notEqual(alive.col, 'offline');
  assert.equal(alive.last_seen, now, 'its silence clock is refreshed from tmux liveness');
  assert.equal(db.prepare("SELECT status FROM spawns WHERE spawn_id = 'sp-alive'").get().status, 'live',
    'goneArchivedSpawns must never reach a spawn that was never wrongly archived');

  // Dead pane: condemned exactly like the liveness tick would.
  const dead = db.prepare("SELECT * FROM sessions WHERE session_id = 'dead'").get();
  assert.equal(dead.col, 'offline', 'a tmux-confirmed dead pane IS condemned');
  assert.ok(dead.ended_at);
  assert.equal(db.prepare("SELECT status FROM spawns WHERE spawn_id = 'sp-dead'").get().status, 'pane-dead');
});

test('BUG 2: a pane-less hook-only session keeps the silence-based presume-dead behavior', async (t) => {
  const { db, core } = memoryCore(t);
  const now = Date.now();
  db.prepare(`INSERT INTO sessions
    (session_id, callsign, col, note, events, started_at, last_seen, source)
    VALUES ('hookonly', 'hook-1', 'idle', 'waiting', 0, ?, ?, 'hooks')`).run(now - 4 * HOUR, now - 4 * HOUR);

  await core.retentionSweep(now);

  const s = db.prepare("SELECT * FROM sessions WHERE session_id = 'hookonly'").get();
  assert.equal(s.col, 'offline', 'no pane to check → silence is the signal');
  assert.ok(s.ended_at);
  assert.match(s.note, /^presumed ended \(silent .+h\)$/);
});

// ---------------------------------------------------------------------------
// BUG 3 — dead spawn states are re-checked and resurrected when tmux disagrees
// ---------------------------------------------------------------------------

async function condemnedSpawnWithLivePane(t, status) {
  const tmux = fakeTmux();
  const ctx = memoryCore(t, { tmux });
  const { core, db } = ctx;
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-res-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  const spawnId = spawn.body.spawn_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' }); // pane live claude
  // Simulate the wrongful condemnation: mark the row terminal and tombstone the
  // card, WHILE the tmux pane keeps running claude (state.windows[0] untouched).
  db.prepare('UPDATE spawns SET status = ? WHERE spawn_id = ?').run(status, spawnId);
  db.prepare("UPDATE sessions SET col = 'offline', note = 'pane died', ended_at = ?, archived_at = ? WHERE session_id = ?")
    .run(Date.now(), Date.now(), sid);
  return { ...ctx, sid, spawnId, cwd };
}

for (const status of ['pane-dead', 'gone']) {
  test(`BUG 3: the liveness tick RESURRECTS a '${status}' spawn whose pane is a live claude`, async (t) => {
    const { core, db, sid, spawnId } = await condemnedSpawnWithLivePane(t, status);

    await core.spawnLivenessTick();

    assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(spawnId).status, 'live',
      `a '${status}' row must be brought back to live once tmux proves the pane is a live claude`);
    const card = cardOf(core, sid);
    assert.equal(card.endedAt, null, 'resurrection clears ended_at');
    assert.notEqual(card.col, 'offline', 'resurrection lifts the card off the offline shelf');
    assert.equal(card.spawn.status, 'live', 'the terminal is shown again (spawn live in the snapshot)');
    assert.equal(db.prepare('SELECT archived_at FROM sessions WHERE session_id = ?').get(sid).archived_at, null);
  });
}

test('BUG 3: a genuinely dead pane-dead spawn is NOT resurrected', async (t) => {
  const { core, db, state, spawnId } = await condemnedSpawnWithLivePane(t, 'pane-dead');
  state.windows[0].pane_dead = true; // the pane really is dead this time

  await core.spawnLivenessTick();

  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(spawnId).status, 'pane-dead',
    'a dead pane stays condemned — resurrection requires a LIVE claude');
});

test('BUG 3: a bare-shell remnant is NOT resurrected', async (t) => {
  const { core, db, state, spawnId } = await condemnedSpawnWithLivePane(t, 'gone');
  state.windows[0].pane_cmd = 'zsh'; // claude exited, remain-on-exit login shell remains

  await core.spawnLivenessTick();

  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(spawnId).status, 'gone',
    'a bare shell is not a live claude — stays gone');
});

test('BUG 3: a human-KILLED spawn stays killed even if a live claude pane exists', async (t) => {
  const { core, db, spawnId } = await condemnedSpawnWithLivePane(t, 'killed');

  await core.spawnLivenessTick();

  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(spawnId).status, 'killed',
    'a human kill is a decision, not a mistake — never resurrected');
});

test('BUG 3: revive() ADOPTS a gone spawn whose window is a live claude (no 409, no duplicate)', async (t) => {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-adopt-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-adopt-cwd-'));
  const tmux = fakeTmux();
  const { core, db, state } = memoryCore(t, { tmux, env: { HOME: userHome } });
  t.after(() => {
    rmSync(userHome, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  });

  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  const spawnId = spawn.body.spawn_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });
  // Wrongly condemned to 'gone' + tombstoned, but the pane kept running claude.
  db.prepare("UPDATE spawns SET status = 'gone' WHERE spawn_id = ?").run(spawnId);
  db.prepare("UPDATE sessions SET col = 'offline', note = 'pane died', ended_at = ?, archived_at = ? WHERE session_id = ?")
    .run(Date.now(), Date.now(), sid);
  // Resume eligibility: cwd exists (temp dir) and a transcript exists.
  const transcript = claudeTranscriptPath(cwd, sid, userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{}\n');

  const out = await core.revive(spawnId);

  assert.equal(out.status, 200, 'a human clicking Revive on the very-alive pane must not be stuck on a 409');
  assert.equal(out.body.ok, true);
  assert.equal(out.body.adopted, true, 'the live pane is ADOPTED, not resumed into a duplicate');
  assert.deepEqual(state.killed, [], 'adoption never kills the live pane');
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(spawnId).status, 'live');
  const card = cardOf(core, sid);
  assert.equal(card.endedAt, null);
  assert.notEqual(card.col, 'offline', 'the terminal is shown again immediately, not one poll later');
});

// ---------------------------------------------------------------------------
// BUG 4 — mail is bounded and truncation is signalled, never silently dropped
// ---------------------------------------------------------------------------

test('BUG 4: oversize mail is bounded and postMail reports truncated + original_length', async (t) => {
  const { db, core } = memoryCore(t);
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mail-big-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  const big = 'x'.repeat(9000);
  const out = await core.postMail({ to: sid, from: 'ops', text: big });

  assert.equal(out.ok, true);
  assert.equal(out.delivered, 1);
  assert.equal(out.truncated, true, 'the sender is TOLD the tail was cut — never a silent {ok:true}');
  assert.equal(out.original_length, 9000);
  assert.ok(out.max_length >= 1000 && out.max_length < 9000, 'the cap is a sane, bounded few-KB value');
  const stored = db.prepare('SELECT text FROM mail WHERE to_session = ? ORDER BY id DESC LIMIT 1').get(sid).text;
  assert.equal(stored.length, out.max_length, 'the stored body is clamped to the cap');
  // It is NOT the old 500-char clamp that ate the bug report.
  assert.ok(stored.length > 500, 'the cap is far larger than the old 500-char limit that truncated real messages');
});

test('BUG 4: a normal-sized message is delivered whole with no truncated flag', async (t) => {
  const { db, core } = memoryCore(t);
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mail-ok-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  const msg = 'the board lost every live terminal today — please look';
  const out = await core.postMail({ to: sid, from: 'ops', text: msg });

  assert.equal(out.ok, true);
  assert.equal(out.delivered, 1);
  assert.equal(out.truncated, undefined, 'no truncation flag when nothing was cut');
  assert.equal('original_length' in out, false);
  const stored = db.prepare('SELECT text FROM mail WHERE to_session = ? ORDER BY id DESC LIMIT 1').get(sid).text;
  assert.equal(stored, msg, 'normal mail is stored verbatim');
});

// ===========================================================================
// AUDIT HARDENING — edge gaps in the NEW resurrection / revive-adoption code
// (and one over-expiry residual) found by adversarial review. Numbers below
// are the fix numbers from the hardening task.
// ===========================================================================

// A terminal, resume-eligible board spawn: durable row set to `status`, card
// tombstoned, cwd + resume transcript on disk, HOME pointed at a temp dir so
// revive()'s os.homedir()-based transcript lookup resolves. paneDead flips the
// reused window's pane dead (the RELAUNCH path); otherwise it stays a live
// claude (the ADOPT path).
async function reviveScenario(t, { status = 'gone', paneDead = false } = {}) {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-hard-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-hard-cwd-'));
  const tmux = fakeTmux();
  const ctx = memoryCore(t, { tmux, env: { HOME: userHome } });
  t.after(() => {
    rmSync(userHome, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  });
  const spawn = await ctx.core.spawn({ cwd });
  const sid = spawn.body.session_id;
  const oldId = spawn.body.spawn_id;
  const window = spawn.body.tmux.window;
  ctx.core.hookSessionStart({ session_id: sid, cwd, source: 'startup' }); // pane live claude, spawn → live
  ctx.db.prepare('UPDATE spawns SET status = ? WHERE spawn_id = ?').run(status, oldId);
  ctx.db.prepare("UPDATE sessions SET col = 'offline', note = 'pane died', ended_at = ?, archived_at = ? WHERE session_id = ?")
    .run(Date.now(), Date.now(), sid);
  const transcript = claudeTranscriptPath(cwd, sid, userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{}\n');
  if (paneDead) ctx.state.windows[0].pane_dead = true;
  return { ...ctx, sid, oldId, window, cwd, userHome };
}

// ---------------------------------------------------------------------------
// Fix 1 [HIGH] — revive() is single-flight: two near-simultaneous revives for
// one session must never both launch a pane.
// ---------------------------------------------------------------------------

test('Fix 1 [HIGH]: two concurrent revives for one session launch exactly ONE pane', async (t) => {
  // paneDead → the RELAUNCH path (kill remnant, insert provisional row, then
  // newWindow) — the path that, without a single-flight claim, inserted TWO
  // provisional rows and called newWindow TWICE for one session.
  const { core, db, state, sid, oldId } = await reviveScenario(t, { status: 'gone', paneDead: true });
  const windowsBefore = state.windows.length;

  // Fire both before awaiting either: revive #1 crosses its first await (the
  // window inspection) and suspends BEFORE inserting its row; revive #2 then
  // runs its synchronous prologue and must be refused by the in-memory claim.
  const [r1, r2] = await Promise.all([core.revive(oldId), core.revive(oldId)]);

  assert.deepEqual([r1.status, r2.status].sort(), [200, 409],
    'exactly one revive succeeds; the concurrent one is refused');
  const refused = [r1, r2].find(r => r.status === 409);
  assert.match(refused.body.reason, /already being revived/,
    'the loser is refused by the single-flight claim, not a generic error');

  const liveEligible = db.prepare(
    "SELECT COUNT(*) AS n FROM spawns WHERE session_id = ? AND status IN ('provisioning','spawning','stalled','live')").get(sid).n;
  assert.equal(liveEligible, 1, 'exactly one new live-eligible row — never two panes for one session');
  assert.equal(state.windows.length, windowsBefore + 1, 'newWindow ran exactly once');

  // The claim is RELEASED, not stuck: with the fresh row goned, a later revive
  // is refused by the ACTIVE-spawn guard, not by a lingering "being revived".
  const freshId = db.prepare("SELECT spawn_id FROM spawns WHERE session_id = ? AND status = 'spawning'").get(sid).spawn_id;
  db.prepare("UPDATE spawns SET status = 'gone' WHERE spawn_id = ?").run(freshId);
  db.prepare("UPDATE sessions SET col = 'offline', ended_at = ? WHERE session_id = ?").run(Date.now(), sid);
  const again = await core.revive(freshId);
  assert.notEqual(again.status, 409, 'the single-flight claim was released; a later solo revive is not blocked');
});

// ---------------------------------------------------------------------------
// Fix 2 [MED] — revive-adoption must mirror the liveness tick's guards: never
// adopt a non-newest row, never resurrect a 'killed' row.
// ---------------------------------------------------------------------------

test('Fix 2 [MED]: reviving a NON-newest id whose window is a live claude refuses (no double-live, no un-killable row)', async (t) => {
  const { core, db, sid, oldId, window } = await reviveScenario(t, { status: 'pane-dead', paneDead: false });
  // Seed a NEWER pane-dead row naming the SAME reused window — it outranks the
  // older row in currentWindowOwner (newest requested_at wins).
  const orig = db.prepare('SELECT * FROM spawns WHERE spawn_id = ?').get(oldId);
  const newerId = 'newer-' + oldId;
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'pane-dead')`)
    .run(newerId, sid, orig.callsign, orig.tmux_session, window, orig.cwd, orig.worktree_path, orig.requested_at + 1000);

  const out = await core.revive(oldId);

  assert.equal(out.status, 409, 'the older row does not own the window — adoption is refused');
  assert.equal(out.body.current_spawn_id, newerId, 'the caller is told which row actually owns the window');
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(oldId).status, 'pane-dead',
    'the older row is NOT resurrected');
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(newerId).status, 'pane-dead',
    'the newer row is untouched');
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM spawns WHERE status = 'live'").get().n, 0,
    'no row was flipped live — countActiveSpawns cannot double-count and no row becomes un-killable');
});

test('Fix 2 [MED]: revive never resurrects a KILLED row by adoption even when its window is a live claude', async (t) => {
  const { core, db, sid, oldId } = await reviveScenario(t, { status: 'killed', paneDead: false });

  const out = await core.revive(oldId);

  assert.equal(out.status, 409, 'a human kill is a decision — adoption never undoes it');
  assert.match(out.body.reason, /killed/);
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(oldId).status, 'killed',
    'the killed row stays killed ("a human kill never resurrects")');
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM spawns WHERE status = 'live'").get().n, 0,
    'nothing was flipped live');
  assert.equal(db.prepare('SELECT col FROM sessions WHERE session_id = ?').get(sid).col, 'offline',
    'the tombstoned card stays offline');
});

// ---------------------------------------------------------------------------
// Fix 3 [MED] — resurrection↔condemnation thrash: one consistent probe + a
// 2-consecutive-dead-read hysteresis.
// ---------------------------------------------------------------------------

test('Fix 3 [MED]: a single transient dead read does NOT condemn-then-flap a steadily-live claude', async (t) => {
  const { core, db, state } = memoryCore(t);
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-hyst-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  const spawnId = spawn.body.spawn_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });
  const statusOf = () => db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(spawnId).status;
  assert.equal(statusOf(), 'live');

  state.windows[0].pane_dead = true; // ONE transient dead read (a momentary tmux glitch)
  await core.spawnLivenessTick();
  assert.equal(statusOf(), 'live', 'a single dead read must not condemn a live spawn (hysteresis arms, does not fire)');
  assert.notEqual(cardOf(core, sid).col, 'offline', 'the card is never tombstoned by a transient read');

  state.windows[0].pane_dead = false; // recovered
  await core.spawnLivenessTick();
  assert.equal(statusOf(), 'live', 'the streak resets on the live read — no live↔pane-dead flap');
  assert.equal(core.snapshot().ticker.filter(x => /pane died|restored/.test(x.msg)).length, 0,
    'no "pane died"/"restored" feed spam — nothing flapped');
});

test('Fix 3 [MED]: a genuinely dead pane is still condemned — on the second consecutive dead read', async (t) => {
  const { core, db, state } = memoryCore(t);
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-hyst2-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  const spawnId = spawn.body.spawn_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });
  const statusOf = () => db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(spawnId).status;

  state.windows[0].pane_dead = true;
  await core.spawnLivenessTick();
  assert.equal(statusOf(), 'live', 'the first dead read only arms the hysteresis streak');
  await core.spawnLivenessTick();
  assert.equal(statusOf(), 'pane-dead', 'a persistently dead pane IS condemned on the second consecutive read');
  assert.equal(cardOf(core, sid).col, 'offline');
});

test('Fix 3 [MED]: condemn and resurrect share ONE probe — a split pane (lowest=shell, active=claude) never condemns', async (t) => {
  const tmux = fakeTmux();
  // The lowest-index pane the scoped list reports is a shell, but the ACTIVE
  // pane is a live claude. The condemn loop must trust the SAME active-pane
  // probe the resurrect loop uses; otherwise it condemns on win.pane_cmd every
  // tick while the resurrect loop revives off paneCurrentCommand every tick.
  tmux.adapter.paneCurrentCommand = async () => ({ dead: false, cmd: 'claude' });
  const { core, db, state } = memoryCore(t, { tmux });
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-split-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  const spawnId = spawn.body.spawn_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });
  state.windows[0].pane_cmd = 'zsh'; // scoped lowest pane reads as a bare shell

  await core.spawnLivenessTick();
  await core.spawnLivenessTick(); // twice — proves it is the consistent probe, not just hysteresis delaying it
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(spawnId).status, 'live',
    'the active pane is a live claude, so condemn agrees with resurrect and never condemns');
  assert.notEqual(cardOf(core, sid).col, 'offline');
});

// ---------------------------------------------------------------------------
// Fix 4 [LOW] — a resurrected card must be coherent immediately: no stale
// needs-you reason chip, and a refreshed last_seen.
// ---------------------------------------------------------------------------

test('Fix 4 [LOW]: resurrection clears a stale needs-you reason and refreshes last_seen', async (t) => {
  const { core, db } = memoryCore(t);
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-res-fields-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  const spawnId = spawn.body.spawn_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  // Condemned WHILE it wore a needs-you chip and with a stale last_seen, but the
  // pane kept running claude — the exact state Fix 4 must scrub on the way back.
  const stale = Date.now() - 5 * HOUR;
  db.prepare("UPDATE spawns SET status = 'pane-dead' WHERE spawn_id = ?").run(spawnId);
  db.prepare("UPDATE sessions SET col = 'offline', ended_at = ?, notification_type = 'spawn_stalled', last_seen = ? WHERE session_id = ?")
    .run(stale, stale, sid);

  await core.spawnLivenessTick();

  const s = db.prepare('SELECT * FROM sessions WHERE session_id = ?').get(sid);
  assert.equal(db.prepare('SELECT status FROM spawns WHERE spawn_id = ?').get(spawnId).status, 'live');
  assert.equal(s.notification_type, null, 'a resurrected card sheds the stale needs-you reason chip');
  assert.ok(s.last_seen > stale, 'last_seen is refreshed so the very next retention sweep cannot re-presume it dead');
  assert.equal(s.col, 'idle');
});

// ---------------------------------------------------------------------------
// Fix 6 [LOW] — mail truncation must not split a Unicode astral character.
// ---------------------------------------------------------------------------

test('Fix 6 [LOW]: mail truncation never splits a surrogate pair (no lone high surrogate stored)', async (t) => {
  const { db, core } = memoryCore(t);
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-astral-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  // Land an astral emoji (U+1F600 = a UTF-16 surrogate PAIR) straddling the
  // 4000-code-unit clamp: 3999 filler + emoji at units [3999,4000] + a tail.
  const raw = 'x'.repeat(3999) + '\u{1F600}' + 'tail';
  const out = await core.postMail({ to: sid, from: 'ops', text: raw });

  assert.equal(out.truncated, true);
  assert.equal(out.original_length, raw.length, 'original_length stays code-unit based (unchanged semantics)');
  assert.equal(out.max_length, 4000);
  const stored = db.prepare('SELECT text FROM mail WHERE to_session = ? ORDER BY id DESC LIMIT 1').get(sid).text;
  assert.ok(stored.isWellFormed(), 'the stored body is a well-formed string — no orphaned high surrogate');
  assert.equal(stored.length, 3999, 'the split emoji is dropped whole rather than left half-encoded');
  const lastCode = stored.charCodeAt(stored.length - 1);
  assert.ok(lastCode < 0xd800 || lastCode > 0xdbff, 'the final code unit is not an unpaired high surrogate');
});

test('Fix 6 [LOW]: an astral character fully within the limit is preserved intact', async (t) => {
  const { db, core } = memoryCore(t);
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-astral2-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  const raw = '\u{1F600} ship it';
  const out = await core.postMail({ to: sid, from: 'ops', text: raw });

  assert.equal(out.truncated, undefined, 'nothing was cut → no truncation flag');
  const stored = db.prepare('SELECT text FROM mail WHERE to_session = ? ORDER BY id DESC LIMIT 1').get(sid).text;
  assert.equal(stored, raw, 'a message under the cap is stored verbatim, emoji and all');
});

// ---------------------------------------------------------------------------
// Fix 7 [LOW] — a crashed FLEETDECK_SPAWN_CMD override session (no tmux window)
// must be presumed dead on silence, not lingered active forever.
// ---------------------------------------------------------------------------

test('Fix 7 [LOW]: a silent override-process spawn (no tmux window) is presumed dead, not lingered active', async (t) => {
  const tmux = fakeTmux();
  tmux.adapter.spawnOverrideCmd = () => '/fake-spawn-override'; // the daemon is in FLEETDECK_SPAWN_CMD mode
  tmux.adapter.launchOverride = () => {};
  const { db, core } = memoryCore(t, { tmux });
  const now = Date.now();
  const silent = now - 4 * HOUR; // > PRESUME_DEAD_MS (default 3h)

  // A board spawn whose first hook already flipped source to 'hooks', now
  // silent for 4h — and no tmux window exists (an override launches a detached
  // process). tmux adjudication would read the absent window as UNKNOWN and
  // never condemn it → it would linger active forever.
  db.prepare(`INSERT INTO sessions
    (session_id, callsign, col, note, events, started_at, last_seen, source)
    VALUES ('ov', 'ov-1', 'idle', 'waiting at its prompt', 0, ?, ?, 'hooks')`).run(silent, silent);
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
    VALUES ('sp-ov', 'ov', 'ov-1', 'fleetdeck-4711', 'fd4711-ov-1', ?, 'live')`).run(silent);
  // Deliberately push NO window into tmux — an override process has none.

  await core.retentionSweep(now);

  const s = db.prepare("SELECT * FROM sessions WHERE session_id = 'ov'").get();
  assert.equal(s.col, 'offline', 'silence is the only signal a pane-less override process exposes → presume it dead');
  assert.ok(s.ended_at);
  assert.match(s.note, /presumed ended \(silent .+h\)/);
  assert.equal(db.prepare("SELECT status FROM spawns WHERE spawn_id = 'sp-ov'").get().status, 'pane-dead',
    'the override spawn row is condemned (revivable), never left stale live');
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM spawns WHERE status IN ('spawning','stalled','live')").get().n, 0,
    'it stops counting toward active spawns');
});
