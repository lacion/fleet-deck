import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { parseRepoInput, quickBranchCheck } from '../scripts/fleetd/repos.mjs';
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
  assert.match(parseRepoInput('http://example.com/repo.git').error, /http.*refused/i);
  assert.match(parseRepoInput('file:///tmp/repo.git').error, /scheme.*refused/i);
  assert.match(parseRepoInput('./repo').error, /relative/i);
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
