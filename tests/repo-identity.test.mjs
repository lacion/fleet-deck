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
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
<<<<<<< /tmp/mf-ours
import { makeRepoWithWorktree, makePlainDir } from './helpers/gitrepo.mjs';
<<<<<<< /tmp/mf-ours
<<<<<<< /tmp/mf-ours
import { mkdirSync, symlinkSync, writeFileSync } from 'node:fs';
=======
import { deriveRepo, branchOf } from '../scripts/fleetd/repo-identity.mjs';
>>>>>>> /tmp/mf-theirs
=======
import { makeRepoWithWorktree, makePlainDir, makeSeparateGitDirRepo } from './helpers/gitrepo.mjs';
>>>>>>> /tmp/mf-theirs
=======
import { ledgerKey } from '../scripts/fleetd/repo-identity.mjs';
>>>>>>> /tmp/mf-theirs

function findSession(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

function findConflictFor(state, relOrAbsName) {
  return (state.conflicts || []).find(c => (c.rel_path || c.file || '').includes(relOrAbsName));
}

test('events from two worktrees of one repo collapse to one repo_id; cross-worktree collision is severity=info', async (t) => {
  const daemon = await startDaemon();
  // Register each teardown the moment its resource exists: if fixture
  // construction throws, the daemon must still be stopped (BUG-207).
  t.after(() => daemon.stop());
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-worktree-test' });
  t.after(() => { repo.cleanup(); });

  const sidRoot = randomUUID();
  const sidWt = randomUUID();

  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidRoot, cwd: repo.root }), { token: daemon });
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidWt, cwd: repo.worktree }), { token: daemon });

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
  await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { token: daemon, session_id: sidRoot, cwd: repo.root }, {
    tool_input: { file_path: path.join(repo.root, 'shared.js'), old_string: 'a', new_string: 'b' },
  }), { token: daemon });
  const whisperRes = await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { token: daemon, session_id: sidWt, cwd: repo.worktree }, {
    tool_input: { file_path: path.join(repo.worktree, 'shared.js'), old_string: 'a', new_string: 'c' },
  }), { token: daemon });
  assert.ok(whisperRes.json?.hookSpecificOutput, 'editing the same rel path from another worktree should whisper');

  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const conflict = findConflictFor(state, 'shared.js');
  assert.ok(conflict, 'conflict on shared.js should be recorded');
  assert.equal(conflict.severity, 'info', 'cross-worktree collision of one repo should be severity=info');
});

test('two sessions in the same worktree colliding is severity=warning', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-sameworktree-test' });
  t.after(() => { repo.cleanup(); });

  const sidA = randomUUID();
  const sidB = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidA, cwd: repo.root }), { token: daemon });
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidB, cwd: repo.root }), { token: daemon });

  const filePath = path.join(repo.root, 'same-worktree.js');
  await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { session_id: sidA, cwd: repo.root }, {
    tool_input: { file_path: filePath, old_string: 'a', new_string: 'b' },
  }), { token: daemon });
  const res = await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { session_id: sidB, cwd: repo.root }, {
    tool_input: { file_path: filePath, old_string: 'b', new_string: 'c' },
  }), { token: daemon });
  assert.ok(res.json?.hookSpecificOutput, 'same-worktree collision should whisper');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const conflict = findConflictFor(state, 'same-worktree.js');
  assert.ok(conflict, 'conflict on same-worktree.js should be recorded');
  assert.equal(conflict.severity, 'warning', 'same-worktree collision should be severity=warning');
});

