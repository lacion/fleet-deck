import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createRepos, parseRepoInput, quickBranchCheck } from '../scripts/fleetd/repos.mjs';
import { startDaemon, randomPort } from './helpers/daemon.mjs';
import { getJson, postHook, postJson } from './helpers/http.mjs';
import { makeRemoteRepo } from './helpers/gitrepo.mjs';

test('parseRepoInput accepts supported forms and rejects argv/scheme hazards', () => {
  assert.deepEqual(parseRepoInput('org/repo'), {
    kind: 'shorthand', origin_url: 'https://github.com/org/repo.git', repo_name: 'repo',
  });
  assert.deepEqual(parseRepoInput('https://example.com/org/repo.git'), {
    kind: 'url', origin_url: 'https://example.com/org/repo.git', repo_name: 'repo',
  });
  assert.deepEqual(parseRepoInput('git@example.com:org/repo.git'), {
    kind: 'url', origin_url: 'git@example.com:org/repo.git', repo_name: 'repo',
  });
  assert.equal(parseRepoInput('-oProxyCommand=sh').error != null, true);
  assert.equal(parseRepoInput('--upload-pack=evil').error != null, true);
  // a dash-leading host hidden behind userinfo must not slip through: git's --
  // protects git's argv but still hands -oProxyCommand=… to ssh as the hostname
  assert.equal(parseRepoInput('git@-oProxyCommand=reboot:x').error != null, true);
  assert.equal(parseRepoInput('ssh://git@-oProxyCommand=reboot/x').error != null, true);
  assert.match(parseRepoInput('http://example.com/repo.git').error, /http.*refused/i);
  assert.match(parseRepoInput('file:///tmp/repo.git').error, /scheme.*refused/i);
  assert.match(parseRepoInput('./repo').error, /relative/i);
});

test('parseRepoInput repo_host steers only shorthand and fails loud on a bad host', () => {
  // Default host stays github — byte-for-byte the legacy shorthand behaviour.
  assert.deepEqual(parseRepoInput('org/repo'), {
    kind: 'shorthand', origin_url: 'https://github.com/org/repo.git', repo_name: 'repo',
  });
  assert.deepEqual(parseRepoInput('org/repo', 'github'), {
    kind: 'shorthand', origin_url: 'https://github.com/org/repo.git', repo_name: 'repo',
  });

  // gitlab, two segments → gitlab.com origin.
  assert.deepEqual(parseRepoInput('org/repo', 'gitlab'), {
    kind: 'shorthand', origin_url: 'https://gitlab.com/org/repo.git', repo_name: 'repo',
  });
  // gitlab nested subgroups (3+ segments): the full path lands in the URL and
  // repo_name is the basename of the last segment.
  assert.deepEqual(parseRepoInput('group/subgroup/repo', 'gitlab'), {
    kind: 'shorthand', origin_url: 'https://gitlab.com/group/subgroup/repo.git', repo_name: 'repo',
  });
  assert.deepEqual(parseRepoInput('group/team/sub/proj.git', 'gitlab'), {
    kind: 'shorthand', origin_url: 'https://gitlab.com/group/team/sub/proj.git', repo_name: 'proj',
  });

  // github has no subgroups: a 3+ segment path gets a HELPFUL error pointing at
  // the gitlab host / a full URL — never the misleading "relative paths refused".
  const nested = parseRepoInput('group/subgroup/repo', 'github');
  assert.match(nested.error, /gitlab/i);
  assert.match(nested.error, /subgroup|group/i);
  assert.doesNotMatch(nested.error, /relative/i);

  // An unknown host is refused outright (fail-loud), naming the allowed values.
  assert.equal(parseRepoInput('org/repo', 'bitbucket').error != null, true);
  assert.match(parseRepoInput('org/repo', 'GitHub').error, /github or gitlab/i);

  // repo_host has NO effect on URL / scp / absolute-path / bare-name kinds.
  const url = 'https://example.com/org/repo.git';
  assert.deepEqual(parseRepoInput(url, 'gitlab'), parseRepoInput(url, 'github'));
  const scp = 'git@example.com:org/repo.git';
  assert.deepEqual(parseRepoInput(scp, 'gitlab'), parseRepoInput(scp, 'github'));
  assert.deepEqual(parseRepoInput('/abs/path/repo', 'gitlab'), parseRepoInput('/abs/path/repo', 'github'));
  assert.deepEqual(parseRepoInput('barename', 'gitlab'), parseRepoInput('barename', 'github'));
});

