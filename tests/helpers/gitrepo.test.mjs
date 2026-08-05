// tests/helpers/gitrepo.test.mjs — regression for BUG-207: if fixture
// construction fails (here: git missing from PATH), makeRepoWithWorktree must
// remove the base dir it already allocated instead of leaking a
// fleetdeck-git-* tree with no cleanup() handle ever handed out.

import test from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { makeRepoWithWorktree } from './gitrepo.mjs';

test('makeRepoWithWorktree removes its base dir when construction fails (missing git)', (t) => {
  const tmp = tmpdir();
  const before = new Set(readdirSync(tmp).filter(n => n.startsWith('fleetdeck-git-')));

  const realPath = process.env.PATH;
  process.env.PATH = '/nonexistent-no-git-here';
  t.after(() => { process.env.PATH = realPath; });

  assert.throws(() => makeRepoWithWorktree({ repoName: 'fleetdeck-leak-test' }));

  const leaked = readdirSync(tmp).filter(n => n.startsWith('fleetdeck-git-') && !before.has(n));
  assert.deepEqual(leaked, [], 'failed construction must not leak a fleetdeck-git-* tree');
});
