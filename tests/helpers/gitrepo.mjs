// tests/helpers/gitrepo.mjs — scratch git repos + worktrees for repo-identity tests.
//
// Repo identity rule (F1): repo_id = `git rev-parse --git-common-dir` (canonicalized),
// which collapses worktrees; repo_name = basename of the main tree. These
// helpers build a throwaway repo (git init + a commit) and add a worktree,
// exactly as instructed by the task brief, so contract tests can exercise
// real git rather than mocking it.

import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

function git(args, cwd) {
  return execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}

/**
 * Create a fresh repo under the OS tmpdir with one commit, and a second
 * worktree checked out from it.
 *
 * Returns:
 *  - root: real path of the main worktree (repo_name = basename(root))
 *  - worktree: real path of the second worktree
 *  - repoName: basename of the main worktree
 *  - gitCommonDir: realpath of `git rev-parse --git-common-dir` from root
 *  - cleanup(): removes both worktrees and the containing tmp dir
 */
export function makeRepoWithWorktree({ repoName = 'fleetdeck-repo-test' } = {}) {
  const base = mkdtempSync(path.join(tmpdir(), 'fleetdeck-git-'));
  const root = path.join(base, repoName);
  mkdirSync(root, { recursive: true });

  git(['init', '-q'], root);
  git(['config', 'user.email', 'test@fleetdeck.local'], root);
  git(['config', 'user.name', 'Fleet Deck Tests'], root);
  writeFileSync(path.join(root, 'shared.js'), '// seed\nmodule.exports = {};\n');
  git(['add', '.'], root);
  git(['commit', '-q', '-m', 'seed'], root);

  const worktree = path.join(base, `${repoName}-wt`);
  git(['worktree', 'add', '-q', '-b', 'wt-branch', worktree], root);

  // --git-common-dir is usually printed relative (".git"); realpathSync()
  // resolves a relative path against process.cwd(), NOT against `root`, so
  // it must be joined against `root` first or this silently resolves inside
  // whatever directory the test runner itself was launched from.
  const gitCommonDirRaw = git(['rev-parse', '--git-common-dir'], root);
  const gitCommonDir = realpathSync(
    path.isAbsolute(gitCommonDirRaw) ? gitCommonDirRaw : path.resolve(root, gitCommonDirRaw)
  );

  return {
    root: realpathSync(root),
    worktree: realpathSync(worktree),
    repoName,
    gitCommonDir,
    cleanup() {
      try { git(['worktree', 'remove', '--force', worktree], root); } catch { /* ignore */ }
      rmSync(base, { recursive: true, force: true });
    },
  };
}

/** Create a plain (non-git) scratch directory for the "falls back to cwd" case. */
export function makePlainDir() {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-plain-'));
  return {
    dir: realpathSync(dir),
    cleanup() { rmSync(dir, { recursive: true, force: true }); },
  };
}
