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
// The local waitUntil below is UNSCALED and predates tests/helpers/wait.mjs; new
// waits use the shared, WAIT_SCALE-aware one so the macOS advisory lane (issue
// #2, WAIT_SCALE=3) gets its headroom.
import { waitUntil as waitUntilScaled } from './helpers/wait.mjs';
import { openDb } from '../scripts/fleetd/db.mjs';

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

function countEvents(home, hookEvent) {
  const db = openDb(path.join(home, 'fleetd.db'));
  try { return db.prepare('SELECT COUNT(*) AS n FROM events WHERE hook_event = ?').get(hookEvent).n; }
  finally { db.close(); }
}

// The durable half of a failure, read straight off disk. The card note and the
// SpawnFailed note are derived from the SAME reason string, so a leak assertion
// has to cover both surfaces or it proves nothing.
function eventNotes(home, hookEvent) {
  const db = openDb(path.join(home, 'fleetd.db'));
  try { return db.prepare('SELECT note FROM events WHERE hook_event = ?').all(hookEvent).map(row => row.note ?? ''); }
  finally { db.close(); }
}

// A daemon whose background clones are guaranteed to die instantly: a 1ms clone
// timeout plus a false GIT_SSH_COMMAND so an ssh origin fails on connect. The
// transport tests only assert the SYNCHRONOUS 202 echo (composed before the
// clone runs), so the clone's fate is irrelevant beyond not outliving teardown.
async function cloneKilledDaemon(t) {
  const reposDir = scratch('fleetdeck-transport-repos-');
  const recordFile = path.join(scratch(), 'specs.jsonl');
  const port = randomPort();
  const daemon = await startDaemon({
    port,
    env: {
      ...spawnEnv(recordFile, reposDir, `http://127.0.0.1:${port}`),
      FLEETDECK_CLONE_TIMEOUT_MS: '1',
      GIT_SSH_COMMAND: 'false',
    },
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(reposDir, { recursive: true, force: true });
    rmSync(path.dirname(recordFile), { recursive: true, force: true });
  });
  return { daemon, reposDir };
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
  // The HIDING half of the snapshot's fail_detail gate, which otherwise had no
  // guard at all: every other assertion pins the field PRESENT on a tombstone, so
  // dropping the status condition and emitting the column raw broke no test. A
  // healthy card must never wear a failure expander.
  assert.equal(card.spawn.fail_detail, null, 'a spawn that did not fail carries no git-failure excerpt');
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
  // D6: the tombstone carries git's OWN distilled `fatal:` line, not its
  // "Cloning into '…'" narration — the whole point of distillGitStderr. (A
  // missing local origin prints only the fatal line, so this proves the note is
  // the verdict, clamped to 200 not 80.)
  assert.match(card.note, /fatal: repository .*does not exist/i);
  assert.doesNotMatch(card.note, /Cloning into/i);
  // …and the full reason is durable in the events table (SpawnFailed), the
  // queryable audit trail alongside the full stderr in fleetd.log.
  assert.ok(countEvents(daemon.home, 'SpawnFailed') >= 1, 'a failed clone must log a durable SpawnFailed event');
  // …and the tombstone now also carries the bounded git-stderr excerpt the board
  // reveals in its per-card expander, because the distilled note above was ALL a
  // human ever saw and the remedy lives in the lines it discards. A missing local
  // origin prints only the verdict, so here the excerpt and the note say the same
  // thing — the strictly-richer case is pinned by the PATH-shim test below. What
  // this asserts is the plumbing (row → snapshot) and the two bounds that keep a
  // pathological stderr out of the DB and off every /ws frame.
  assert.equal(typeof card.spawn.fail_detail, 'string');
  assert.match(card.spawn.fail_detail, /fatal: repository .*does not exist/i);
  assert.ok(Buffer.byteLength(card.spawn.fail_detail) <= 2000, 'the excerpt is byte-bounded');
  assert.ok(card.spawn.fail_detail.split('\n').length <= 20, 'the excerpt is line-bounded');
  assert.equal(existsSync(dest), false);
  assert.equal(existsSync(`${dest}.fd-cloning-${response.json.spawn_id.slice(0, 8)}`), false);
});

