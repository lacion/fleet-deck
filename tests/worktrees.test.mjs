import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { openDb } from '../scripts/fleetd/db.mjs';
import { startDaemon } from './helpers/daemon.mjs';
import { makeRepoWithWorktree, makePlainDir } from './helpers/gitrepo.mjs';
import { getJson, postJson } from './helpers/http.mjs';

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

  rmSync(repo.worktree, { recursive: true, force: true });
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
  t.after(() => rmSync(dir, { recursive: true, force: true }));

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
    rmSync(dir, { recursive: true, force: true });
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
