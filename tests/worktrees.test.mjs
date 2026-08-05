import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from '../scripts/fleetd/db.mjs';
import * as fleetdHelpers from '../scripts/fleetd/helpers.mjs';
import { createStatements } from '../scripts/fleetd/statements.mjs';
import { createWorktrees } from '../scripts/fleetd/worktrees.mjs';
import { claudeTranscriptPath } from '../scripts/fleetd/derive.mjs';

// The BUG-060 fix introduces createKeyedMutex; the pre-fix tree does not have
// it. Resolve it dynamically so THIS test file still loads against the
// unfixed sources — the race test below must fail on its own assertion (the
// revive launching into the removed checkout), not on a module-load error.
// Direct createWorktrees ctx below falls back to a fresh mutex per call,
// which preserves those tests' single-removal semantics exactly.
const createKeyedMutex = fleetdHelpers.createKeyedMutex ?? (() => { throw new Error('createKeyedMutex missing'); });
const freshMutexCtx = () => (fleetdHelpers.createKeyedMutex ? { acquireWorktreePathLock: fleetdHelpers.createKeyedMutex() } : {});
import { startDaemon } from './helpers/daemon.mjs';
import { makeRepoWithWorktree, makePlainDir, makeRemoteRepo } from './helpers/gitrepo.mjs';
import { getJson, postJson } from './helpers/http.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPAWN_CMD_FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.mjs');
try { chmodSync(SPAWN_CMD_FIXTURE, 0o755); } catch { /* best effort */ }

function git(args, cwd) {
  return execFileSync('git', args, {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
}

function withDb(home, fn) {
  const db = openDb(path.join(home, 'fleetd.db'));
  try { return fn(db); } finally { db.close(); }
}

// Tests create the durable ownership evidence directly. That keeps these
// contracts about inspection/removal rather than tmux launch mechanics, while
// still crossing the real HTTP, SQLite, filesystem, and git boundaries.
function ownWorktree(home, repo, {
  sessionId = 'worktree-session',
  callsign = 'otter',
  alive = false,
  spawnId = `spawn-${sessionId}`,
} = {}) {
  const now = Date.now();
  withDb(home, db => {
    db.prepare(`INSERT INTO sessions
      (session_id, callsign, cwd, branch, col, note, events, started_at, last_seen, ended_at, archived_at, source)
      VALUES (?, ?, ?, 'wt-branch', ?, 'test worktree', 0, ?, ?, ?, ?, 'spawned')`)
      .run(sessionId, callsign, repo.worktree, alive ? 'idle' : 'offline', now, now,
        alive ? null : now, alive ? null : now);
    db.prepare(`INSERT INTO spawns
      (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
      VALUES (?, ?, ?, 'fleetdeck-test', ?, ?, ?, ?, ?)`)
      .run(spawnId, sessionId, callsign, `fd-${callsign}`, repo.root, repo.worktree, now,
        alive ? 'live' : 'pane-dead');
  });
}

test('GET /api/worktrees follows clean, dirty, ahead/no-upstream, and gone real git state', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-inspect' });
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); repo.cleanup(); });
  ownWorktree(daemon.home, repo);

  let response = await getJson(`${daemon.baseUrl}/api/worktrees`);
  assert.equal(response.status, 200);
  assert.equal(response.json.ok, true);
  assert.equal(response.json.worktrees.length, 1, 'distinct spawn paths are returned once');
  let item = response.json.worktrees[0];
  assert.equal(item.path, repo.worktree);
  assert.equal(item.exists, true);
  assert.equal(item.callsign, 'otter');
  assert.equal(item.session_id, 'worktree-session');
  assert.equal(item.session_alive, false);
  assert.equal(item.spawn_status, 'pane-dead');
  assert.equal(item.branch, 'wt-branch');
  assert.equal(item.dirty, 0);
  assert.deepEqual(item.dirty_files, []);
  assert.equal(item.ahead, 0);
  assert.ok(['main', 'master'].includes(item.base));
  assert.equal(item.upstream, null);
  assert.equal(item.unpushed, 0);
  assert.equal(item.merged, true);
  assert.equal(item.verdict, 'safe');
  assert.match(item.last_commit.sha, /^[0-9a-f]+$/);
  assert.equal(item.last_commit.subject, 'seed');
  assert.equal(typeof item.last_commit.at, 'number');

  writeFileSync(path.join(repo.worktree, 'precious.txt'), 'not committed\n');
  response = await getJson(`${daemon.baseUrl}/api/worktrees`);
  item = response.json.worktrees[0];
  assert.equal(item.verdict, 'has-work');
  assert.ok(item.dirty > 0);
  assert.ok(item.dirty_files.some(file => file.includes('precious.txt')));

  git(['add', 'precious.txt'], repo.worktree);
  git(['commit', '-q', '-m', 'ahead locally'], repo.worktree);
  response = await getJson(`${daemon.baseUrl}/api/worktrees`);
  item = response.json.worktrees[0];
  assert.equal(item.dirty, 0);
  assert.equal(item.upstream, null);
  assert.ok(item.ahead > 0);
  assert.equal(item.unpushed, item.ahead, 'no upstream means every ahead commit is unpushed');
  assert.equal(item.verdict, 'has-work');

  rmSync(repo.worktree, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  response = await getJson(`${daemon.baseUrl}/api/worktrees`);
  item = response.json.worktrees[0];
  assert.equal(item.exists, false);
  assert.equal(item.verdict, 'gone');
  assert.equal(item.branch, null);
  assert.equal(item.dirty, null);
  assert.equal(item.unpushed, null);
});

