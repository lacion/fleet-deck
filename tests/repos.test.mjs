import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import os, { tmpdir } from 'node:os';
import path from 'node:path';
import { createRepos, parseRepoInput, quickBranchCheck, repoDefaultOrgChoice, repoDefaultOrgProblem } from '../scripts/fleetd/repos.mjs';
import { detectCoderWorkspaceRoot } from '../scripts/fleetd/config.mjs';
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
  assert.match(parseRepoInput('org/repo#fragment').error, /shorthand path segments/i);
  assert.match(parseRepoInput('org/repo?query').error, /shorthand path segments/i);
  assert.match(parseRepoInput('org/repo%2Fother').error, /shorthand path segments/i);
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

test('parseRepoInput composes ssh scp-style origins and stays https on the two-arg call', () => {
  // The third param defaults https so EVERY existing two-arg caller is
  // byte-stable — the daemon SETTING owns the ssh default, not this function.
  assert.equal(parseRepoInput('org/repo', 'github').origin_url, 'https://github.com/org/repo.git');
  assert.equal(parseRepoInput('org/repo', 'gitlab').origin_url, 'https://gitlab.com/org/repo.git');
  assert.equal(parseRepoInput('org/repo', 'github', 'https').origin_url, 'https://github.com/org/repo.git');

  // Explicit ssh yields the injection-safe scp form on both hosts.
  assert.deepEqual(parseRepoInput('org/repo', 'github', 'ssh'), {
    kind: 'shorthand', origin_url: 'git@github.com:org/repo.git', repo_name: 'repo',
  });
  assert.deepEqual(parseRepoInput('org/repo', 'gitlab', 'ssh'), {
    kind: 'shorthand', origin_url: 'git@gitlab.com:org/repo.git', repo_name: 'repo',
  });
  // gitlab nested subgroups keep the full path under ssh; repo_name is the last.
  assert.deepEqual(parseRepoInput('group/sub/proj', 'gitlab', 'ssh'), {
    kind: 'shorthand', origin_url: 'git@gitlab.com:group/sub/proj.git', repo_name: 'proj',
  });
  // A trailing .git is stripped before our own suffix, exactly like https.
  assert.equal(parseRepoInput('group/team/sub/proj.git', 'gitlab', 'ssh').origin_url, 'git@gitlab.com:group/team/sub/proj.git');

  // A typo'd transport fails loud (mirrors the repo_host gate), naming values.
  assert.match(parseRepoInput('org/repo', 'github', 'sftp').error, /repo_transport must be ssh or https/i);
  assert.match(parseRepoInput('org/repo', 'gitlab', 'SSH').error, /ssh or https/i);

  // repo_transport steers ONLY shorthand — URL/scp/absolute-path/bare-name
  // kinds carry their own transport and ignore it entirely.
  const url = 'https://example.com/org/repo.git';
  assert.deepEqual(parseRepoInput(url, 'github', 'ssh'), parseRepoInput(url, 'github', 'https'));
  const scp = 'git@example.com:org/repo.git';
  assert.deepEqual(parseRepoInput(scp, 'github', 'ssh'), parseRepoInput(scp, 'github', 'https'));
  assert.deepEqual(parseRepoInput('/abs/path/repo', 'github', 'ssh'), parseRepoInput('/abs/path/repo', 'github', 'https'));
  assert.deepEqual(parseRepoInput('barename', 'github', 'ssh'), parseRepoInput('barename', 'github', 'https'));
});

