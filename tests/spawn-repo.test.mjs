import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { startDaemon, randomPort } from './helpers/daemon.mjs';
import { getJson, postHook, postJson } from './helpers/http.mjs';
import { makeRemoteRepo } from './helpers/gitrepo.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPAWN_CMD_FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.mjs');
try { chmodSync(SPAWN_CMD_FIXTURE, 0o755); } catch { /* best effort */ }

function scratch(prefix = 'fleetdeck-spawn-repo-') {
  return mkdtempSync(path.join(tmpdir(), prefix));
}

function git(args, cwd) {
  return execFileSync('git', ['-C', cwd, ...args], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}

function records(file) {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8').split('\n').filter(Boolean).map(line => JSON.parse(line));
}

async function waitUntil(fn, { timeoutMs = 12_000, label = 'condition' } = {}) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await fn();
    if (value) return value;
    if (Date.now() >= deadline) throw new Error(`timed out waiting for ${label}`);
    await new Promise(resolve => setTimeout(resolve, 75));
  }
}

function spawnEnv(recordFile, reposDir, postUrl = null) {
  return {
    FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE,
    FLEETDECK_TEST_SPAWN_RECORD: recordFile,
    FLEETDECK_REPOS_DIR: reposDir,
    ...(postUrl ? { FLEETDECK_TEST_SPAWN_POST_URL: postUrl } : {}),
  };
}

async function setup(t, { branches = ['existing', 'remote-only'] } = {}) {
  const remote = makeRemoteRepo({ branches });
  const root = remote.clone('local');
  const reposDir = scratch('fleetdeck-managed-repos-');
  const recordFile = path.join(scratch(), 'specs.jsonl');
  const port = randomPort();
  const daemon = await startDaemon({
    port,
    env: spawnEnv(recordFile, reposDir, `http://127.0.0.1:${port}`),
  });
  t.after(async () => {
    await daemon.stop();
    remote.cleanup();
    rmSync(reposDir, { recursive: true, force: true });
    rmSync(path.dirname(recordFile), { recursive: true, force: true });
  });
  return { remote, root, reposDir, recordFile, daemon };
}

test('repo mode in-place switches an existing branch and launches in that cwd', async t => {
  const { root, recordFile, daemon } = await setup(t);
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, { repo: root, branch: 'existing', branch_mode: 'in-place' });
  assert.equal(response.status, 200, response.text);
  const spec = (await waitUntil(() => records(recordFile)[0], { label: 'spawn fixture record' })).parsed;
  assert.equal(spec.cwd, root);
  assert.equal(git(['branch', '--show-current'], root), 'existing');
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = state.sessions.find(s => s.session_id === response.json.session_id);
  assert.equal(card.branch, 'existing');
  assert.equal(card.spawn.requested_branch, 'existing');
  assert.equal(card.spawn.branch_mode, 'in-place');
});

test('dirty in-place checkout is refused with file names', async t => {
  const { root, daemon } = await setup(t);
  writeFileSync(path.join(root, 'dirty.txt'), 'dirty\n');
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, { repo: root, branch: 'existing', branch_mode: 'in-place' });
  assert.equal(response.status, 409);
  assert.match(response.json.reason, /dirty.*dirty\.txt/i);
});

test('in-place surfaces git stderr when the branch is checked out in another worktree', async t => {
  const { root, daemon } = await setup(t);
  const occupied = path.join(path.dirname(root), 'occupied');
  git(['worktree', 'add', '-q', '-b', 'occupied', occupied, 'main'], root);
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, { repo: root, branch: 'occupied', branch_mode: 'in-place' });
  assert.equal(response.status, 409);
  // git's own words, surfaced verbatim — the exact phrasing varies by git
  // version ("already used by worktree at …" / "already checked out at …")
  assert.match(response.json.reason, /already (used by worktree|checked out)/i);
});

test('worktree mode creates the named sibling and tracks a remote-only branch', async t => {
  const { root, daemon } = await setup(t);
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, { repo: root, branch: 'remote-only', branch_mode: 'worktree' });
  assert.equal(response.status, 200, response.text);
  const expected = `${root}--fd-remote-only`;
  assert.equal(existsSync(expected), true);
  assert.equal(git(['branch', '--show-current'], expected), 'remote-only');
  assert.equal(git(['rev-parse', '--abbrev-ref', '@{u}'], expected), 'origin/remote-only');
});

test('worktree mode reuses an existing checkout of the requested branch', async t => {
  const { root, recordFile, daemon } = await setup(t);
  const existing = path.join(path.dirname(root), 'reuse-existing');
  git(['worktree', 'add', '-q', '--track', '-b', 'reuse-me', existing, 'origin/main'], root);
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, { repo: root, branch: 'reuse-me', branch_mode: 'worktree' });
  assert.equal(response.status, 200, response.text);
  const spec = (await waitUntil(() => records(recordFile)[0], { label: 'reuse fixture record' })).parsed;
  assert.equal(spec.cwd, existing);
  assert.equal(existsSync(`${root}--fd-reuse-me`), false);
});