test('POST /api/worktrees/remove rejects an arbitrary path absent from spawns', async (t) => {
  const daemon = await startDaemon();
  const arbitrary = makePlainDir();
  t.after(async () => { await daemon.stop(); arbitrary.cleanup(); });

  const response = await postJson(`${daemon.baseUrl}/api/worktrees/remove`, { path: arbitrary.dir, force: true });
  assert.equal(response.status, 400);
  assert.deepEqual(response.json, { ok: false, reason: 'not a fleet worktree' });
  assert.equal(existsSync(arbitrary.dir), true, 'unowned client paths are never removed');
});

test('POST remove refuses has-work without force, then force removes disk and archived DB rows', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-force-remove' });
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); repo.cleanup(); });
  ownWorktree(daemon.home, repo, { sessionId: 'archived-force' });
  writeFileSync(path.join(repo.worktree, 'uncommitted.txt'), 'valuable\n');

  let response = await postJson(`${daemon.baseUrl}/api/worktrees/remove`, { path: repo.worktree });
  assert.equal(response.status, 409);
  assert.equal(response.json.ok, false);
  assert.equal(response.json.verdict, 'has-work');
  assert.ok(response.json.dirty > 0);
  assert.equal(response.json.unpushed, 0);
  assert.equal(existsSync(repo.worktree), true);

  response = await postJson(`${daemon.baseUrl}/api/worktrees/remove`, { path: repo.worktree, force: true });
  assert.equal(response.status, 200);
  assert.deepEqual(response.json, {
    ok: true,
    removed: true,
    branch_deleted: false,
    rows_purged: 2,
    path: repo.worktree,
  });
  assert.equal(existsSync(repo.worktree), false);
  withDb(daemon.home, db => {
    assert.equal(db.prepare('SELECT 1 FROM spawns WHERE worktree_path = ?').get(repo.worktree), undefined);
    assert.equal(db.prepare("SELECT 1 FROM sessions WHERE session_id = 'archived-force'").get(), undefined);
    assert.ok(db.prepare('SELECT msg FROM ticker WHERE msg LIKE ?').get(`%${repo.worktree}%`));
  });
});

test('POST remove refuses while any owning session is alive', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-live-remove' });
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); repo.cleanup(); });
  ownWorktree(daemon.home, repo, { sessionId: 'still-live', alive: true });

  const response = await postJson(`${daemon.baseUrl}/api/worktrees/remove`, { path: repo.worktree, force: true });
  assert.equal(response.status, 409);
  assert.deepEqual(response.json, { ok: false, reason: 'session is still alive' });
  assert.equal(existsSync(repo.worktree), true);
  assert.ok(withDb(daemon.home, db => db.prepare('SELECT 1 FROM spawns WHERE worktree_path = ?').get(repo.worktree)));
});

