// tests/helpers/gitrepo.ts — scratch git repos + worktrees for repo-identity tests.
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

function git(args: string[], cwd: string): string {
  return execFileSync('git', args, {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    // Live env, not Bun's startup snapshot: a test that overrides PATH at
    // runtime (e.g. PATH=/nonexistent to force git-missing) only reaches the
    // child git when env is explicit. A no-op under Node. See exec.ts.
    env: process.env,
  }).trim();
}

export interface RepoWithWorktreeOptions {
  repoName?: string;
  branch?: string;
}

export interface RepoWithWorktree {
  root: string;
  worktree: string;
  repoName: string;
  branch: string;
  gitCommonDir: string;
  cleanup(): void;
}

/**
 * Create a fresh repo under the OS tmpdir with one commit, and a second
 * worktree checked out from it.
 *
 * Options:
 *  - repoName: basename of the main worktree (default 'fleetdeck-repo-test')
 *  - branch:   branch name checked out in the second worktree. Defaults to
 *              'wt-branch' so every pre-0.6.0 caller behaves exactly as before;
 *              the 0.6.0 ticket-callsign tests pass a ticket-bearing branch
 *              (e.g. 'feature/PROJ-123-checkout') so the daemon's server-side
 *              branchOf() yields a Jira key to detect.
 *
 * Returns:
 *  - root: real path of the main worktree (repo_name = basename(root))
 *  - worktree: real path of the second worktree
 *  - repoName: basename of the main worktree
 *  - branch: the branch name checked out in the worktree
 *  - gitCommonDir: realpath of `git rev-parse --git-common-dir` from root
 *  - cleanup(): removes both worktrees and the containing tmp dir
 */