<<<<<<< /tmp/mf-ours
test('edits through a symlinked directory alias collide with edits to the real path (BUG-127)', async (t) => {
  const daemon = await startDaemon();
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-symlink-test' });
  t.after(async () => { await daemon.stop(); repo.cleanup(); });

  // `alias/` inside the repo is a symlink to the real subdirectory `real/`, so
  // alias/x.js and real/x.js are the same inode under two lexical paths.
  mkdirSync(path.join(repo.root, 'real'));
  writeFileSync(path.join(repo.root, 'real', 'x.js'), '// seed\n');
  symlinkSync(path.join(repo.root, 'real'), path.join(repo.root, 'alias'), 'dir');

  const sidA = randomUUID();
  const sidB = randomUUID();
  const callsignA = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidA, cwd: repo.root }), { token: daemon })
    .then(r => r.json?.callsign);
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidB, cwd: repo.root }), { token: daemon });

  // A edits the file at its real path; B edits the same inode via the alias.
  // Both file_path values are lexical absolute paths, exactly what a tool call
  // reports — canonicalizing them to one ledger key is the daemon's job.
  const firstTouch = await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { session_id: sidA, cwd: repo.root }, {
    tool_input: { file_path: path.join(repo.root, 'real', 'x.js'), old_string: 'a', new_string: 'b' },
  }), { token: daemon });
  assert.deepEqual(firstTouch.json, {}, 'first touch should not whisper');

  const secondTouch = await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { session_id: sidB, cwd: repo.root }, {
    tool_input: { file_path: path.join(repo.root, 'alias', 'x.js'), old_string: 'b', new_string: 'c' },
  }), { token: daemon });
  const hso = secondTouch.json?.hookSpecificOutput;
  assert.ok(hso, 'editing the same inode through a symlinked alias must still whisper');
  assert.ok(hso.additionalContext.includes(callsignA), 'whisper should name the rival by callsign');
=======
test('root files named with leading dots (`..config`, `...hidden`) still key repo-relatively, so cross-worktree edits collide', async (t) => {
  const daemon = await startDaemon();
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-dotfile-test' });
  t.after(async () => { await daemon.stop(); repo.cleanup(); });

  const sidRoot = randomUUID();
  const sidWt = randomUUID();

  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidRoot, cwd: repo.root }), { token: daemon });
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidWt, cwd: repo.worktree }), { token: daemon });

  // Root session edits ..config through the worktree fast path; worktree
  // session edits the same logical file through the directory-lookup path.
  // Both must land on (repo_id, '..config'), not two different absolute keys.
  await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { token: daemon, session_id: sidRoot, cwd: repo.root }, {
    tool_input: { file_path: path.join(repo.root, '..config'), old_string: 'a', new_string: 'b' },
  }), { token: daemon });
  const res = await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { token: daemon, session_id: sidWt, cwd: repo.worktree }, {
    tool_input: { file_path: path.join(repo.worktree, '..config'), old_string: 'a', new_string: 'c' },
  }), { token: daemon });
  assert.ok(res.json?.hookSpecificOutput, 'editing ..config from another worktree should whisper');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const conflict = findConflictFor(state, '..config');
  assert.ok(conflict, 'conflict on ..config should be recorded');
  assert.equal(conflict.severity, 'info', 'cross-worktree collision on ..config should be severity=info');
  assert.equal(conflict.rel_path, '..config', 'ledger should keep the repo-relative name, not an absolute fallback');
});

test('unit: ledgerKey keeps dot-leading root files repo-relative; real parent escapes still fall back to absolute keys', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-ledgerkey-test' });
  t.after(() => { repo.cleanup(); });

  const session = { cwd: repo.root, worktree: repo.root, repo_id: repo.gitCommonDir };

  assert.deepEqual(
    ledgerKey(path.join(repo.root, '..config'), session),
    { repo_id: repo.gitCommonDir, rel_path: '..config', worktree: repo.root },
    'a root file named ..config is inside the tree, not a parent escape',
  );
  assert.deepEqual(
    ledgerKey(path.join(repo.root, '...hidden'), session),
    { repo_id: repo.gitCommonDir, rel_path: '...hidden', worktree: repo.root },
    'a root file named ...hidden is inside the tree, not a parent escape',
  );
  assert.deepEqual(
    ledgerKey(path.join(path.dirname(repo.root), '..config'), session),
    { repo_id: '', rel_path: path.join(path.dirname(repo.root), '..config'), worktree: null },
    'a file that truly escapes upward must still fall back to an absolute key',
  );
>>>>>>> /tmp/mf-theirs
});

