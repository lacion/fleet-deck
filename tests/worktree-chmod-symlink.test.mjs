// tests/worktree-chmod-symlink.test.mjs — regression for BUG-029: worktree
// cleanup's chmodWritableWhereOwned used lstat for the ownership check, then
// called chmodSync BEFORE rejecting symbolic links. chmodSync follows links,
// so a user-owned symlink inside a managed worktree could make cleanup
// weaken the mode of a sensitive target OUTSIDE Fleet Deck's custody
// (private keys, credentials). The fix skips symlinks before any chmod.
// Pure: exercises the helper directly, no daemon.

import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, mkdirSync, mkdtempSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { chmodWritableWhereOwned } from '../scripts/fleetd/helpers.mjs';

function scratch(t, prefix = 'fleetdeck-chmod-symlink-') {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  return dir;
}

const MODE_BITS_MEANINGFUL = process.platform !== 'win32';

test('symlink to external target: target mode untouched by cleanup chmod', { skip: !MODE_BITS_MEANINGFUL }, (t) => {
  const dir = scratch(t);
  const worktree = path.join(dir, 'worktree');
  const outside = path.join(dir, 'outside');
  mkdirSync(worktree);
  mkdirSync(outside);

  const secret = path.join(outside, 'id_rsa');
  writeFileSync(secret, 'not-a-real-key');
  chmodSync(secret, 0o600);
  assert.equal(statSync(secret).mode & 0o777, 0o600, 'target starts owner-only');

  // A same-uid symlink inside the managed tree pointing outside it — the
  // lstat ownership check passes, so only the symlink guard can save the target.
  symlinkSync(secret, path.join(worktree, 'innocent.txt'));

  chmodWritableWhereOwned(worktree);

  assert.equal(statSync(secret).mode & 0o777, 0o600,
    'external symlink target must keep its original mode — chmodSync must not follow the link');
});

test('real files inside the tree are still made owner-writable', { skip: !MODE_BITS_MEANINGFUL }, (t) => {
  const dir = scratch(t);
  const worktree = path.join(dir, 'worktree');
  mkdirSync(worktree);
  const ro = path.join(worktree, 'artifact');
  writeFileSync(ro, 'build output');
  chmodSync(ro, 0o400); // read-only build artifact — our mess to clear

  chmodWritableWhereOwned(worktree);

  assert.ok(statSync(ro).mode & 0o200, 'owned real file regains owner-write');
});