test('gitlab host keeps every argv/scheme/whitespace hazard gate', () => {
  assert.equal(parseRepoInput('-oProxyCommand=sh', 'gitlab').error != null, true);
  assert.equal(parseRepoInput('--upload-pack=evil', 'gitlab').error != null, true);
  assert.equal(parseRepoInput('git@-oProxyCommand=reboot:x', 'gitlab').error != null, true);
  assert.equal(parseRepoInput('ssh://git@-oProxyCommand=reboot/x', 'gitlab').error != null, true);
  // a dash segment hiding INSIDE a subgroup path must not ride the 3+ branch
  assert.equal(parseRepoInput('group/-osub/repo', 'gitlab').error != null, true);
  assert.equal(parseRepoInput('group/sub group/repo', 'gitlab').error != null, true);
  assert.match(parseRepoInput('http://example.com/repo.git', 'gitlab').error, /http.*refused/i);
  assert.match(parseRepoInput('file:///tmp/repo.git', 'gitlab').error, /scheme.*refused/i);
});

test('shorthand refuses a trailing .git-only segment on both hosts', () => {
  // repoNameOf strips '.git', so these would name NOTHING and the clone dest
  // would collapse onto the repos root — refused up front, on every host.
  assert.match(parseRepoInput('org/.git').error, /repository name/i);
  assert.match(parseRepoInput('org/.git', 'github').error, /repository name/i);
  assert.match(parseRepoInput('org/.git', 'gitlab').error, /repository name/i);
  assert.match(parseRepoInput('group/subgroup/.git', 'gitlab').error, /repository name/i);
});

// resolveTarget needs only these slivers of ctx: an empty catalog and no
// repos_dir override (so FLEETDECK_REPOS_DIR decides the repos root).
function fakeReposCtx() {
  return {
    q: {
      repoByName: { all: () => [] },
      getSetting: { get: () => undefined },
    },
    onMutate: () => {},
  };
}

function checkoutWithOrigin(reposDir, name, origin) {
  const dest = path.join(reposDir, name);
  execFileSync('git', ['init', '-q', dest]);
  execFileSync('git', ['-C', dest, 'remote', 'add', 'origin', origin]);
  return dest;
}

function withReposDir(t) {
  const reposDir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-origin-eq-'));
  const previous = process.env.FLEETDECK_REPOS_DIR;
  process.env.FLEETDECK_REPOS_DIR = reposDir;
  t.after(() => {
    if (previous === undefined) delete process.env.FLEETDECK_REPOS_DIR;
    else process.env.FLEETDECK_REPOS_DIR = previous;
    rmSync(reposDir, { recursive: true, force: true });
  });
  return reposDir;
}

test('resolveTarget reuses a checkout whose scp-style origin matches the gitlab shorthand', async t => {
  const reposDir = withReposDir(t);
  // The user's reported failure: cloned once over ssh, then spawned by
  // shorthand — the checkout IS the requested repo, spelled differently.
  const dest = checkoutWithOrigin(reposDir, 'repo', 'git@gitlab.com:org/repo.git');
  const { resolveTarget } = createRepos(fakeReposCtx());
  const target = await resolveTarget({ repo: 'org/repo', repo_host: 'gitlab' });
  assert.equal(target.mode, 'local');
  assert.equal(target.root, dest);
  assert.equal(target.origin_url, 'https://gitlab.com/org/repo.git');
});