// ---------------------------------------------------------------------------
// A `git` shim on PATH (the writeGitShim technique from
// tests/derive-audit-reliability.test.mjs) is the only way to pin what the board
// shows for a failed clone: real git's wording varies by version and transport,
// and the two cases that matter — a Coder-style remedy block, and a stderr that
// echoes a credentialed URL — cannot be produced on demand without a network.
// Every non-clone call still goes to real git, so the daemon behaves normally.
// ---------------------------------------------------------------------------

function realGitPath() {
  // Resolved from the UNSHIMMED PATH (call before installing the shim).
  return execFileSync('sh', ['-c', 'command -v git'], { encoding: 'utf8' }).trim();
}

function writeCloneShim(t) {
  const dir = mkdtempSync(path.join(tmpdir(), 'fd-clone-shim-'));
  writeFileSync(path.join(dir, 'git'),
    '#!/usr/bin/env bash\n'
    // argv is exactly ['clone', '--', <origin>, <temp>] — cloneRepo's, by
    // construction (no shell anywhere in the daemon), so $3 is the origin.
    + 'if [ "$1" = clone ] && [ -n "$FD_SHIM_CLONE" ]; then\n'
    + '  if [ "$FD_SHIM_CLONE" = coder ]; then\n'
    + '    {\n'
    + '      printf "Cloning into \'%s\'...\\n" "$4"\n'
    + '      printf \'Coder: this workspace authenticates to git with the key below.\\n\'\n'
    + '      printf \'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFleetdeckTestKey000 coder@workspace\\n\'\n'
    + '      printf \'Add it at https://github.com/settings/ssh/new before cloning a private repo.\\n\'\n'
    + '      printf \'git@github.com: Permission denied (publickey).\\n\'\n'
    + '      printf \'fatal: Could not read from remote repository.\\n\'\n'
    + '    } >&2\n'
    + '  else\n'
    + '    {\n'
    + '      printf "Cloning into \'%s\'...\\n" "$4"\n'
    + '      printf \'remote: HTTP Basic: Access denied for user %s\\n\' "$FD_SHIM_SECRET"\n'
    // The BARE secret rides the `fatal:` VERDICT line too, not only a `remote:`
    // line — and that placement is the whole point of this fixture. The note is
    // distillGitStderr's pick of the verdict line, so a secret that appears only
    // above it is never in the note and a "no secret in /state" assertion passes
    // VACUOUSLY for the note. It did: the note used to get the positional URL
    // scrub ONLY, so a bare, shapeless credential on this line shipped verbatim to
    // the card, the ticker, the 409 body and the durable SpawnFailed event.
    + '      printf "fatal: unable to access \'%s/\': HTTP Basic: Access denied for user %s\\n" "$3" "$FD_SHIM_SECRET"\n'
    + '    } >&2\n'
    + '  fi\n'
    + '  exit 128\n'
    + 'fi\n'
    + 'exec "$FD_REAL_GIT" "$@"\n',
    { mode: 0o755 });
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  return dir;
}

// A daemon whose clones fail with a scripted stderr. Deliberately NOT built on
// setup(): no remote repo is needed (the clone never runs real git) and the PATH
// has to carry the shim.
async function shimmedDaemon(t, shimEnv) {
  const realGit = realGitPath();
  const shimDir = writeCloneShim(t);
  const reposDir = scratch('fleetdeck-shim-repos-');
  const recordFile = path.join(scratch(), 'specs.jsonl');
  const port = randomPort();
  const daemon = await startDaemon({
    port,
    env: {
      ...spawnEnv(recordFile, reposDir, `http://127.0.0.1:${port}`),
      PATH: `${shimDir}:${process.env.PATH}`,
      FD_REAL_GIT: realGit,
      ...shimEnv,
    },
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(reposDir, { recursive: true, force: true });
    rmSync(path.dirname(recordFile), { recursive: true, force: true });
  });
  return { daemon, reposDir };
}

async function tombstonedCard(daemon, session_id) {
  return waitUntilScaled(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    const found = state.sessions.find(s => s.session_id === session_id);
    return found?.col === 'offline' ? found : null;
  }, { timeoutMs: 12_000, label: 'failed clone tombstone' });
}

