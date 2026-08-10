import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from '../scripts/fleetd/db.ts';
import { startDaemon } from './helpers/daemon.ts';
import { getJson, postHook, postJson } from './helpers/http.ts';
import { makeRepoWithWorktree } from './helpers/gitrepo.ts';
import { waitUntil } from './helpers/wait.ts';

// tests/rename.test.mjs — 0.7.1 custom names.
//
// A callsign is <animal>-<suffix>. The animal is the fleet's identity (12 of
// them, rotating) and is never the human's to choose; the SUFFIX is the part
// that should say what the session is doing, so a human may set it — from the
// board (POST /api/sessions/:id/name) or from Compose (`name <target> <suffix>`).
// Both doors open onto ONE core write, so the rules can never drift apart.
//
// The charset is load-bearing, not taste: the board's ticker filters a card's
// timeline by matching its callsign on a [^A-Za-z0-9-] boundary, and a pane's
// tmux window is fd<port>-<callsign>. A space or a dot in a name would quietly
// break a card's history and its pane addressing — so the refusals below are
// part of the contract, not politeness.

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPAWN_CMD_FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.mjs');
try { chmodSync(SPAWN_CMD_FIXTURE, 0o755); } catch { /* best effort */ }

function scratch(prefix) {
  return mkdtempSync(path.join(tmpdir(), prefix));
}

function withDb(home, fn) {
  const db = openDb(path.join(home, 'fleetd.db'));
  try { return fn(db); } finally { db.close(); }
}