test('resolveTarget reuses a checkout whose unported ssh:// origin matches the gitlab shorthand', async t => {
  const reposDir = withReposDir(t);
  const dest = checkoutWithOrigin(reposDir, 'repo', 'ssh://git@gitlab.com/org/repo.git');
  const { resolveTarget } = createRepos(fakeReposCtx());
  const target = await resolveTarget({ repo: 'org/repo', repo_host: 'gitlab' });
  assert.equal(target.mode, 'local');
  assert.equal(target.root, dest);
});

test('resolveTarget still refuses a same-named checkout with a different origin', async t => {
  const reposDir = withReposDir(t);
  checkoutWithOrigin(reposDir, 'repo', 'git@gitlab.com:other/repo.git');
  const { resolveTarget } = createRepos(fakeReposCtx());
  await assert.rejects(
    () => resolveTarget({ repo: 'org/repo', repo_host: 'gitlab' }),
    err => err.status === 409 && /exists and is not/.test(err.message),
  );
});

test('a ported ssh origin stays outside normalization (conservative fallback)', async t => {
  const reposDir = withReposDir(t);
  // ssh://host:2222 can front a DIFFERENT server than https://host — a ported
  // origin is never proven equal, so this checkout is not reused (409, as before).
  checkoutWithOrigin(reposDir, 'repo', 'ssh://git@gitlab.com:2222/org/repo.git');
  const { resolveTarget } = createRepos(fakeReposCtx());
  await assert.rejects(
    () => resolveTarget({ repo: 'org/repo', repo_host: 'gitlab' }),
    err => err.status === 409 && /exists and is not/.test(err.message),
  );
});

test('quickBranchCheck mirrors the board gates', () => {
  assert.equal(quickBranchCheck('feature/clean-name'), null);
  for (const branch of ['-bad', 'has space', 'a..b', 'a@{b', 'a.lock', '/bad', 'bad/', 'x'.repeat(201)]) {
    assert.equal(typeof quickBranchCheck(branch), 'string', branch);
  }
});

test('hook catalog writes and /state carries repo_catalog plus settings', async t => {
  const remote = makeRemoteRepo();
  const root = remote.clone('catalog-checkout');
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); remote.cleanup(); });

  await postHook(daemon.baseUrl, 'SessionStart', {
    session_id: randomUUID(), cwd: root, hook_event_name: 'SessionStart', source: 'startup',
  });
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const row = state.repo_catalog.find(repo => repo.root === root);
  assert.ok(row);
  assert.equal(row.repo_name, path.basename(root));
  assert.ok(state.settings?.repos_dir?.resolved);
  assert.ok(['override', 'env', 'default'].includes(state.settings.repos_dir.source));
});

test('POST /api/settings persists across restart and null clears the override', async t => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-settings-home-'));
  const reposDir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-repos-root-'));
  t.after(() => {
    rmSync(home, { recursive: true, force: true });
    rmSync(reposDir, { recursive: true, force: true });
  });
  const port = randomPort();
  const first = await startDaemon({ port, home });
  try {
    const set = await postJson(`${first.baseUrl}/api/settings`, { repos_dir: reposDir });
    assert.equal(set.status, 200);
    assert.deepEqual(set.json.settings.repos_dir, { value: reposDir, source: 'override', resolved: reposDir });
  } finally {
    await first.stop({ keepHome: true });
  }

  const second = await startDaemon({ port: randomPort(), home });
  try {
    const got = await getJson(`${second.baseUrl}/api/settings`);
    assert.equal(got.status, 200);
    assert.equal(got.json.settings.repos_dir.value, reposDir);
    assert.equal(got.json.settings.repos_dir.source, 'override');
    const cleared = await postJson(`${second.baseUrl}/api/settings`, { repos_dir: null });
    assert.equal(cleared.status, 200);
    assert.notEqual(cleared.json.settings.repos_dir.source, 'override');
  } finally {
    await second.stop({ keepHome: false });
  }
});

test('POST /api/settings rejects an existing file', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-settings-file-'));
  const file = path.join(dir, 'not-a-directory');
  writeFileSync(file, 'x');
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true }); });
  const response = await postJson(`${daemon.baseUrl}/api/settings`, { repos_dir: file });
  assert.equal(response.status, 400);
  assert.match(response.json.reason, /file/i);
});