// BUG #4, the remove-vs-revive TOCTOU, pinned deterministically. The liveness
// gate is decided BEFORE inspectWorktree's multi-second git probes and the repo
// rev-parse yield the event loop. A /api/spawn/:id/revive of the offline spawn
// during that window relaunches Claude in this very worktree by INSERTING a
// 'spawning' spawn row — but it leaves the session's ended_at set until a later
// hook, so a recheck keyed on session_ended_at is blind to it and force-removes
// the now-live tree. Racing a real daemon is nondeterministic (the window is
// milliseconds on a scratch repo), so we drive createWorktrees directly and land
// the revive at the exact dangerous instant: our wrapper inserts the live-eligible
// spawn row on the first worktreeSpawns.all() AFTER the initial gate — precisely
// what the concurrent revive does. The status-aware recheck must re-query fresh
// and abort with 409; the earlier ended_at-only version silently proceeds.
test('remove re-checks liveness after the git probes and aborts a revive that raced in', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-revive-race' });
  const home = mkdtempSync(path.join(tmpdir(), 'fd-revive-race-home-'));
  t.after(() => {
    repo.cleanup();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const db = openDb(path.join(home, 'fleetd.db'));
  t.after(() => db.close());
  const now = Date.now();
  // An OFFLINE spawn (ended_at set): the initial liveness gate lets removal proceed.
  db.prepare(`INSERT INTO sessions
    (session_id, callsign, cwd, branch, col, note, events, started_at, last_seen, ended_at, source)
    VALUES ('revive-race', 'otter', ?, 'wt-branch', 'offline', 'test', 0, ?, ?, ?, 'spawned')`)
    .run(repo.worktree, now, now, now);
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
    VALUES ('sp-revive-race', 'revive-race', 'otter', 'fleetdeck-test', ?, ?, ?, ?, 'pane-dead')`)
    .run(repo.root, repo.root, repo.worktree, now);

  const { q } = createStatements(db);
  // The initial gate reads worktreeSpawns twice (rows + the liveness check) with
  // the spawn still 'pane-dead' → passes. Once the awaited git probes yield, a
  // real POST /api/spawn/:id/revive lands: it INSERTS a live-eligible 'spawning'
  // spawn row for this worktree_path and deliberately does NOT clear the session's
  // ended_at (that waits for the resumed child's later SessionStart hook). So the
  // recheck must key on spawn STATUS, not session_ended_at — this wrapper injects
  // exactly that row on the first read after the gate. A fix that re-read
  // ended_at (the earlier, ineffective version) would MISS this and force-remove
  // the live tree; the status-aware recheck sees it and aborts 409.
  let calls = 0;
  let injected = false;
  const realStmt = q.worktreeSpawns;
  q.worktreeSpawns = {
    all: (...args) => {
      calls += 1;
      if (calls >= 3 && !injected) {
        injected = true;
        db.prepare(`INSERT INTO spawns
          (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
          VALUES ('sp-revive-inflight', 'revive-race', 'otter', 'fleetdeck-test', ?, ?, ?, ?, 'spawning')`)
          .run(repo.root, repo.root, repo.worktree, now + 1);
      }
      return realStmt.all(...args);
    },
  };

  const { removeWorktree } = createWorktrees({ q, tick() {}, onMutate() {}, ...freshMutexCtx() });
  const res = await removeWorktree({ path: repo.worktree, force: true });

  assert.equal(res.status, 409, `a revive during the probe window must abort removal (got ${JSON.stringify(res.body)})`);
  assert.equal(res.body.ok, false);
  assert.equal(res.body.reason, 'session became live during removal');
  assert.ok(injected && calls >= 3, 'the recheck re-queried worktreeSpawns fresh after the gate — not the stale rows closure');
  assert.equal(existsSync(repo.worktree), true, 'the now-live worktree survives — the force removal was aborted');
  assert.ok(
    db.prepare("SELECT 1 FROM spawns WHERE spawn_id = 'sp-revive-inflight'").get(),
    'the revived spawn row was not purged',
  );
});

// Direct, timing-free validation of the core status-aware predicate: a worktree
// whose spawn is mid-revive ('spawning') with the session's ended_at STILL SET
// (the real revive-in-flight state) must be refused at the very first gate. The
// earlier ended_at-only check let this through and force-removed a live tree.
test('remove refuses a worktree whose spawn is launching (revive in flight, ended_at still set)', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-launching' });
  const home = mkdtempSync(path.join(tmpdir(), 'fd-launching-home-'));
  t.after(() => {
    repo.cleanup();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const db = openDb(path.join(home, 'fleetd.db'));
  t.after(() => db.close());
  const now = Date.now();
  // ended_at SET — exactly what a revive leaves until its first hook — yet the
  // spawn row is 'spawning' (launching). Status must win.
  db.prepare(`INSERT INTO sessions
    (session_id, callsign, cwd, branch, col, note, events, started_at, last_seen, ended_at, source)
    VALUES ('launching', 'otter', ?, 'wt-branch', 'queued', 'test', 0, ?, ?, ?, 'spawned')`)
    .run(repo.worktree, now, now, now);
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
    VALUES ('sp-launching', 'launching', 'otter', 'fleetdeck-test', ?, ?, ?, ?, 'spawning')`)
    .run(repo.root, repo.root, repo.worktree, now);

  const { q } = createStatements(db);
  const { removeWorktree } = createWorktrees({ q, tick() {}, onMutate() {}, ...freshMutexCtx() });
  const res = await removeWorktree({ path: repo.worktree, force: true });

  assert.equal(res.status, 409, `a launching spawn must block removal (got ${JSON.stringify(res.body)})`);
  assert.equal(res.body.reason, 'session is still alive');
  assert.equal(existsSync(repo.worktree), true, 'the launching worktree survives');
});

// The FINAL gate: a revive that lands AFTER the pre-remove recheck but during the
// awaited `git worktree remove`/prune/branch ops. The tree is already gone by the
// time we notice, but the last check before deleteWorktreeSpawns must KEEP the
// now-live spawn/session rows so the card doesn't vanish (a lost-terminal bug).
test('remove keeps rows (rows_purged:0) when a revive lands during the git ops', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-final-gate' });
  const home = mkdtempSync(path.join(tmpdir(), 'fd-final-gate-home-'));
  t.after(() => {
    repo.cleanup();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const db = openDb(path.join(home, 'fleetd.db'));
  t.after(() => db.close());
  const now = Date.now();
  db.prepare(`INSERT INTO sessions
    (session_id, callsign, cwd, branch, col, note, events, started_at, last_seen, ended_at, source)
    VALUES ('final-gate', 'otter', ?, 'wt-branch', 'offline', 'test', 0, ?, ?, ?, 'spawned')`)
    .run(repo.worktree, now, now, now);
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
    VALUES ('sp-final-gate', 'final-gate', 'otter', 'fleetdeck-test', ?, ?, ?, ?, 'pane-dead')`)
    .run(repo.root, repo.root, repo.worktree, now);

  const { q } = createStatements(db);
  // Let the initial gate (#1, #2) and the pre-remove recheck (#3) pass, then inject
  // the revive's live row only on the FINAL worktreeSpawns.all() (#4, after the git
  // remove has already run). This is the one window the pre-remove gate cannot see.
  let calls = 0;
  let injected = false;
  const realStmt = q.worktreeSpawns;
  q.worktreeSpawns = {
    all: (...args) => {
      calls += 1;
      if (calls >= 4 && !injected) {
        injected = true;
        db.prepare(`INSERT INTO spawns
          (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
          VALUES ('sp-revive-late', 'final-gate', 'otter', 'fleetdeck-test', ?, ?, ?, ?, 'spawning')`)
          .run(repo.root, repo.root, repo.worktree, now + 1);
      }
      return realStmt.all(...args);
    },
  };

  const { removeWorktree } = createWorktrees({ q, tick() {}, onMutate() {}, ...freshMutexCtx() });
  const res = await removeWorktree({ path: repo.worktree, force: true });

  assert.equal(res.status, 200, `the tree is gone but this is not an error (got ${JSON.stringify(res.body)})`);
  assert.equal(res.body.removed, true);
  assert.equal(res.body.rows_purged, 0, 'the revive that raced in kept its rows');
  assert.equal(res.body.spawn_became_live, true);
  assert.ok(injected && calls >= 4, 'the final gate ran after the git ops');
  assert.equal(existsSync(repo.worktree), false, 'the tree WAS removed (git already ran)');
  assert.ok(
    db.prepare("SELECT 1 FROM spawns WHERE spawn_id = 'sp-revive-late'").get(),
    'the revived spawn row survived the purge',
  );
});

<<<<<<< /tmp/mf-ours
// BUG-058: the liveness gate read only worktreeSpawns, whose
// `worktree_path IS NOT NULL` filter drops cwd-only rows. A live shell (shells
// refuse worktree:true outright — always cwd-only) or an adopted Claude
// launched INTO a fleet worktree carries `worktree_path NULL, cwd = <tree>`,
// so removal saw no live owner and deleted the directory under the running
// process. The gate must key on the effective directory `worktree_path ?? cwd`
// and treat containment as a claim. Pinned at the createWorktrees level (the
// same direct-drive style as the revive-race tests above): three sub-cases —
// cwd at the tree root, cwd in a SUBdirectory, and a lookalike PREFIX path
// that must NOT count.
test('remove refuses while a cwd-only live shell occupies the worktree (BUG-058)', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-shell-live' });
  const home = mkdtempSync(path.join(tmpdir(), 'fd-shell-live-home-'));
  t.after(() => {
    repo.cleanup();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const db = openDb(path.join(home, 'fleetd.db'));
  t.after(() => db.close());
  const now = Date.now();

  // The ended, worktree-keyed owner row — the allow-list entry that makes the
  // path removable at all.
  db.prepare(`INSERT INTO sessions
    (session_id, callsign, cwd, branch, col, note, events, started_at, last_seen, ended_at, source)
    VALUES ('wt-owner', 'otter', ?, 'wt-branch', 'offline', 'test', 0, ?, ?, ?, 'spawned')`)
    .run(repo.worktree, now, now, now);
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
    VALUES ('sp-wt-owner', 'wt-owner', 'otter', 'fleetdeck-test', ?, ?, ?, ?, 'pane-dead')`)
    .run(repo.root, repo.root, repo.worktree, now);

  // The LIVE shell the human spawned into that worktree: cwd-only
  // (worktree_path NULL), status 'live', session not ended — exactly the row
  // POST /api/spawn with kind:'shell' writes.
  db.prepare(`INSERT INTO sessions
    (session_id, callsign, cwd, branch, col, note, events, started_at, last_seen, ended_at, source)
    VALUES ('shell-in-wt', 'fox', ?, NULL, 'idle', 'shell', 0, ?, ?, NULL, 'shell')`)
    .run(repo.worktree, now, now);
  db.prepare(`INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status, kind)
    VALUES ('sp-shell-in-wt', 'shell-in-wt', 'fox', 'fleetdeck-test', 'fd-test-fox', ?, NULL, ?, 'live', 'shell')`)
    .run(repo.worktree, now + 1);

  const { q } = createStatements(db);
  const { removeWorktree } = createWorktrees({ q, tick() {}, onMutate() {} });

  const res = await removeWorktree({ path: repo.worktree, force: true });
  assert.equal(res.status, 409, `a live shell in the tree must block removal (got ${JSON.stringify(res.body)})`);
  assert.deepEqual(res.body, { ok: false, reason: 'session is still alive' });
  assert.equal(existsSync(repo.worktree), true, 'the occupied worktree survives');

  // Containment: the shell's cwd is a SUBdirectory of the target tree — it
  // still loses its ground when the tree is removed, so it must still block.
  db.prepare('UPDATE spawns SET cwd = ? WHERE spawn_id = ?')
    .run(path.join(repo.worktree, 'sub', 'dir'), 'sp-shell-in-wt');
  const subRes = await removeWorktree({ path: repo.worktree, force: true });
  assert.equal(subRes.status, 409, 'a live shell in a subdirectory of the tree must also block removal');
  assert.deepEqual(subRes.body, { ok: false, reason: 'session is still alive' });

  // A lookalike PREFIX path ('<tree>-evil' shares the string prefix but is not
  // inside the tree) must NOT count as a claim — and a dead cwd-only row must
  // not count either. With those ruled out, removal proceeds.
  db.prepare('UPDATE spawns SET cwd = ?, status = ? WHERE spawn_id = ?')
    .run(`${repo.worktree}-evil`, 'pane-dead', 'sp-shell-in-wt');
  const goneRes = await removeWorktree({ path: repo.worktree, force: true });
  assert.equal(goneRes.status, 200, `no live claim left — removal proceeds (got ${JSON.stringify(goneRes.body)})`);
  assert.equal(existsSync(repo.worktree), false);
=======
// BUG-060, the post-final-check window, pinned end-to-end. The pre-remove
// liveness recheck passes, then the removal awaits its destructive git ops —
// and a /api/spawn/:id/revive of the offline spawn lands in THAT window.
// Without the canonical worktree-path claim the revive validates the
// still-existing directory, inserts its provisional owner, and launches while
// git deletes that checkout: a success reported into a pane whose cwd has
// already disappeared. With the claim, the revive queues behind the removal
// and then correctly refuses (410 — the cwd is gone), never launching a pane.
//
// The shim sits IN FRONT of the real git binary: the removal's
// `worktree remove`/`prune` sleep ~3s under a marker file so the test can
// catch the removal mid-destruction and fire the revive at the exact
// vulnerable instant; every other git call (inspect probes, the test's own
// bookkeeping) passes straight through. With the fix the revive's lock
// acquisition queues behind the removal, so by the time it proceeds the cwd
// is gone and it 410s; without it the revive launches into the
// about-to-be-removed tree (the fixture records a pane) — the failure this
// test pins.
test('a revive landing during the awaited removal git ops is held to 410, never launched into a removed checkout', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-remove-revive-lock' });
  const daemonHome = mkdtempSync(path.join(tmpdir(), 'fd-bug060-daemon-'));
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-bug060-user-'));
  const record = path.join(userHome, 'spawn.jsonl');
  const shimDir = mkdtempSync(path.join(tmpdir(), 'fd-bug060-shim-'));
  const realGit = execFileSync('which', ['git'], { encoding: 'utf8' }).trim();
  t.after(async () => {
    rmSync(daemonHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(shimDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    repo.cleanup();
  });
  writeFileSync(path.join(shimDir, 'git'), `#!/bin/sh
# The removal's destructive ops run ~3s under a marker file so the test can
# reliably catch the removal mid-destruction; every other git call (the
# daemon's inspect probes included) passes straight through.
case "$*" in
  *"worktree remove"*|*"worktree prune"*)
    touch "$FLEETDECK_TEST_REMOVING_FILE"
    sleep 3
    "\${FLEETDECK_TEST_REAL_GIT}" "$@"
    rc=$?
    rm -f "$FLEETDECK_TEST_REMOVING_FILE"
    exit $rc
    ;;
esac
exec "\${FLEETDECK_TEST_REAL_GIT}" "$@"
`);
  chmodSync(path.join(shimDir, 'git'), 0o755);

  const daemon = await startDaemon({
    home: daemonHome,
    env: {
      HOME: userHome,
      FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE,
      FLEETDECK_TEST_SPAWN_RECORD: record,
      PATH: `${shimDir}:${process.env.PATH}`,
      FLEETDECK_TEST_REAL_GIT: realGit,
      FLEETDECK_TEST_REMOVING_FILE: path.join(shimDir, 'removing'),
    },
  });
  t.after(async () => { await daemon.stop({ keepHome: true }); });

  // The daemon's FIRST agents-poll tick (1s after listen) runs the owned-pane
  // liveness sweep, and its tmux probe (~5-10s against a slow/absent test
  // server) single-flights the whole tick chain — starving request handling
  // of prompt subprocess attention for that long once. Trigger it here and
  // wait it out with a warm request, so the race below is decided by the lock
  // and not by boot noise.
  await postJson(`${daemon.baseUrl}/api/worktrees/remove`, { path: '/never-a-fleet-path' }, { timeout: 25000 });
  await getJson(`${daemon.baseUrl}/api/worktrees`, { timeout: 25000 });

  // The board-owned spawn lineage for this worktree, ended and revivable.
  ownWorktree(daemonHome, repo, { sessionId: 'bug060-race' });
  const transcript = claudeTranscriptPath(repo.worktree, 'bug060-race', userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{"type":"summary"}\n');

  const removal = postJson(`${daemon.baseUrl}/api/worktrees/remove`, { path: repo.worktree, force: true }, { timeout: 25000 });
  // Wait until removal is inside its ~3s destructive git op, THEN revive.
  const deadline = Date.now() + 20000;
  while (!existsSync(path.join(shimDir, 'removing'))) {
    if (Date.now() > deadline) throw new Error('removal never reached git worktree remove');
    await new Promise(resolve => setTimeout(resolve, 25));
  }
  // The removal is now mid-destruction. The revive races it: WITH the path
  // claim it queues until the removal has deleted the checkout and purged,
  // then 410s on the missing cwd; WITHOUT it the revive validates the
  // still-standing tree and launches (the fixture would record a pane).
  const revived = await postJson(`${daemon.baseUrl}/api/spawn/spawn-bug060-race/revive`, {}, { timeout: 25000 });

  const removed = await removal;
  assert.equal(removed.status, 200, `removal still completes (got ${JSON.stringify(removed.json)})`);
  assert.equal(revived.status, 410, `the revive was held behind the removal and then refused (got ${revived.status}: ${JSON.stringify(revived.json)})`);
  assert.match(revived.json.reason, /cwd no longer exists/);
  assert.equal(existsSync(repo.worktree), false, 'the checkout was removed');
  assert.ok(!existsSync(record), 'no pane was ever launched into the removed checkout');
  assert.equal(removed.json.rows_purged >= 1, true, 'the removal purged its dead lineage');
  withDb(daemonHome, db => {
    // The dead lineage is gone, and the refused revive left no provisional
    // row behind: exactly zero spawns remain for the session, none of them
    // in a launching state.
    const rows = db.prepare("SELECT status FROM spawns WHERE session_id = 'bug060-race'").all();
    assert.deepEqual(rows, [], 'the refused revive left no spawn row behind');
  });
>>>>>>> /tmp/mf-theirs
});

test('POST remove with delete_branch deletes the worktree branch', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-branch-remove' });
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); repo.cleanup(); });
  ownWorktree(daemon.home, repo, { sessionId: 'branch-delete' });

  const response = await postJson(`${daemon.baseUrl}/api/worktrees/remove`, {
    path: repo.worktree,
    delete_branch: true,
  });
  assert.equal(response.status, 200);
  assert.equal(response.json.branch_deleted, true);
  assert.equal(existsSync(repo.worktree), false);
  assert.throws(
    () => git(['show-ref', '--verify', '--quiet', 'refs/heads/wt-branch'], repo.root),
    /Command failed/,
  );
});

// THE false alarm, pinned. A local `main` is a cache, and a stale one lies.
// Live incident: a worktree whose work was ALREADY merged on origin read as
// "9 commits that exist nowhere else", because the local main was ten commits
// behind. That is the exact reading that talks a human into force-deleting.
test('work already on the remote is SAFE even when the local base branch is stale', async (t) => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fd-wt-stale-'));
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  const origin = path.join(dir, 'origin.git');
  const repo = path.join(dir, 'repo');
  const wt = path.join(dir, 'repo--fd-agent');
  execFileSync('git', ['init', '--bare', '-b', 'main', origin]);
  execFileSync('git', ['clone', origin, repo]);
  const g = args => execFileSync('git', ['-C', repo, ...args], { stdio: 'ignore' });
  g(['config', 'user.email', 't@t']); g(['config', 'user.name', 't']);
  writeFileSync(path.join(repo, 'a.txt'), 'one\n');
  g(['add', '-A']); g(['commit', '-m', 'base']); g(['push', 'origin', 'main']);

  // an agent's worktree does work, and it lands on origin/main upstream…
  g(['worktree', 'add', '-b', 'fd/agent', wt]);
  writeFileSync(path.join(wt, 'b.txt'), 'agent work\n');
  execFileSync('git', ['-C', wt, 'add', '-A'], { stdio: 'ignore' });
  execFileSync('git', ['-C', wt, '-c', 'user.email=t@t', '-c', 'user.name=t', 'commit', '-m', 'agent work'], { stdio: 'ignore' });
  execFileSync('git', ['-C', wt, 'push', 'origin', 'HEAD:main'], { stdio: 'ignore' });
  execFileSync('git', ['-C', wt, 'fetch', 'origin'], { stdio: 'ignore' });

  // …while the LOCAL main never moved. This is the trap.
  const localMain = execFileSync('git', ['-C', repo, 'rev-parse', 'main'], { encoding: 'utf8' }).trim();
  const remoteMain = execFileSync('git', ['-C', repo, 'rev-parse', 'origin/main'], { encoding: 'utf8' }).trim();
  assert.notEqual(localMain, remoteMain, 'sanity: the local base really is stale');

  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); });
  const db = openDb(daemon.dbPath ?? path.join(daemon.home, 'fleetd.db'));
  t.after(() => db.close());
  db.prepare(`INSERT INTO sessions (session_id, callsign, cwd, col, started_at, last_seen, ended_at, source)
    VALUES ('s-stale', 'agent-1', ?, 'offline', 1, 1, 2, 'spawned')`).run(repo);
  db.prepare(`INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
    VALUES ('sp-stale', 's-stale', 'agent-1', 'fleetdeck-1', 'fd1-agent-1', ?, ?, 1, 'gone')`).run(repo, wt);

  const res = await getJson(`${daemon.baseUrl}/api/worktrees`);
  const item = res.json.worktrees.find(w => w.path === wt);
  assert.ok(item, 'the worktree is listed');
  assert.equal(item.unpushed, 0, 'commits that exist on a remote are NOT "nowhere else"');
  assert.equal(item.verdict, 'safe', 'a stale local base must not manufacture a has-work verdict');
  assert.equal(item.base, 'origin/main', 'the base measured against must be the remote one');
});