export function makeRepoWithWorktree({
  repoName = 'fleetdeck-repo-test',
  branch = 'wt-branch',
}: RepoWithWorktreeOptions = {}): RepoWithWorktree {
  const base = mkdtempSync(path.join(tmpdir(), 'fleetdeck-git-'));
  try {
    return build(base, repoName, branch);
  } catch (err) {
    // The base dir is allocated before any git call; if git is missing or a
    // later step throws, callers never receive a handle with cleanup(), so
    // the fixture must remove its own base here or every setup failure leaks
    // a fleetdeck-git-* tree (BUG-207).
    rmSync(base, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    throw err;
  }

  function build(base: string, repoName: string, branch: string): RepoWithWorktree {
    const root = path.join(base, repoName);
    mkdirSync(root, { recursive: true });

    // -b main pins the initial branch: bare `git init` inherits the host's
    // init.defaultBranch (or git's built-in default), so tests downstream of
    // this fixture would pass or fail with the platform.
    git(['init', '-q', '-b', 'main'], root);
    git(['config', 'user.email', 'test@fleetdeck.local'], root);
    git(['config', 'user.name', 'Fleet Deck Tests'], root);
    writeFileSync(path.join(root, 'shared.js'), '// seed\nmodule.exports = {};\n');
    git(['add', '.'], root);
    git(['commit', '-q', '-m', 'seed'], root);

    const worktree = path.join(base, `${repoName}-wt`);
    git(['worktree', 'add', '-q', '-b', branch, worktree], root);

    // --git-common-dir is usually printed relative (".git"); realpathSync()
    // resolves a relative path against process.cwd(), NOT against `root`, so
    // it must be joined against `root` first or this silently resolves inside
    // whatever directory the test runner itself was launched from.
    const gitCommonDirRaw = git(['rev-parse', '--git-common-dir'], root);
    const gitCommonDir = realpathSync(
      path.isAbsolute(gitCommonDirRaw) ? gitCommonDirRaw : path.resolve(root, gitCommonDirRaw),
    );

    return {
      root: realpathSync(root),
      worktree: realpathSync(worktree),
      repoName,
      branch,
      gitCommonDir,
      cleanup() {
        try {
          git(['worktree', 'remove', '--force', worktree], root);
        } catch {
          /* ignore */
        }
        rmSync(base, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
      },
    };
  }
}

export interface PlainDir {
  dir: string;
  cleanup(): void;
}

/** Create a plain (non-git) scratch directory for the "falls back to cwd" case. */
export function makePlainDir(): PlainDir {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-plain-'));
  return {
    dir: realpathSync(dir),
    cleanup() {
      rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    },
  };
}

export interface SeparateGitDirRepoOptions {
  repoName?: string;
}

export interface SeparateGitDirRepo {
  checkout: string;
  gitDir: string;
  repoName: string;
  cleanup(): void;
}

/**
 * Create a repo whose git metadata lives OUTSIDE the checkout
 * (`git init --separate-git-dir <state>/<name>.git <checkout>`). In this
 * layout `git worktree list --porcelain`'s FIRST record is the metadata
 * directory itself, not the checkout — identity derivation must not catalog
 * that metadata dir as the main tree (BUG-142).
 *
 * Returns:
 *  - checkout: real path of the working tree (the expected main tree)
 *  - gitDir: real path of the separate metadata directory
 *  - repoName: basename of the checkout
 *  - cleanup(): removes the containing tmp dir
 */
export function makeSeparateGitDirRepo({
  repoName = 'fleetdeck-sepgit-test',
}: SeparateGitDirRepoOptions = {}): SeparateGitDirRepo {
  const base = mkdtempSync(path.join(tmpdir(), 'fleetdeck-sepgit-'));
  const gitDir = path.join(base, 'state', `${repoName}.git`);
  const checkout = path.join(base, 'work', repoName);
  mkdirSync(path.dirname(gitDir), { recursive: true });
  execFileSync('git', ['init', '-q', `--separate-git-dir=${gitDir}`, checkout], {
    env: process.env, // live env (see git() above); no-op under Node
  });
  git(['config', 'user.email', 'test@fleetdeck.local'], checkout);
  git(['config', 'user.name', 'Fleet Deck Tests'], checkout);
  writeFileSync(path.join(checkout, 'shared.js'), '// seed\nmodule.exports = {};\n');
  git(['add', '.'], checkout);
  git(['commit', '-q', '-m', 'seed'], checkout);

  return {
    checkout: realpathSync(checkout),
    gitDir: realpathSync(gitDir),
    repoName,
    cleanup() {
      rmSync(base, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    },
  };
}

export interface RemoteRepoOptions {
  repoName?: string;
  branches?: string[];
}

export interface RemoteRepo {
  base: string;
  origin: string;
  repoName: string;
  seed: string;
  clone(name?: string): string;
  cleanup(): void;
}

/**
 * Create a networkless bare origin with a seeded main branch and optional
 * pushed branches. Call clone(name) for independent working copies whose
 * non-default branches exist only as origin/* refs.
 */
export function makeRemoteRepo({
  repoName = 'fleetdeck-remote-test',
  branches = [],
}: RemoteRepoOptions = {}): RemoteRepo {
  const base = mkdtempSync(path.join(tmpdir(), 'fleetdeck-remote-'));
  const origin = path.join(base, `${repoName}.git`);
  const seed = path.join(base, 'seed');
  execFileSync('git', ['init', '--bare', '-q', '-b', 'main', origin], {
    env: process.env, // live env (see git() above); no-op under Node
  });
  mkdirSync(seed, { recursive: true });
  git(['init', '-q', '-b', 'main'], seed);
  git(['config', 'user.email', 'test@fleetdeck.local'], seed);
  git(['config', 'user.name', 'Fleet Deck Tests'], seed);
  writeFileSync(path.join(seed, 'README.md'), '# seed\n');
  git(['add', '.'], seed);
  git(['commit', '-q', '-m', 'seed'], seed);
  git(['remote', 'add', 'origin', origin], seed);
  git(['push', '-q', '-u', 'origin', 'main'], seed);
  for (const branch of branches) {
    git(['switch', '-q', '-c', branch, 'main'], seed);
    const marker = branch.replaceAll('/', '-') + '.txt';
    writeFileSync(path.join(seed, marker), `${branch}\n`);
    git(['add', marker], seed);
    git(['commit', '-q', '-m', `seed ${branch}`], seed);
    git(['push', '-q', '-u', 'origin', branch], seed);
    git(['switch', '-q', 'main'], seed);
  }

  let cloneNo = 0;
  return {
    base,
    origin: realpathSync(origin),
    repoName,
    seed: realpathSync(seed),
    clone(name = `clone-${++cloneNo}`) {
      const target = path.join(base, name);
      execFileSync('git', ['clone', '-q', origin, target], {
        env: process.env, // live env (see git() above); no-op under Node
      });
      git(['config', 'user.email', 'test@fleetdeck.local'], target);
      git(['config', 'user.name', 'Fleet Deck Tests'], target);
      return realpathSync(target);
    },
    cleanup() {
      rmSync(base, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    },
  };
}