function cardOf(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

async function boot(t, prefix) {
  const home = scratch(`${prefix}-daemon-`);
  const cwd = scratch(`${prefix}-cwd-`);
  const daemon = await startDaemon({ home, env: { FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE } });
  t.after(async () => {
    await daemon.stop({ keepHome: true });
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return { daemon, home, cwd };
}

async function startSession(daemon, cwd) {
  const sid = randomUUID();
  const res = await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd, source: 'startup' }, { token: daemon.token });
  return { sid, callsign: res.json.callsign, animal: res.json.callsign.split('-')[0] };
}

const rename = (daemon, sid, body) => postJson(`${daemon.baseUrl}/api/sessions/${sid}/name`, body);
const command = (daemon, text) => postJson(`${daemon.baseUrl}/command`, { text });

const git = (args, cwd) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
const writeSeed = (dir, content) => writeFileSync(path.join(dir, 'seed.txt'), content);
const sleep = (ms) => new Promise(resolve => setTimeout(resolve, ms));

const tmuxOk = () => {
  try { execFileSync('tmux', ['-V'], { stdio: 'ignore' }); return true; } catch { return false; }
};
const tmux = (socket, args) => execFileSync('tmux', ['-L', socket, ...args], { encoding: 'utf8' }).trim();

test('renaming keeps the animal and takes the suffix, and answers in the shape Compose already renders', async (t) => {
  const { daemon, home, cwd } = await boot(t, 'fleetdeck-rename-happy');
  const { sid, callsign, animal } = await startSession(daemon, cwd);

  const res = await rename(daemon, sid, { suffix: 'docs-review' });
  assert.equal(res.status, 200, JSON.stringify(res.json));
  assert.equal(res.json.ok, true);
  assert.equal(res.json.renamed, true);
  assert.equal(res.json.callsign, `${animal}-docs-review`, 'the animal survived; the suffix is the human’s');
  assert.equal(res.json.previous, callsign);

  const card = cardOf((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.callsign, `${animal}-docs-review`);
  // The birth name is kept as the mail anchor (write-once), so anything still
  // addressed to the name peers were told keeps landing.
  assert.equal(card.prev_callsign, callsign);
  const row = withDb(home, db => db.prepare('SELECT custom_suffix FROM sessions WHERE session_id = ?').get(sid));
  assert.equal(row.custom_suffix, 'docs-review', 'the card is marked human-named');
});

test('mail addressed to the birth name still reaches a renamed session', async (t) => {
  const { daemon, cwd } = await boot(t, 'fleetdeck-rename-mail');
  const { sid, callsign } = await startSession(daemon, cwd);
  await rename(daemon, sid, { suffix: 'shipping' });

  const res = await postJson(`${daemon.baseUrl}/mail`, { to: callsign, from: 'operator', text: 'still finds you' }, { token: daemon.token });
  assert.equal(res.status, 200, JSON.stringify(res.json));
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.mail_meta[sid].queued, 1, 'the old name is a fallback route, not a dead letter');
});

test('bad suffixes are refused loudly — charset, leading dash, length, reserved words', async (t) => {
  const { daemon, cwd } = await boot(t, 'fleetdeck-rename-refuse');
  const { sid } = await startSession(daemon, cwd);

  // The charset is what the board's ticker filter and the tmux window name are
  // built on, so these are contract refusals, not fussiness.
  for (const bad of ['docs review', 'docs.review', 'docs_review', '-leading', '', 'x'.repeat(25)]) {
    const res = await rename(daemon, sid, { suffix: bad });
    assert.equal(res.status, 400, `"${bad}" must be refused`);
    assert.match(res.json.reason, /letters, digits and dashes/);
  }
  // Reserved: the mail router resolves these before it ever looks at a callsign,
  // so a card named `all` could never be messaged directly.
  for (const reserved of ['all', 'everyone']) {
    const res = await rename(daemon, sid, { suffix: reserved });
    assert.equal(res.status, 400);
    assert.match(res.json.reason, /reserved/);
  }
});

test('a name already held by another card is refused', async (t) => {
  const { daemon, cwd } = await boot(t, 'fleetdeck-rename-collide');
  // The animal rotation is 12 deep, so on a fresh daemon the 1st and the 13th
  // session are guaranteed the SAME animal — create that pair deliberately so
  // the clash below is deterministic, not a coin flip the test can skip.
  const sessions = [];
  for (let i = 0; i < 13; i++) sessions.push(await startSession(daemon, cwd));
  const a = sessions[0];
  const b = sessions[12];
  assert.equal(b.animal, a.animal, 'sessions 1 and 13 wrap the 12-animal rotation onto the same animal');
  // Name A explicitly, then give B the same suffix: same animal + same suffix =
  // the same full callsign, and the daemon must refuse it unconditionally.
  await rename(daemon, a.sid, { suffix: 'hot-seat' });
  const bRenamed = await rename(daemon, b.sid, { suffix: 'hot-seat' });
  assert.equal(bRenamed.status, 409, 'same animal + same suffix = the same card name');
  assert.match(bRenamed.json.reason, /already taken/);
  // And B keeps its own name — the refusal must not have renamed anything.
  const card = cardOf((await getJson(`${daemon.baseUrl}/state`)).json, b.sid);
  assert.equal(card.callsign, b.callsign);
});

test('clearing a custom name reverts to the ticket name, or to the birth name when there is no ticket', async (t) => {
  const { daemon, cwd } = await boot(t, 'fleetdeck-rename-clear');
  const { sid, callsign, animal } = await startSession(daemon, cwd);

  // No ticket → clear reverts to the automatic <animal>-<sid4> birth name.
  await rename(daemon, sid, { suffix: 'temporary' });
  let res = await rename(daemon, sid, { clear: true });
  assert.equal(res.status, 200);
  assert.equal(res.json.callsign, callsign, 'back to the name it was born with');

  // With a ticket → clear reverts to the ticket name.
  await command(daemon, `ticket ${callsign} PROJ-9`);
  await rename(daemon, sid, { suffix: 'renamed-again' });
  res = await rename(daemon, sid, { clear: true });
  assert.equal(res.status, 200);
  assert.equal(res.json.callsign, `${animal}-PROJ-9`, 'the automatic name for a ticketed card is its ticket name');
});

test('a human name outranks branch auto-detection: a real ticket-branch checkout must not rename or ticket it', async (t) => {
  const { daemon, home } = await boot(t, 'fleetdeck-rename-precedence');
  // A real checkout, not a scratch dir: the defect this guards lives in the
  // daemon's server-side branchOf() auto path, which a plain directory can
  // never reach (BUG-170 — the old version of this test only exercised the
  // manual `ticket` command, which uses source='manual' and would keep passing
  // even if branch auto-detection started overwriting custom names).
  const repo = makeRepoWithWorktree({ repoName: 'fd-rename-precedence', branch: 'plain-work' });
  t.after(() => repo.cleanup());

  // Ticketless birth on the plain branch → hex suffix, no ticket recorded.
  const sid = randomUUID();
  const start = await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd: repo.worktree, source: 'startup' }, { token: daemon.token });
  const animal = start.json.callsign.split('-')[0];
  await rename(daemon, sid, { suffix: 'my-name' });
  const named = cardOf((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(named.callsign, `${animal}-my-name`);

  // Switch the worktree to a ticket-bearing branch; the daemon derives the
  // branch server-side from cwd on the NEXT hook event — exactly the path that
  // must never overwrite an explicit human name.
  writeSeed(repo.worktree, 'seed on plain-work\n');
  git(['add', '.'], repo.worktree);
  git(['commit', '-q', '-m', 'seed plain-work'], repo.worktree);
  git(['switch', '-q', '-c', 'feature/PROJ-7-auto'], repo.worktree);
  // The daemon's branch cache holds 'plain-work' for up to 20 s — sit it out
  // so the event below cannot pass for the wrong reason (stale branch read).
  await sleep(21_000);
  await postHook(daemon.baseUrl, 'UserPromptSubmit', { session_id: sid, cwd: repo.worktree, prompt: 'back on the ticket branch' }, { token: daemon.token });

  const card = cardOf((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(card.branch, 'feature/PROJ-7-auto', 'the daemon saw the ticket branch — the auto path was actually reachable');
  assert.equal(card.callsign, `${animal}-my-name`, 'automation never overwrites a human name');
  assert.equal(card.ticket ?? null, null, 'branch detection must not even RECORD a ticket over a custom name');
  const core = withDb(home, db => db.prepare('SELECT custom_suffix, ticket_source FROM sessions WHERE session_id = ?').get(sid));
  assert.equal(core.custom_suffix, 'my-name');
  assert.equal(core.ticket_source ?? null, null);
});

test('an explicit ticket command still wins over a human name', async (t) => {
  const { daemon, home, cwd } = await boot(t, 'fleetdeck-rename-manual-wins');
  const { sid, animal } = await startSession(daemon, cwd);
  await rename(daemon, sid, { suffix: 'my-name' });

  const res = await command(daemon, `ticket ${animal}-my-name PROJ-1`);
  // The manual `ticket` command IS an explicit human act, so it may rename —
  // and it clears the custom name on its way through (the card is now
  // ticket-named, and `name … clear` reverts to exactly that).
  assert.equal(res.json.callsign, `${animal}-PROJ-1`);
  const after = withDb(home, db => db.prepare('SELECT custom_suffix, ticket_source FROM sessions WHERE session_id = ?').get(sid));
  assert.equal(after.custom_suffix, null, 'an explicit ticket takes the name over from an explicit name');
  assert.equal(after.ticket_source, 'manual');
});

test('the `name` command mirrors the REST route and is never silently filed as a note', async (t) => {
  const { daemon, cwd } = await boot(t, 'fleetdeck-rename-command');
  const { sid, callsign, animal } = await startSession(daemon, cwd);

  const ok = await command(daemon, `name ${callsign} via-compose`);
  assert.equal(ok.json.renamed, true);
  assert.equal(ok.json.callsign, `${animal}-via-compose`);
  assert.equal(ok.json.previous, callsign);
  assert.equal(ok.json.session_id, sid);

  // A malformed rename is an error to show, never a note to file.
  const bare = await command(daemon, 'name');
  assert.equal(bare.json.ok, false);
  assert.match(bare.json.reason, /usage: name/);
  const bad = await command(daemon, `name ${animal}-via-compose has spaces`);
  assert.equal(bad.json.ok, false);
  const unknown = await command(daemon, 'name nobody-here whatever');
  assert.equal(unknown.json.ok, false);
  assert.match(unknown.json.reason, /no live session/);
  const scoped = await command(daemon, 'name all whatever');
  assert.equal(scoped.json.ok, false);
  assert.match(scoped.json.reason, /one session/);
});

// This one drives a REAL pane: no FLEETDECK_SPAWN_CMD, so the spawn path opens
// a genuine window on the test-isolated tmux server (startDaemon scopes
// FLEETDECK_TMUX_SOCKET per port and stop() reaps it). A regression that
// re-targets pane operations at the mutable callsign instead of the frozen
// spawn window cannot hide here — the rename and the kill both resolve
// against a pane that actually exists. (BUG-171: the previous version asserted
// only DB/snapshot fields, so a stranded renamed pane still read green.)
test('renaming a session with a live pane keeps its pane: the frozen tmux window still drives it', { skip: !tmuxOk() && 'tmux unavailable' }, async (t) => {
  const home = scratch('fleetdeck-rename-pane-daemon-');
  const cwd = scratch('fleetdeck-rename-pane-cwd-');
  const daemon = await startDaemon({
    home,
    env: {
      SHELL: '/bin/bash',
      FLEETDECK_AGENTS_POLL_MS: '100',
      FLEETDECK_NUDGE_MS: '60000',
    },
  });
  const socket = `fleetdeck-test-${daemon.port}`;
  t.after(async () => {
    await daemon.stop({ keepHome: true });
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { kind: 'shell', cwd });
  assert.equal(spawned.status, 200, JSON.stringify(spawned.json));
  const sid = spawned.json.session_id;
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd, source: 'startup' }, { token: daemon.token });
  const before = cardOf((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  const window = before.spawn.tmux_window;
  assert.equal(spawned.json.tmux.window, window, 'the row names the window the daemon just created');

  const res = await rename(daemon, sid, { suffix: 'renamed-worker' });
  assert.equal(res.status, 200);

  // The tmux window is an internal handle, deliberately NOT renamed: every pane
  // operation (mail-to-pane, kill, revive, liveness, the terminal bridge) reads
  // spawns.tmux_window, so the pane keeps working. Renaming the window instead
  // would risk a row that names a window tmux no longer has — which the boot
  // reconcile would read as a dead pane and condemn.
  const after = cardOf((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(after.callsign, res.json.callsign, 'the card took the new name');
  assert.equal(after.spawn.tmux_window, window, 'and kept the window that actually exists');
  assert.equal(after.spawn.status, 'live', 'the pane is still live and still owned');

  // The post-rename proof this test exists for: a pane operation must still
  // reach the REAL pane. The frozen window is on the test's isolated tmux
  // server — resolved by the recorded window name, never by the new callsign.
  tmux(socket, ['send-keys', '-t', `fleetdeck-${daemon.port}:${window}`, 'echo still-drivable', 'Enter']);
  const seen = await waitUntil(() => {
    try { return tmux(socket, ['capture-pane', '-p', '-t', `fleetdeck-${daemon.port}:${window}`]); } catch { return null; }
  }, { timeoutMs: 5000, label: 'typed line to reach the renamed pane' });
  assert.match(seen, /still-drivable/, 'the frozen window accepted input after the rename');

  // And the daemon's own post-rename pane operation — the name-verified kill —
  // must target that same recorded window: 200 with the window actually gone
  // from the real server (the session's default window 0 may outlive it).
  const killed = await postJson(`${daemon.baseUrl}/api/spawn/${before.spawn.spawn_id}/kill`, { force: true });
  assert.equal(killed.status, 200, JSON.stringify(killed.json));
  const remaining = tmux(socket, ['list-windows', '-t', `fleetdeck-${daemon.port}`, '-F', '#W']).split('\n');
  assert.ok(!remaining.includes(window), `kill-window removed ${window} from the real tmux server (left: ${remaining})`);
});

test('an offline session cannot be renamed — its name is on its way back to the pool', async (t) => {
  const { daemon, cwd } = await boot(t, 'fleetdeck-rename-offline');
  const { sid } = await startSession(daemon, cwd);
  await postHook(daemon.baseUrl, 'SessionEnd', { session_id: sid, cwd, reason: 'logout' }, { token: daemon.token });

  const res = await rename(daemon, sid, { suffix: 'too-late' });
  assert.equal(res.status, 409);
  assert.match(res.json.reason, /no live session/);
});

test('POST /api/sessions/:id/name validates its body', async (t) => {
  const { daemon, cwd } = await boot(t, 'fleetdeck-rename-body');
  const { sid } = await startSession(daemon, cwd);
  const res = await rename(daemon, sid, { suffix: 42 });
  assert.equal(res.status, 400);
  assert.match(res.json.reason, /suffix must be a string/);
  const unknown = await rename(daemon, randomUUID(), { suffix: 'ghost' });
  assert.equal(unknown.status, 409);
  assert.match(unknown.json.reason, /no live session/);
});