// THE stale-cache trap, pinned. A commit that was pushed AND fetched lives on
// the local refs/remotes/* — but if the remote branch is force-reset elsewhere
// and nobody fetches again, `rev-list HEAD --not --remotes` still certifies the
// commit as safely copied, and a 'safe' removal deletes the last branch that
// references it. The inspector must fetch+prune the remote before its verdict,
// and must treat an unreachable remote as UNKNOWN, never as safe.
test('a stale remote-tracking ref cannot certify a force-reset-away commit as safe', async (t) => {
  const remote = makeRemoteRepo({ repoName: 'fleetdeck-vanish' });
  t.after(() => remote.cleanup());
  const repo = remote.clone();
  const wt = path.join(remote.base, 'clone-1-wt');
  execFileSync('git', ['-C', repo, 'worktree', 'add', '-b', 'fd/vanish', wt], { stdio: 'ignore' });

  // the agent commits work in its worktree and pushes it…
  writeFileSync(path.join(wt, 'work.txt'), 'the only copy\n');
  execFileSync('git', ['-C', wt, 'add', '-A'], { stdio: 'ignore' });
  execFileSync('git', ['-C', wt, '-c', 'user.email=t@t', '-c', 'user.name=t', 'commit', '-m', 'agent work'], { stdio: 'ignore' });
  execFileSync('git', ['-C', wt, 'push', 'origin', 'fd/vanish'], { stdio: 'ignore' });
  // …and the clone fetches, caching the pushed commit on refs/remotes/*
  execFileSync('git', ['-C', repo, 'fetch', 'origin'], { stdio: 'ignore' });

  // elsewhere, the branch is force-reset away — but the local cache never
  // hears about it. (Delete the ref DIRECTLY in the bare origin: a
  // `push --delete` from this clone would also prune its own remote-tracking
  // ref and destroy the stale-cache setup this test exists to create.)
  execFileSync('git', ['-C', remote.origin, 'update-ref', '-d', 'refs/heads/fd/vanish'], { stdio: 'ignore' });
  const stillCached = execFileSync('git', ['-C', wt, 'rev-list', '--count', 'HEAD', '--not', '--remotes'], {
    encoding: 'utf8',
  }).trim();
  assert.equal(stillCached, '0', 'sanity: the stale cache alone would still certify the commit as safe');

  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); });
  const db = openDb(path.join(daemon.home, 'fleetd.db'));
  t.after(() => db.close());
  db.prepare(`INSERT INTO sessions (session_id, callsign, cwd, col, started_at, last_seen, ended_at, source)
    VALUES ('s-vanish', 'vanish-1', ?, 'offline', 1, 1, 2, 'spawned')`).run(repo);
  db.prepare(`INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
    VALUES ('sp-vanish', 's-vanish', 'vanish-1', 'fleetdeck-1', 'fd1-vanish-1', ?, ?, 1, 'gone')`).run(repo, wt);

  const res = await getJson(`${daemon.baseUrl}/api/worktrees`);
  const item = res.json.worktrees.find(w => w.path === wt);
  assert.ok(item, 'the worktree is listed');
  assert.equal(item.unpushed, 1, 'after a fresh fetch+prune the vanished remote copy no longer counts');
  assert.equal(item.verdict, 'has-work', 'the last branch holding the commit must never read as safe');

  // And removal must refuse without force — the refresh just proved the only
  // remaining copy is this worktree's own branch.
  const rm = await postJson(`${daemon.baseUrl}/api/worktrees/remove`, { path: wt });
  assert.equal(rm.status, 409);
  assert.equal(rm.json.verdict, 'has-work');
  assert.equal(existsSync(path.join(wt, 'work.txt')), true, 'the only copy of the work survives');
});