test('ssh transport keeps every argv/scheme/whitespace hazard gate', () => {
  assert.equal(parseRepoInput('-oProxyCommand=sh', 'github', 'ssh').error != null, true);
  assert.equal(parseRepoInput('git@-oProxyCommand=reboot:x', 'gitlab', 'ssh').error != null, true);
  assert.equal(parseRepoInput('group/-osub/repo', 'gitlab', 'ssh').error != null, true);
  assert.equal(parseRepoInput('group/sub group/repo', 'gitlab', 'ssh').error != null, true);
  // The composed ssh origin itself passes cloneRepo's argv re-gate: constant
  // host, an already-gated slug, and no leading-dash segment.
  const composed = parseRepoInput('org/repo', 'github', 'ssh').origin_url;
  assert.equal(/[\s\x00-\x1f\x7f]/.test(composed), false);
  assert.equal(composed.startsWith('-'), false);
  assert.equal(composed.split(/[/:@]/).some(segment => segment.startsWith('-')), false);
});

test('detectCoderWorkspaceRoot needs both a Coder signal and the probe directory', () => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-coder-'));
  const absent = path.join(dir, 'nope');
  const file = path.join(dir, 'a-file');
  writeFileSync(file, 'x');
  try {
    // Any single signal (non-empty) plus an existing probe dir → the dir.
    for (const key of ['CODER', 'CODER_WORKSPACE_NAME', 'CODER_AGENT_URL']) {
      assert.equal(detectCoderWorkspaceRoot({ env: { [key]: '1' }, probeDir: dir }), dir, key);
    }
    // Signal but no probe dir → null (a /workspace-less box).
    assert.equal(detectCoderWorkspaceRoot({ env: { CODER: '1' }, probeDir: absent }), null);
    // Probe dir exists but no signal → null (not a Coder box).
    assert.equal(detectCoderWorkspaceRoot({ env: {}, probeDir: dir }), null);
    // Empty-string signals are NOT a signal.
    assert.equal(detectCoderWorkspaceRoot({ env: { CODER: '', CODER_WORKSPACE_NAME: '' }, probeDir: dir }), null);
    // A probe path that is a FILE, not a directory → null.
    assert.equal(detectCoderWorkspaceRoot({ env: { CODER: '1' }, probeDir: file }), null);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('resolveReposDir default is ~/projects off Coder (detection needs both signal and /workspace)', () => {
  const saved = {};
  for (const k of ['FLEETDECK_REPOS_DIR', 'CODER', 'CODER_WORKSPACE_NAME', 'CODER_AGENT_URL']) {
    saved[k] = process.env[k];
    delete process.env[k];
  }
  try {
    const off = createRepos(fakeReposCtx()).resolveReposDir();
    assert.equal(off.source, 'default');
    assert.equal(off.value, path.join(os.homedir(), 'projects'));
    // A Coder SIGNAL alone, with the default /workspace absent on this box,
    // still falls to ~/projects — detection requires the probe dir too. (On a
    // genuine Coder box /workspace exists and this would be /workspace.)
    process.env.CODER = '1';
    const withSignal = createRepos(fakeReposCtx()).resolveReposDir();
    if (withSignal.value !== '/workspace') {
      assert.equal(withSignal.value, path.join(os.homedir(), 'projects'));
    }
  } finally {
    for (const [k, v] of Object.entries(saved)) {
      if (v === undefined) delete process.env[k]; else process.env[k] = v;
    }
  }
});

// resolveTarget needs only these slivers of ctx: an empty catalog and no
// repos_dir override (so FLEETDECK_REPOS_DIR decides the repos root).
function fakeReposCtx(settings = {}, catalog = []) {
  return {
    q: {
      repoByName: { all: name => catalog.filter(r => r.repo_name === name) },
      getSetting: { get: key => (key in settings ? { value: settings[key] } : undefined) },
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

test('default org choice precedence and Coder seed are explicit', () => {
  assert.deepEqual(repoDefaultOrgChoice({ coder: true }), { value: 'textemma', source: 'coder' });
  assert.deepEqual(repoDefaultOrgChoice({ env: 'envorg', coder: true }), { value: 'envorg', source: 'env' });
  assert.deepEqual(repoDefaultOrgChoice({ setting: 'saved', env: 'envorg', coder: true }), { value: 'saved', source: 'override' });
  assert.deepEqual(repoDefaultOrgChoice(), { value: null, source: 'default' });
  assert.equal(repoDefaultOrgProblem('owner'), null);
  assert.equal(repoDefaultOrgProblem('group/subgroup'), null);
  for (const bad of [
    '', '-owner', 'a//b', 'a/../b', 'has space', 'group/sub#fragment',
    'group/sub?query', 'group/sub%2Fother', 'group\\sub', 'x'.repeat(201),
  ]) {
    assert.equal(typeof repoDefaultOrgProblem(bad), 'string', bad);
  }
});

test('resolveTarget promotes an unknown bare name through the default org, but a known checkout still wins', async t => {
  const reposDir = withReposDir(t);
  const { resolveTarget } = createRepos(fakeReposCtx({ repo_default_org: 'textemma', repo_transport: 'https' }));
  const clone = await resolveTarget({ repo: 'earm-module' });
  assert.equal(clone.mode, 'clone');
  assert.equal(clone.origin_url, 'https://github.com/textemma/earm-module.git');
  assert.equal(clone.dest, path.join(reposDir, 'earm-module'));
  const requestOverride = await resolveTarget({ repo: 'other-module', repo_org: 'oneoff' });
  assert.equal(requestOverride.origin_url, 'https://github.com/oneoff/other-module.git',
    'explicit repo_org makes the current spawn deterministic even before settings persistence');
  await assert.rejects(
    () => resolveTarget({ repo: 'explicit/repo', repo_org: 'ignored-would-be-confusing' }),
    err => err.status === 400 && /only to a bare repo/.test(err.message),
  );

  const localRoot = path.join(reposDir, 'known');
  mkdirSync(localRoot);
  execFileSync('git', ['init', '-q', localRoot]);
  const catalog = [{ repo_name: 'known', root: localRoot, origin_url: null }];
  const localResolver = createRepos(fakeReposCtx({ repo_default_org: 'textemma' }, catalog));
  const local = await localResolver.resolveTarget({ repo: 'known' });
  assert.equal(local.mode, 'local');
  assert.equal(local.root, localRoot, 'local catalog wins before default-org expansion');
});

test('default org infers gitlab for subgroups when the host is omitted', async t => {
  withReposDir(t);
  const { resolveTarget } = createRepos(fakeReposCtx({ repo_default_org: 'group/sub', repo_transport: 'ssh' }));
  const out = await resolveTarget({ repo: 'module' });
  assert.equal(out.origin_url, 'git@gitlab.com:group/sub/module.git');
  await assert.rejects(
    () => resolveTarget({ repo: 'other', repo_host: 'github' }),
    err => err.status === 400 && /gitlab/i.test(err.message),
    'an explicit github choice must fail loud instead of being silently replaced',
  );
});

test('resolveTarget reuses a checkout whose scp-style origin matches the gitlab shorthand', async t => {
  const reposDir = withReposDir(t);
  // The user's reported failure: cloned once over ssh, then spawned by an https
  // shorthand — the checkout IS the requested repo, spelled differently. The
  // request is explicitly https here so the composed origin is the https form
  // and the cross-spelling reuse is exactly what's under test (with ssh now the
  // resolved default, the plain shorthand would compose the SAME ssh spelling).
  const dest = checkoutWithOrigin(reposDir, 'repo', 'git@gitlab.com:org/repo.git');
  const { resolveTarget } = createRepos(fakeReposCtx());
  const target = await resolveTarget({ repo: 'org/repo', repo_host: 'gitlab', repo_transport: 'https' });
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
  }, { token: daemon });
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

test('POST /api/settings round-trips repo transport/default-org, browse_root and fav_dirs across restart', async t => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-settings2-home-'));
  const browseDir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-browse-'));
  const favA = mkdtempSync(path.join(tmpdir(), 'fleetdeck-fav-a-'));
  const favB = mkdtempSync(path.join(tmpdir(), 'fleetdeck-fav-b-'));
  t.after(() => {
    for (const d of [home, browseDir, favA, favB]) rmSync(d, { recursive: true, force: true });
  });
  const port = randomPort();
  const first = await startDaemon({ port, home });
  try {
    const set = await postJson(`${first.baseUrl}/api/settings`, {
      repo_transport: 'https', repo_default_org: 'textemma', browse_root: browseDir, fav_dirs: [favA, favB, favA],
    });
    assert.equal(set.status, 200, set.text);
    assert.equal(set.json.settings.repo_transport.value, 'https');
    assert.deepEqual(set.json.settings.repo_default_org, { value: 'textemma', source: 'override' });
    assert.equal(set.json.settings.repo_transport.source, 'override');
    assert.equal(set.json.settings.browse_root.value, browseDir);
    assert.equal(set.json.settings.browse_root.source, 'override');
    assert.equal(set.json.settings.browse_root.resolved, browseDir);
    assert.deepEqual(set.json.settings.fav_dirs, [favA, favB]); // deduped, order kept
  } finally {
    await first.stop({ keepHome: true });
  }

  const second = await startDaemon({ port: randomPort(), home });
  try {
    const got = await getJson(`${second.baseUrl}/api/settings`);
    assert.equal(got.json.settings.repo_transport.value, 'https');
    assert.deepEqual(got.json.settings.repo_default_org, { value: 'textemma', source: 'override' });
    assert.equal(got.json.settings.browse_root.value, browseDir);
    assert.deepEqual(got.json.settings.fav_dirs, [favA, favB]);
    // /state carries the SAME settings object (shared board contract), plus the
    // legacy repos_dir key and home_dir label for stale boards.
    const state = (await getJson(`${second.baseUrl}/state`)).json;
    assert.equal(state.settings.repo_transport.value, 'https');
    assert.equal(state.settings.repo_default_org.value, 'textemma');
    assert.equal(state.settings.browse_root.resolved, browseDir);
    assert.deepEqual(state.settings.fav_dirs, [favA, favB]);
    assert.ok(state.settings.repos_dir?.resolved);
    // Stale-board compat: home_dir means "the absolute root /api/fs serves".
    // An old board composes its explorer paths against it, so with a configured
    // browse_root it must be THAT root, never os.homedir().
    assert.equal(state.home_dir, browseDir);
    // null clears the transport back to the ssh default; [] clears favourites.
    const cleared = await postJson(`${second.baseUrl}/api/settings`, { repo_transport: null, repo_default_org: null, fav_dirs: [] });
    assert.equal(cleared.status, 200);
    assert.equal(cleared.json.settings.repo_transport.source, 'default');
    assert.notEqual(cleared.json.settings.repo_default_org.source, 'override');
    assert.equal(cleared.json.settings.repo_transport.value, 'ssh');
    assert.deepEqual(cleared.json.settings.fav_dirs, []);
  } finally {
    await second.stop({ keepHome: false });
  }
});

test('POST /api/settings validates values, caps fav_dirs, and refuses unknown keys', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-settings-bad-'));
  const file = path.join(dir, 'a-file');
  writeFileSync(file, 'x');
  const many = path.join(dir, 'many');
  mkdirSync(many);
  const twentyOne = [];
  for (let i = 0; i < 21; i += 1) { const d = path.join(many, `d${i}`); mkdirSync(d); twentyOne.push(d); }
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true }); });

  const badTransport = await postJson(`${daemon.baseUrl}/api/settings`, { repo_transport: 'sftp' });
  assert.equal(badTransport.status, 400);
  assert.match(badTransport.json.reason, /repo_transport must be ssh or https/i);

  for (const value of ['has space', '-owner', 'a//b', 'a/../b', 'group/sub#fragment', 'group/sub?query', 'group/sub%2Fother']) {
    const badOrg = await postJson(`${daemon.baseUrl}/api/settings`, { repo_default_org: value });
    assert.equal(badOrg.status, 400, value);
    assert.match(badOrg.json.reason, /default org/i);
  }

  const browseFile = await postJson(`${daemon.baseUrl}/api/settings`, { browse_root: file });
  assert.equal(browseFile.status, 400);
  assert.match(browseFile.json.reason, /browse_root.*file/i);

  const favMissing = await postJson(`${daemon.baseUrl}/api/settings`, { fav_dirs: [path.join(dir, 'nope')] });
  assert.equal(favMissing.status, 400);
  assert.match(favMissing.json.reason, /fav_dir is not an existing directory/i);

  const tooMany = await postJson(`${daemon.baseUrl}/api/settings`, { fav_dirs: twentyOne });
  assert.equal(tooMany.status, 400);
  assert.match(tooMany.json.reason, /20 directories or fewer/i);

  const unknown = await postJson(`${daemon.baseUrl}/api/settings`, { bogus: 'x' });
  assert.equal(unknown.status, 400);
  assert.match(unknown.json.reason, /unknown setting "bogus"/i);
  assert.match(unknown.json.reason, /repos_dir/);

  // Relative paths are refused BEFORE path.resolve can absolutize them against
  // the daemon's cwd — "." must never validate and persist a cwd-dependent root.
  for (const [key, body] of [
    ['browse_root', { browse_root: '.' }],
    ['repos_dir', { repos_dir: 'relative/dir' }],
    ['fav_dirs', { fav_dirs: ['.'] }],
  ]) {
    const rel = await postJson(`${daemon.baseUrl}/api/settings`, body);
    assert.equal(rel.status, 400, `${key} must reject a relative path`);
    assert.match(rel.json.reason, /absolute/i, key);
  }
});

