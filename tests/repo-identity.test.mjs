// tests/repo-identity.test.mjs
//
// Repo identity rule: repo_id is derived from
// `git rev-parse --git-common-dir`, which collapses all worktrees of one
// repo to a single identity (unlike --show-toplevel, which would fragment
// them). File-ledger severity is "warning" for a same-worktree collision and
// "info" for a cross-worktree collision of the same repo; a non-git cwd
// falls back to repo_id = cwd.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import { makeRepoWithWorktree, makePlainDir } from './helpers/gitrepo.mjs';

function findSession(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

function findConflictFor(state, relOrAbsName) {
  return (state.conflicts || []).find(c => (c.rel_path || c.file || '').includes(relOrAbsName));
}

test('events from two worktrees of one repo collapse to one repo_id; cross-worktree collision is severity=info', async (t) => {
  const daemon = await startDaemon();
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-worktree-test' });
  t.after(async () => { await daemon.stop(); repo.cleanup(); });

  const sidRoot = randomUUID();
  const sidWt = randomUUID();

  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidRoot, cwd: repo.root }));
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidWt, cwd: repo.worktree }));

  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const cardRoot = findSession(state, sidRoot);
  const cardWt = findSession(state, sidWt);
  assert.ok(cardRoot && cardWt, 'both sessions should register');

  assert.ok(cardRoot.repo_id, 'root session should carry a repo_id');
  assert.equal(cardRoot.repo_id, cardWt.repo_id, 'both worktrees of one repo must collapse to the same repo_id');
  assert.equal(cardRoot.repo_name, repo.repoName, 'repo_name should be the basename of the main tree');
  assert.equal(cardWt.repo_name, repo.repoName, 'repo_name is a repo-level property, same for the worktree session');
  assert.notEqual(cardRoot.branch, cardWt.branch, 'each session should still report its own worktree branch');

  // Cross-worktree collision on the same rel path -> severity "info".
  await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { session_id: sidRoot, cwd: repo.root }, {
    tool_input: { file_path: path.join(repo.root, 'shared.js'), old_string: 'a', new_string: 'b' },
  }));
  const whisperRes = await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { session_id: sidWt, cwd: repo.worktree }, {
    tool_input: { file_path: path.join(repo.worktree, 'shared.js'), old_string: 'a', new_string: 'c' },
  }));
  assert.ok(whisperRes.json?.hookSpecificOutput, 'editing the same rel path from another worktree should whisper');

  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const conflict = findConflictFor(state, 'shared.js');
  assert.ok(conflict, 'conflict on shared.js should be recorded');
  assert.equal(conflict.severity, 'info', 'cross-worktree collision of one repo should be severity=info');
});

test('two sessions in the same worktree colliding is severity=warning', async (t) => {
  const daemon = await startDaemon();
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-sameworktree-test' });
  t.after(async () => { await daemon.stop(); repo.cleanup(); });

  const sidA = randomUUID();
  const sidB = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidA, cwd: repo.root }));
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidB, cwd: repo.root }));

  const filePath = path.join(repo.root, 'same-worktree.js');
  await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { session_id: sidA, cwd: repo.root }, {
    tool_input: { file_path: filePath, old_string: 'a', new_string: 'b' },
  }));
  const res = await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { session_id: sidB, cwd: repo.root }, {
    tool_input: { file_path: filePath, old_string: 'b', new_string: 'c' },
  }));
  assert.ok(res.json?.hookSpecificOutput, 'same-worktree collision should whisper');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const conflict = findConflictFor(state, 'same-worktree.js');
  assert.ok(conflict, 'conflict on same-worktree.js should be recorded');
  assert.equal(conflict.severity, 'warning', 'same-worktree collision should be severity=warning');
});

test('a non-git cwd falls back to repo_id = cwd', async (t) => {
  const daemon = await startDaemon();
  const plain = makePlainDir();
  t.after(async () => { await daemon.stop(); plain.cleanup(); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd: plain.dir }));

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = findSession(state, sid);
  assert.ok(card, 'session in a non-git cwd should still register');
  assert.equal(card.repo_id, plain.dir, 'non-git cwd should fall back to repo_id = cwd');
});