// The flip side of the same contract: when the remote cannot be refreshed at
// all, the inspector knows nothing about what still exists there — and
// "nothing proven lost" must never be sold as "safe".
test('an unreachable remote keeps the verdict at unknown instead of certifying safe', async (t) => {
  const remote = makeRemoteRepo({ repoName: 'fleetdeck-offline' });
  const repo = remote.clone();
  const wt = path.join(remote.base, 'clone-1-wt');
  execFileSync('git', ['-C', repo, 'worktree', 'add', '-b', 'fd/offline', wt], { stdio: 'ignore' });
  t.after(() => remote.cleanup()); // removes the whole base, wt included
  // the origin the cached refs describe is GONE — every refresh attempt fails
  rmSync(remote.origin, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });

  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); });
  const db = openDb(path.join(daemon.home, 'fleetd.db'));
  t.after(() => db.close());
  db.prepare(`INSERT INTO sessions (session_id, callsign, cwd, col, started_at, last_seen, ended_at, source)
    VALUES ('s-offline', 'off-1', ?, 'offline', 1, 1, 2, 'spawned')`).run(repo);
  db.prepare(`INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
    VALUES ('sp-offline', 's-offline', 'off-1', 'fleetdeck-1', 'fd1-off-1', ?, ?, 1, 'gone')`).run(repo, wt);

  const res = await getJson(`${daemon.baseUrl}/api/worktrees`);
  const item = res.json.worktrees.find(w => w.path === wt);
  assert.ok(item, 'the worktree is listed');
  assert.equal(item.verdict, 'unknown', 'an unrefreshable remote is ignorance, not safety');
  assert.match(item.note ?? '', /could not refresh the remote/);

  const rm = await postJson(`${daemon.baseUrl}/api/worktrees/remove`, { path: wt });
  assert.equal(rm.status, 409, 'removal without force must refuse an unknown verdict');
  assert.equal(rm.json.verdict, 'unknown');
  assert.equal(existsSync(wt), true, 'nothing is destroyed on unproven knowledge');
});