test('the note stays the distilled verdict while fail_detail carries the remedy', async t => {
  // THE deliverable, reproduced: on a Coder workspace the card said only
  // "fatal: Could not read from remote repository." while the public key and the
  // URL to register it — printed by git in the same stderr, two lines above —
  // went nowhere but fleetd.log. Debugging that took hours. Both facts are now
  // on the card: the verdict as the note, the remedy behind the expander.
  const { daemon } = await shimmedDaemon(t, { FD_SHIM_CLONE: 'coder' });
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'https://github.com/textemma/private-thing.git', branch: 'main', branch_mode: 'in-place',
  });
  assert.equal(response.status, 202, response.text);
  const card = await tombstonedCard(daemon, response.json.session_id);

  // The note is UNCHANGED in shape: one distilled line, no narration.
  assert.match(card.note, /spawn failed: fatal: Could not read from remote repository\./);
  assert.doesNotMatch(card.note, /Cloning into/i);
  assert.doesNotMatch(card.note, /settings\/ssh\/new/, 'the remedy does not belong in an 80-column note');

  // The detail is what was being thrown away.
  const detail = card.spawn.fail_detail;
  assert.match(detail, /https:\/\/github\.com\/settings\/ssh\/new/, 'the URL to visit must reach the board');
  assert.match(detail, /ssh-ed25519 AAAAC3NzaC1lZDI1NTE5/, 'the key to paste there must reach it too');
  assert.match(detail, /fatal: Could not read from remote repository\./);
  assert.ok(detail.split('\n').length > card.note.split('\n').length,
    'the detail must be strictly richer than the note — that is the whole point');
  assert.ok(Buffer.byteLength(detail) <= 2000);
  assert.ok(detail.split('\n').length <= 20);
});

test('a credential in the clone URL reaches NO surface of the API', async t => {
  // The security control, end to end — and the bundle blind spot closed: the
  // unit tests import exec.mjs directly, so without a /state-level assertion the
  // redactor could be missing from the shipped fleetd.bundle.mjs and the suite
  // would stay green. The shim guarantees the token IS in git's stderr; real
  // git/curl sometimes strips it itself, which would pass vacuously.
  const secret = 'glpat-DEADBEEFdeadbeef00';
  const origin = `https://fdtest:${secret}@127.0.0.1:1/x.git`;
  const { daemon } = await shimmedDaemon(t, { FD_SHIM_CLONE: 'credurl', FD_SHIM_SECRET: secret });
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: origin, branch: 'main', branch_mode: 'in-place',
  });
  assert.equal(response.status, 202, response.text);
  const card = await tombstonedCard(daemon, response.json.session_id);

  // ONE assertion covering the note, the ticker line and fail_detail at once:
  // the token must not appear anywhere in the whole snapshot payload.
  const state = await getJson(`${daemon.baseUrl}/state`);
  assert.equal(state.text.includes(secret), false, 'the token must appear NOWHERE in /state');
  assert.equal(state.text.includes('fdtest:'), false, 'nor the userinfo it sat in');
  // Both scrub paths are exercised: positional (the echoed URL) and exact-secret
  // (the bare `HTTP Basic` credential, which matches no known credential shape).
  assert.match(card.spawn.fail_detail, /\[redacted\]@127\.0\.0\.1:1\/x\.git/);
  assert.match(card.spawn.fail_detail, /Access denied for user \[redacted\]/);
  // And the NOTE specifically, on its own, not folded into the /state scan above:
  // it is the surface with the widest reach (card, ticker, 409 body, durable
  // event) and it used to be the one with the WEAKER control — the exact-secret
  // needle was applied inside the detail only. The shim puts the bare secret on
  // the verdict line precisely so this assertion can fail.
  assert.equal(card.note.includes(secret), false, 'the note must not carry the bare secret');
  assert.match(card.note, /Access denied for user \[redacted\]/, 'the note is hardened by the SAME pass as the detail');
  // The SpawnFailed event is derived from the same reason string and is durable
  // on disk long after the card is archived, so it gets its own proof.
  const notes = eventNotes(daemon.home, 'SpawnFailed');
  assert.ok(notes.length >= 1, 'the failure is still durably logged');
  for (const note of notes) assert.equal(note.includes(secret), false, 'no token in a durable event note');
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
  }, { token: daemon });
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
  }, { token: daemon });

  const response = await postJson(`${daemon.baseUrl}/api/spawn`, { repo: remote.origin, branch: 'main', branch_mode: 'in-place' });
  assert.equal(response.status, 202, response.text); // cloned, not reused
  const dest = path.join(reposDir, remote.repoName);
  await waitUntil(() => records(recordFile).length && existsSync(dest), { label: 'clone-not-reuse' });
  const spec = records(recordFile)[0].parsed;
  assert.equal(spec.cwd, dest);          // ran in the freshly cloned tree…
  assert.notEqual(spec.cwd, decoy);      // …never in the unrelated same-named decoy
});