test('a filesystem-root ALIAS is refused even when the lexical root ban passes', async t => {
  // /proc/self/root is a magic symlink to / on Linux: it survives the lexical
  // dirname(resolved)===resolved ban (its spelling is not "/") and only the
  // canonical realpath check catches it. Skip where procfs is absent (macOS).
  let alias = null;
  try { if (realpathSync('/proc/self/root') === '/') alias = '/proc/self/root'; } catch { /* no procfs */ }
  if (!alias) return t.skip('no /proc/self/root alias to / on this platform');
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); });
  const browse = await postJson(`${daemon.baseUrl}/api/settings`, { browse_root: alias });
  assert.equal(browse.status, 400);
  assert.match(browse.json.reason, /filesystem root/i);
  // …and the literal spelling stays refused by the lexical ban, as before.
  const literal = await postJson(`${daemon.baseUrl}/api/settings`, { browse_root: '/' });
  assert.equal(literal.status, 400);
  assert.match(literal.json.reason, /filesystem root/i);
});

test('POST /api/settings applies a mixed subset and never half-writes on a bad field', async t => {
  const reposDir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-mixed-repos-'));
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); rmSync(reposDir, { recursive: true, force: true }); });

  // A mixed subset — the legacy repos_dir key alongside a new one — applies both.
  const ok = await postJson(`${daemon.baseUrl}/api/settings`, { repos_dir: reposDir, repo_transport: 'https' });
  assert.equal(ok.status, 200, ok.text);
  assert.equal(ok.json.settings.repos_dir.value, reposDir);
  assert.equal(ok.json.settings.repo_transport.value, 'https');

  // validate-all-then-apply-all: a good repos_dir alongside a BAD repo_transport
  // writes NOTHING — the prior overrides must both survive untouched.
  const partial = await postJson(`${daemon.baseUrl}/api/settings`, { repos_dir: '/some/other/dir', repo_transport: 'bogus' });
  assert.equal(partial.status, 400);
  const after = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal(after.json.settings.repos_dir.value, reposDir, 'a rejected body must not have rewritten repos_dir');
  assert.equal(after.json.settings.repo_transport.value, 'https');
});