// A worktree is a working directory: build tooling leaves read-only files in
// it, and `git worktree remove` then dies with one opaque "Permission denied".
// What the daemon owns, it fixes and retries. (The other half of this — paths
// owned by ROOT, which a container run inside the worktree leaves behind — is
// reported with the blocking paths and their owner instead, and cannot be
// exercised here without root. Fleet Deck never escalates.)
test('a read-only directory the daemon owns is made writable and the removal retries', async (t) => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fd-wt-ro-'));
  t.after(() => {
    try { chmodSync(path.join(dir, 'repo--fd-ro', 'locked'), 0o700); } catch { /* already gone */ }
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const repo = path.join(dir, 'repo');
  const wt = path.join(dir, 'repo--fd-ro');
  execFileSync('git', ['init', '-b', 'main', repo], { stdio: 'ignore' });
  const g = args => execFileSync('git', ['-C', repo, ...args], { stdio: 'ignore' });
  g(['config', 'user.email', 't@t']); g(['config', 'user.name', 't']);
  writeFileSync(path.join(repo, 'a.txt'), 'one\n');
  g(['add', '-A']); g(['commit', '-m', 'base']);
  g(['worktree', 'add', '-b', 'fd/ro', wt]);

  // an untracked, read-only directory — exactly what a build leaves behind
  mkdirSync(path.join(wt, 'locked'));
  writeFileSync(path.join(wt, 'locked', 'artifact.bin'), 'x');
  chmodSync(path.join(wt, 'locked'), 0o500); // no write on the parent → unlink EACCES

  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); });
  const db = openDb(path.join(daemon.home, 'fleetd.db'));
  t.after(() => db.close());
  db.prepare(`INSERT INTO sessions (session_id, callsign, cwd, col, started_at, last_seen, ended_at, source)
    VALUES ('s-ro', 'ro-1', ?, 'offline', 1, 1, 2, 'spawned')`).run(repo);
  db.prepare(`INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
    VALUES ('sp-ro', 's-ro', 'ro-1', 'fleetdeck-1', 'fd1-ro-1', ?, ?, 1, 'gone')`).run(repo, wt);

  const res = await postJson(`${daemon.baseUrl}/api/worktrees/remove`, { path: wt, force: true });
  assert.equal(res.status, 200, `removal should recover from a read-only dir it owns (got: ${JSON.stringify(res.json)})`);
  assert.equal(existsSync(wt), false, 'the worktree is gone from disk');
});