test('repo mode rejects an unknown repo_host value', async t => {
  const { root, daemon } = await setup(t);
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: root, branch: 'main', branch_mode: 'in-place', repo_host: 'bitbucket',
  });
  assert.equal(response.status, 400);
  assert.match(response.json.reason, /repo_host must be github or gitlab/i);
});

test('repo_host without repo is refused', async t => {
  const { daemon } = await setup(t);
  const response = await postJson(`${daemon.baseUrl}/api/spawn`, { repo_host: 'gitlab', branch: 'main' });
  assert.equal(response.status, 400);
  assert.match(response.json.reason, /repo_host requires repo/i);
});

test('repo_org validates and requires a repo', async t => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const missing = await postJson(`${daemon.baseUrl}/api/spawn`, { repo_org: 'textemma', branch: 'main' });
  assert.equal(missing.status, 400);
  assert.match(missing.json.reason, /repo_org requires repo/i);
  const bad = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'module', repo_org: 'bad org', branch: 'main', branch_mode: 'worktree',
  });
  assert.equal(bad.status, 400);
  assert.match(bad.json.reason, /default org/i);
});

test('repo_host gitlab shorthand resolves a gitlab.com clone origin (ssh by default)', async t => {
  // A gitlab shorthand with nested subgroups is a clone request; the 202 echoes
  // the composed origin synchronously, so we assert the resolved gitlab.com URL
  // without depending on the (here unreachable) background clone. With ssh now
  // the resolved default (v0.14.0 behaviour change; was https in 0.12.0) an
  // absent-transport shorthand composes the scp-style origin.
  const { daemon, reposDir } = await cloneKilledDaemon(t);

  const response = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'mygroup/mysub/myproj', branch: 'main', branch_mode: 'in-place', repo_host: 'gitlab',
  });
  assert.equal(response.status, 202, response.text);
  assert.equal(response.json.provisioning, true);
  assert.equal(response.json.clone.origin_url, 'git@gitlab.com:mygroup/mysub/myproj.git');
  assert.equal(response.json.clone.dest, path.join(reposDir, 'myproj'));
});

test('explicit repo_transport composes the echoed origin (ssh + https) on a fresh daemon', async t => {
  const { daemon, reposDir } = await cloneKilledDaemon(t);
  // distinct repo names so the two clones never collide on the provisioning claim
  const ssh = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'org/alpha', branch: 'main', branch_mode: 'in-place', repo_transport: 'ssh',
  });
  assert.equal(ssh.status, 202, ssh.text);
  assert.equal(ssh.json.clone.origin_url, 'git@github.com:org/alpha.git');
  assert.equal(ssh.json.clone.dest, path.join(reposDir, 'alpha'));

  const https = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'org/beta', branch: 'main', branch_mode: 'in-place', repo_transport: 'https',
  });
  assert.equal(https.status, 202, https.text);
  assert.equal(https.json.clone.origin_url, 'https://github.com/org/beta.git');
});

test('absent repo_transport on a fresh daemon composes an ssh origin (the new default)', async t => {
  const { daemon } = await cloneKilledDaemon(t);
  const res = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'org/repo', branch: 'main', branch_mode: 'in-place',
  });
  assert.equal(res.status, 202, res.text);
  assert.equal(res.json.clone.origin_url, 'git@github.com:org/repo.git');
});

test('persisted subgroup default org infers gitlab for a bare-name HTTPS API spawn', async t => {
  const { daemon, reposDir } = await cloneKilledDaemon(t);
  const set = await postJson(`${daemon.baseUrl}/api/settings`, {
    repo_default_org: 'group/subgroup', repo_transport: 'https',
  });
  assert.equal(set.status, 200, set.text);
  const res = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'module', branch: 'main', branch_mode: 'in-place',
  });
  assert.equal(res.status, 202, res.text);
  assert.equal(res.json.clone.origin_url, 'https://gitlab.com/group/subgroup/module.git');
  assert.equal(res.json.clone.dest, path.join(reposDir, 'module'));
});