test('a non-git cwd falls back to repo_id = cwd', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const plain = makePlainDir();
  t.after(() => { plain.cleanup(); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd: plain.dir }), { token: daemon });

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = findSession(state, sid);
  assert.ok(card, 'session in a non-git cwd should still register');
  assert.equal(card.repo_id, plain.dir, 'non-git cwd should fall back to repo_id = cwd');
});

<<<<<<< /tmp/mf-ours
test('a repo whose path ends in a space keeps its full path (no trim corruption)', (t) => {
  // BUG-141: the git() helper used trim() on ALL output, so a POSIX checkout
  // path ending in whitespace came back corrupted -- `git rev-parse
  // --show-toplevel` emits "<path>\n" and trim() ate the trailing space too,
  // yielding a nonexistent worktree path. Only the record-terminating newline
  // may be stripped.
  const base = mkdtempSync(path.join(tmpdir(), 'fleetdeck-trailspace-'));
  const spaced = path.join(base, 'repo with trailing space ');
  t.after(() => { rmSync(base, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  mkdirSync(spaced, { recursive: true });
  execFileSync('git', ['init', '-q'], { cwd: spaced });
  execFileSync('git', ['config', 'user.email', 'test@fleetdeck.local'], { cwd: spaced });
  execFileSync('git', ['config', 'user.name', 'Fleet Deck Tests'], { cwd: spaced });
  writeFileSync(path.join(spaced, 'shared.js'), '// seed\n');
  execFileSync('git', ['add', '.'], { cwd: spaced });
  execFileSync('git', ['commit', '-q', '-m', 'seed'], { cwd: spaced });

  const expected = realpathSync(spaced);
  assert.ok(expected.endsWith(' '), 'fixture path must end in a space');

  const repo = deriveRepo(spaced);
  assert.equal(repo.is_git, true, 'a spaced checkout should still be recognized as git');
  assert.equal(repo.worktree, expected, 'worktree must keep its trailing space, not a trimmed nonexistent path');
  assert.equal(repo.main_tree, expected, 'main tree must keep its trailing space');
  assert.equal(repo.repo_id, realpathSync(path.join(spaced, '.git')), 'repo_id should be the canonicalized common git dir');
  assert.equal(branchOf(spaced), execFileSync('git', ['rev-parse', '--abbrev-ref', 'HEAD'], { cwd: spaced, encoding: 'utf8' }).trim(),
    'branch detection should still work from a spaced path');
=======
// BUG-142: with `git init --separate-git-dir`, `git worktree list --porcelain`'s
// FIRST record is the metadata directory itself (e.g. /state/repo.git), not a
// working tree. Identity derivation must not catalog that metadata dir as the
// main checkout: repo_name must be the checkout's basename and the repo
// catalog root must be the real working tree.
test('a --separate-git-dir repo catalogs the real checkout, not the metadata dir', async (t) => {
  const daemon = await startDaemon();
  const repo = makeSeparateGitDirRepo();
  t.after(async () => { await daemon.stop(); repo.cleanup(); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd: repo.checkout }), { token: daemon });

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = findSession(state, sid);
  assert.ok(card, 'session in a separate-git-dir checkout should register');
  assert.equal(card.repo_id, repo.gitDir, 'repo_id is the canonicalized --git-common-dir');
  assert.equal(card.repo_name, repo.repoName, 'repo_name must be the checkout basename, not the metadata dir');
  assert.equal(card.worktree, repo.checkout, 'worktree must be the real checkout');

  const entry = (state.repo_catalog || []).find(r => r.repo_id === repo.gitDir);
  assert.ok(entry, 'the repo should be cataloged');
  assert.equal(entry.root, repo.checkout, 'catalog root must be the real checkout, not the metadata dir');
>>>>>>> /tmp/mf-theirs
});