// BUG-059, the cross-repository data-loss path, pinned end to end. The
// remembered spawn cwd (repository A) is deleted and RECREATED as a different
// repository B while A's fleet worktree stays registered. Unverified, removal
// resolves B from the stale cwd, manually deletes A's worktree after B's
// `git worktree remove` refuses it, and then runs `git branch -D wt-branch`
// IN B — erasing an unrelated repository's branch and its unique commit.
// The fix must prove repo ownership (canonical common-dir / the repo's own
// worktree registry) before ANY destructive step: refuse 409, touch neither
// A's worktree nor B's branch, and keep the DB rows.
test('POST remove refuses when the recorded cwd has been replaced by a different repository', async (t) => {
  const repoA = makeRepoWithWorktree({ repoName: 'fleetdeck-replaced-repo' });
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); repoA.cleanup(); });
  ownWorktree(daemon.home, repoA, { sessionId: 'replaced-repo' });

  // Unique work on the worktree branch — the commits the wrong `branch -D`
  // would erase had they ever lived in B.
  writeFileSync(path.join(repoA.worktree, 'unique.txt'), 'exists nowhere else\n');
  git(['add', 'unique.txt'], repoA.worktree);
  git(['commit', '-q', '-m', 'unique work'], repoA.worktree);

  // Replace the recorded cwd: A's main checkout dies, and the SAME path comes
  // back as an unrelated repository B that happens to carry a branch named
  // exactly 'wt-branch' — pointing at its own unique commit.
  const branchCommitBefore = git(['rev-parse', 'wt-branch'], repoA.root);
  rmSync(repoA.root, { recursive: true, force: true });
  mkdirSync(repoA.root, { recursive: true });
  git(['init', '-q'], repoA.root);
  git(['config', 'user.email', 'test@fleetdeck.local'], repoA.root);
  git(['config', 'user.name', 'Fleet Deck Tests'], repoA.root);
  writeFileSync(path.join(repoA.root, 'b-only.txt'), 'B precious\n');
  git(['add', '.'], repoA.root);
  git(['commit', '-q', '-m', 'B unique commit'], repoA.root);
  git(['branch', 'wt-branch'], repoA.root);
  const bCommit = git(['rev-parse', 'wt-branch'], repoA.root);
  assert.notEqual(bCommit, branchCommitBefore, 'sanity: B really is a different repository');

  const response = await postJson(`${daemon.baseUrl}/api/worktrees/remove`, {
    path: repoA.worktree,
    force: true,
    delete_branch: true,
  });
  assert.equal(response.status, 409, `removal must refuse a replaced repository (got: ${JSON.stringify(response.json)})`);
  assert.equal(response.json.ok, false);
  assert.equal(existsSync(repoA.worktree), true, "A's worktree must not be removed via B");
  assert.equal(git(['rev-parse', 'wt-branch'], repoA.root), bCommit, "B's branch and its unique commit survive");
  withDb(daemon.home, db => {
    assert.ok(db.prepare('SELECT 1 FROM spawns WHERE worktree_path = ?').get(repoA.worktree),
      'the refused removal keeps its spawn rows for a later, provable cleanup');
  });
});