test('a branch existing nowhere is created from origin HEAD', async t => {
  const { root, daemon } = await setup(t);
  const branch = `new-${randomUUID().slice(0, 6)}`;
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, { repo: root, branch, branch_mode: 'worktree' });
  assert.equal(response.status, 200, response.text);
  const expected = `${root}--fd-${branch}`;
  assert.equal(git(['branch', '--show-current'], expected), branch);
  assert.equal(git(['rev-parse', 'HEAD'], expected), git(['rev-parse', 'origin/main'], root));
});

test('clone provisioning returns 202 then launches from the cloned requested branch', async t => {
  const { remote, reposDir, recordFile, daemon } = await setup(t);
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: remote.origin, branch: 'remote-only', branch_mode: 'in-place',
  });
  assert.equal(response.status, 202, response.text);
  assert.equal(response.json.provisioning, true);
  const dest = path.join(reposDir, remote.repoName);
  await waitUntil(() => records(recordFile).length && existsSync(dest), { label: 'clone launch' });
  assert.equal(git(['branch', '--show-current'], dest), 'remote-only');
});

test('clone failure tombstones the card and removes destination plus temp', async t => {
  const { reposDir, daemon } = await setup(t);
  const missing = path.join(scratch('fleetdeck-missing-origin-'), 'bad.git');
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, { repo: missing, branch: 'main', branch_mode: 'in-place' });
  assert.equal(response.status, 202, response.text);
  const dest = path.join(reposDir, 'bad');
  const card = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    const found = state.sessions.find(s => s.session_id === response.json.session_id);
    return found?.col === 'offline' ? found : null;
  }, { label: 'failed clone tombstone' });
  assert.match(card.note, /spawn failed/i);
  assert.equal(existsSync(dest), false);
  assert.equal(existsSync(`${dest}.fd-cloning-${response.json.spawn_id.slice(0, 8)}`), false);
});

test('concurrent clone requests for one destination are single-flight', async t => {
  const { remote, daemon } = await setup(t);
  const [a, b] = await Promise.all([
    postJson(`${daemon.baseUrl}/api/spawn`, { repo: remote.origin, branch: 'main' }),
    postJson(`${daemon.baseUrl}/api/spawn`, { repo: remote.origin, branch: 'main' }),
  ]);
  const statuses = [a.status, b.status].sort();
  assert.deepEqual(statuses, [202, 409]);
  const conflict = a.status === 409 ? a : b;
  assert.match(conflict.json.reason, /provisioned by (falcon|otter|raven|lynx|orca|wren|viper|heron|badger|comet|ember|drift)-/);
});

test('bare-name repo mode resolves through the hook-populated catalog', async t => {
  const { root, daemon } = await setup(t);
  await postHook(daemon.baseUrl, 'SessionStart', {
    session_id: randomUUID(), cwd: root, hook_event_name: 'SessionStart', source: 'startup',
  });
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: path.basename(root), branch: 'main', branch_mode: 'worktree',
  });
  assert.equal(response.status, 200, response.text);
});

test('repo mode rejects legacy worktree:true', async t => {
  const { root, daemon } = await setup(t);
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, { repo: root, branch: 'main', worktree: true });
  assert.equal(response.status, 400);
  assert.equal(response.json.reason, 'branch_mode replaces worktree in repo mode');
});

test('a same-named checkout with no matching origin is not reused when the request names an origin', async t => {
  const { remote, reposDir, recordFile, daemon } = await setup(t);
  // A decoy: a local repo with the SAME basename as the requested one but NO
  // remote, seeded into the catalog via a session hook. Reusing it would run the
  // agent in an unrelated tree — the request names a concrete origin, so the
  // daemon must clone into the repos root instead of resolving to the decoy.
  const decoy = path.join(scratch('fleetdeck-decoy-'), remote.repoName);
  execFileSync('git', ['init', '-q', decoy]);
  execFileSync('git', ['-C', decoy, 'commit', '-q', '--allow-empty', '-m', 'decoy'], {
    env: { ...process.env, GIT_AUTHOR_NAME: 'x', GIT_AUTHOR_EMAIL: 'x@x', GIT_COMMITTER_NAME: 'x', GIT_COMMITTER_EMAIL: 'x@x' },
  });
  t.after(() => rmSync(path.dirname(decoy), { recursive: true, force: true }));
  await postHook(daemon.baseUrl, 'SessionStart', {
    session_id: randomUUID(), cwd: decoy, hook_event_name: 'SessionStart', source: 'startup',
  });

  const response = await postJson(`${daemon.baseUrl}/api/spawn`, { repo: remote.origin, branch: 'main', branch_mode: 'in-place' });
  assert.equal(response.status, 202, response.text); // cloned, not reused
  const dest = path.join(reposDir, remote.repoName);
  await waitUntil(() => records(recordFile).length && existsSync(dest), { label: 'clone-not-reuse' });
  const spec = records(recordFile)[0].parsed;
  assert.equal(spec.cwd, dest);          // ran in the freshly cloned tree…
  assert.notEqual(spec.cwd, decoy);      // …never in the unrelated same-named decoy
});