test('absent repo_transport honors the persisted https setting', async t => {
  const { daemon } = await cloneKilledDaemon(t);
  const set = await postJson(`${daemon.baseUrl}/api/settings`, { repo_transport: 'https' });
  assert.equal(set.status, 200, set.text);
  const res = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'org/repo', branch: 'main', branch_mode: 'in-place',
  });
  assert.equal(res.status, 202, res.text);
  assert.equal(res.json.clone.origin_url, 'https://github.com/org/repo.git');
});

test('an explicit transport on a shorthand spawn persists as an override', async t => {
  const { daemon } = await cloneKilledDaemon(t);
  const before = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal(before.json.settings.repo_transport.source, 'default', 'a fresh daemon has no override');

  const spawn1 = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'org/persist', branch: 'main', branch_mode: 'in-place', repo_transport: 'https',
  });
  assert.equal(spawn1.status, 202, spawn1.text);
  const afterExplicit = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal(afterExplicit.json.settings.repo_transport.source, 'override');
  assert.equal(afterExplicit.json.settings.repo_transport.value, 'https');

  // An absent-transport shorthand spawn must NOT rewrite the remembered choice.
  const spawn2 = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'org/persist2', branch: 'main', branch_mode: 'in-place',
  });
  assert.equal(spawn2.status, 202, spawn2.text);
  const afterAbsent = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal(afterAbsent.json.settings.repo_transport.value, 'https');
  assert.equal(afterAbsent.json.settings.repo_transport.source, 'override');
});

test('an absent transport never creates a transport override', async t => {
  const { daemon } = await cloneKilledDaemon(t);
  const res = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'org/repo', branch: 'main', branch_mode: 'in-place',
  });
  assert.equal(res.status, 202, res.text);
  const got = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal(got.json.settings.repo_transport.source, 'default');
  assert.equal(got.json.settings.repo_transport.value, 'ssh');
});

test('a 409-refused explicit transport never rewrites the remembered setting', async t => {
  const reposDir = scratch('fleetdeck-claim-repos-');
  const recordFile = path.join(scratch(), 'specs.jsonl');
  const port = randomPort();
  const daemon = await startDaemon({
    port,
    env: {
      ...spawnEnv(recordFile, reposDir, `http://127.0.0.1:${port}`),
      // The first clone must HOLD its destination claim long enough for the
      // second request to collide with it: ssh "connects" straight into a 30s
      // sleep (`sh -c` swallows git's appended host/command args as positional
      // params) and the 5s clone timeout reaps it — the immediate second POST
      // lands well inside that window, and nothing outlives teardown.
      FLEETDECK_CLONE_TIMEOUT_MS: '5000',
      GIT_SSH_COMMAND: 'sh -c "exec sleep 30"',
    },
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(reposDir, { recursive: true, force: true });
    rmSync(path.dirname(recordFile), { recursive: true, force: true });
  });

  // First spawn (absent transport → ssh default) accepts and holds the claim.
  const first = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'org/heldrepo', branch: 'main', branch_mode: 'in-place',
  });
  assert.equal(first.status, 202, first.text);

  // Second spawn names the SAME destination with an explicit https — it 409s on
  // the provisioning-owner gate, and a refused request must NOT persist.
  const second = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'org/heldrepo', branch: 'main', branch_mode: 'in-place', repo_transport: 'https',
  });
  assert.equal(second.status, 409, second.text);
  assert.match(second.json.reason, /already being provisioned/i);

  const got = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal(got.json.settings.repo_transport.source, 'default',
    'a 409-refused spawn must not rewrite the remembered transport');
  assert.equal(got.json.settings.repo_transport.value, 'ssh');
});

test('repo mode rejects an unknown repo_transport value', async t => {
  const { daemon } = await cloneKilledDaemon(t);
  const res = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: 'org/repo', branch: 'main', branch_mode: 'in-place', repo_transport: 'sftp',
  });
  assert.equal(res.status, 400);
  assert.match(res.json.reason, /repo_transport must be ssh or https/i);
});

test('repo_transport without repo is refused', async t => {
  const { daemon } = await cloneKilledDaemon(t);
  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { repo_transport: 'ssh', branch: 'main' });
  assert.equal(res.status, 400);
  assert.match(res.json.reason, /repo_transport requires repo/i);
});