// Same trap, worktree already gone from disk: with no directory to read a
// common-dir from, the repo's OWN worktree registry is the only witness that
// the fleet worktree ever belonged to it. A replaced repository's registry
// never names the path, so removal must refuse before `branch -D` and keep
// the DB rows.
test('POST remove refuses a replaced repository even when the worktree is already gone from disk', async (t) => {
  const repoA = makeRepoWithWorktree({ repoName: 'fleetdeck-replaced-gone' });
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); repoA.cleanup(); });
  ownWorktree(daemon.home, repoA, { sessionId: 'replaced-gone' });

  // Drop the worktree from disk WITHOUT git's knowledge (a crashed cleanup),
  // then replace A's main checkout with repository B at the same path.
  rmSync(repoA.worktree, { recursive: true, force: true });
  rmSync(repoA.root, { recursive: true, force: true });
  mkdirSync(repoA.root, { recursive: true });
  git(['init', '-q'], repoA.root);
  git(['config', 'user.email', 'test@fleetdeck.local'], repoA.root);
  git(['config', 'user.name', 'Fleet Deck Tests'], repoA.root);
  writeFileSync(path.join(repoA.root, 'b-only.txt'), 'B precious\n');
  git(['add', '.'], repoA.root);
  git(['commit', '-q', '-m', 'B unique commit'], repoA.root);
  git(['branch', 'wt-branch'], repoA.root);
  const bCommit = git(['rev-parse', 'wt-branch'], repoA.root);

  const response = await postJson(`${daemon.baseUrl}/api/worktrees/remove`, {
    path: repoA.worktree,
    force: true,
    delete_branch: true,
  });
  assert.equal(response.status, 409, `a gone worktree still may not redirect into B (got: ${JSON.stringify(response.json)})`);
  assert.equal(response.json.ok, false);
  assert.equal(git(['rev-parse', 'wt-branch'], repoA.root), bCommit, "B's branch survives even with the worktree gone");
  withDb(daemon.home, db => {
    assert.ok(db.prepare('SELECT 1 FROM spawns WHERE worktree_path = ?').get(repoA.worktree),
      'rows survive the refusal — nothing destructive ran');
  });
});
