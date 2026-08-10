// tests/base-branch.test.ts — the primary integration ref must be DERIVED,
// not guessed (BUG-113).
//
// baseBranch() used to fall back to only local `main`/`master` once no usable
// origin ref existed, so a no-remote repository whose default branch is named
// anything else — trunk, develop, a company convention — resolved to null.
// The worktree inspector then lost branch/dirty/ahead evidence and repo-mode
// branch materialization could 409. The fix reads the branch of the MAIN
// worktree (first entry of `git worktree list --porcelain`), which names the
// primary branch whatever it is called.

import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { baseBranch } from '../scripts/fleetd/exec.ts';

function git(args: string[], cwd: string) {
  return execFileSync('git', args, {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
}

/** A no-remote repo with one commit on `defaultBranch`. */
function makeLocalRepo(defaultBranch: string) {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-base-'));
  git(['init', '-q', '-b', defaultBranch], dir);
  git(['config', 'user.email', 'test@fleetdeck.local'], dir);
  git(['config', 'user.name', 'Fleet Deck Tests'], dir);
  writeFileSync(path.join(dir, 'seed.txt'), 'seed\n');
  git(['add', '.'], dir);
  git(['commit', '-q', '-m', 'seed'], dir);
  return {
    dir,
    cleanup() {
      rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    },
  };
}

test('no-remote repo with a custom default branch (trunk) resolves the base from the main worktree', async (t) => {
  const repo = makeLocalRepo('trunk');
  t.after(() => { repo.cleanup(); });

  const base = await baseBranch(repo.dir);
  assert.deepEqual(
    base,
    { ref: 'trunk', local: true },
    "the main worktree's branch is the primary branch, whatever it is named",
  );
});

test('agent worktree on a feature branch still resolves the trunk base', async (t) => {
  const repo = makeLocalRepo('trunk');
  t.after(() => { repo.cleanup(); });
  const wt = path.join(repo.dir, '..', `${path.basename(repo.dir)}-wt`);
  git(['worktree', 'add', '-q', '-b', 'feature', wt], repo.dir);
  t.after(() => {
    try {
      git(['worktree', 'remove', '--force', wt], repo.dir);
    } catch {
      /* ignore */
    }
  });

  const base = await baseBranch(wt);
  assert.deepEqual(
    base,
    { ref: 'trunk', local: true },
    "probing from a linked worktree must name the MAIN worktree's branch, not the probe's own",
  );
});

test('conventional local fallbacks still hold: main, then master', async (t) => {
  for (const name of ['main', 'master']) {
    const repo = makeLocalRepo(name);
    t.after(() => { repo.cleanup(); });
    assert.deepEqual(await baseBranch(repo.dir), { ref: name, local: true });
  }
});

test('a main worktree in detached HEAD falls back to conventional local branches', async (t) => {
  const repo = makeLocalRepo('trunk');
  t.after(() => { repo.cleanup(); });
  // Detach the main worktree so `git worktree list` shows no branch for it,
  // then give the repo a conventional branch to land on.
  git(['checkout', '-q', '--detach'], repo.dir);
  git(['branch', 'main', 'trunk'], repo.dir);

  const base = await baseBranch(repo.dir);
  assert.deepEqual(
    base,
    { ref: 'main', local: true },
    'detached main worktree must not strand the conventional fallback',
  );
});
