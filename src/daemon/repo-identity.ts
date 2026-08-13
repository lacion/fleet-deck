// repo-identity.ts — derive {repo_id, repo_name, worktree, branch} from a cwd.
//
// Repo identity rule:
//   repo_id  = canonicalized `git rev-parse --git-common-dir` (collapses all
//              worktrees of a repo to one identity; NOT --show-toplevel)
//   repo_name = basename of the main tree
//   worktree  = toplevel of the cwd
//   branch    = `git rev-parse --abbrev-ref HEAD` (server-side; short TTL —
//               branches change under a session)
//   non-git dir: repo_id = cwd
// Cache per cwd: one subprocess round per unknown cwd, not per event. Both
// caches are deliberately bounded and expiring: cwd values come from external
// hook/CLI input, and a daemon can otherwise retain every directory it has ever
// seen for its whole lifetime.

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

// A git repository identity. `is_git: true` guarantees every field is a real
// path (discriminated union), so callers that gate on `.is_git` narrow the
// nullable fields away without a cast.
export interface RepoIdentityGit {
  repo_id: string;
  repo_name: string;
  worktree: string;
  main_tree: string;
  is_git: true;
}
export interface RepoIdentityNonGit {
  repo_id: string | null;
  repo_name: string | null;
  worktree: string | null;
  main_tree: string | null;
  is_git: false;
}
export type RepoIdentity = RepoIdentityGit | RepoIdentityNonGit;

// Conflict-radar key for an edited file. `repo_id` is '' (never null) for the
// outside-git fallback; `worktree` is null there and a real tree otherwise.
export interface LedgerKey {
  repo_id: string;
  rel_path: string;
  worktree: string | null;
}

// The subset of a session/card row that ledgerKey reads. A real card row is
// wider; only these three fields matter here.
export interface SessionRef {
  cwd?: string | null;
  worktree?: string | null;
  repo_id?: string | null;
}

interface CacheEntry<V> {
  value: V;
  expiresAt: number;
}

const identityCache = new Map<string, CacheEntry<RepoIdentity>>(); // cwd -> {value, expiresAt}; insertion order is LRU order
const branchCache = new Map<string, CacheEntry<string | null>>(); // cwd -> {value, expiresAt}; insertion order is LRU order
const CACHE_MAX = 512;
const IDENTITY_TTL_MS = 5 * 60_000;
// A directory can become a repository in place (`git init`). Keeping that
// negative answer for minutes caused worktree spawn requests to keep returning
// 409 until daemon restart, so absence gets only a short quiet-period cache.
const NEGATIVE_TTL_MS = 2_000;
const BRANCH_TTL_MS = 20_000;

function cacheGet<V>(
  cache: Map<string, CacheEntry<V>>,
  key: string,
  now = Date.now(),
): V | undefined {
  const hit = cache.get(key);
  if (!hit) return undefined;
  if (hit.expiresAt <= now) {
    cache.delete(key);
    return undefined;
  }
  // Map preserves insertion order. Reinsert a hit so the size cap evicts the
  // least-recently-used cwd, not merely the oldest-created cwd.
  cache.delete(key);
  cache.set(key, hit);
  return hit.value;
}

function cacheSet<V>(
  cache: Map<string, CacheEntry<V>>,
  key: string,
  value: V,
  ttlMs: number,
  now = Date.now(),
): void {
  cache.delete(key);
  cache.set(key, { value, expiresAt: now + ttlMs });
  while (cache.size > CACHE_MAX) {
    // The loop condition guarantees a key exists; the guard only satisfies the
    // `IteratorResult.value` (string | undefined) type.
    const oldest = cache.keys().next().value;
    if (oldest === undefined) break;
    cache.delete(oldest);
  }
}

function isDirectory(cwd: string): boolean {
  try {
    return fs.statSync(cwd).isDirectory();
  } catch {
    return false;
  }
}

function git(args: string[], cwd: string): string | null {
  // These calls intentionally remain synchronous for now: derive.mjs consumes
  // deriveRepo()/branchOf() inline while constructing SQL updates. Moving git
  // off the daemon event loop is worthwhile, but requires making that caller
  // chain async as one coordinated change; silently changing this module's
  // return contract would instead write Promises into session state.
  try {
    const raw = execFileSync('git', args, {
      cwd,
      timeout: 1500,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      // Live env, not Bun's startup snapshot (see exec.ts): a runtime PATH/HOME
      // mutation only reaches git when env is explicit; a no-op under Node.
      env: process.env,
    });
    // Strip only the single record-terminating newline (LF, or CRLF under
    // core.autocrlf) — never trim(): path-valued output (--show-toplevel,
    // worktree list) may legitimately end in whitespace on POSIX, and
    // trim() would corrupt such a path into one that does not exist.
    const out = raw.replace(/\r?\n$/, '');
    return out || null;
  } catch {
    return null;
  }
}

function canon(p: string): string {
  try {
    return fs.realpathSync(p);
  } catch {
    return path.resolve(p);
  }
}

// A path from `worktree list --porcelain` is a usable checkout only if it is a
// working tree. A bare repo (or a --separate-git-dir metadata directory) has
// the shape of a git dir — HEAD + objects/ + refs/ — and no checked-out files.
function isBareGitDir(absPath: string): boolean {
  try {
    return (
      fs.existsSync(path.join(absPath, 'HEAD')) &&
      fs.statSync(path.join(absPath, 'objects')).isDirectory() &&
      fs.statSync(path.join(absPath, 'refs')).isDirectory()
    );
  } catch {
    return false;
  }
}

export function deriveRepo(cwd: string | null | undefined): RepoIdentity {
  if (!cwd)
    return { repo_id: null, repo_name: null, worktree: null, main_tree: null, is_git: false };
  // Validate before consulting the cache too: a formerly valid directory may
  // have been removed, and git's 1.5 s timeout is wasted work for files/missing
  // paths. Invalid paths are not cached so a later mkdir is noticed at once.
  if (!isDirectory(cwd)) {
    const c = canon(cwd);
    return { repo_id: c, repo_name: path.basename(c), worktree: c, main_tree: c, is_git: false };
  }
  const hit = cacheGet(identityCache, cwd);
  if (hit !== undefined) return hit;

  let out: RepoIdentity;
  const common = git(['rev-parse', '--git-common-dir'], cwd);
  if (common) {
    // --git-common-dir may be relative (usually ".git"); resolve against cwd,
    // then canonicalize so every worktree of the repo lands on one repo_id.
    const commonAbs = canon(path.isAbsolute(common) ? common : path.resolve(cwd, common));
    const toplevel = git(['rev-parse', '--show-toplevel'], cwd);
    // Normal repositories and linked worktrees share <main>/.git. For less
    // conventional layouts, git worktree list is the source of main-tree
    // candidates — but only records that point at a real working tree count.
    // With `git init --separate-git-dir`, the FIRST porcelain record is the
    // metadata directory itself (git resolves the main worktree through the
    // config's core.worktree, not its directory name), so taking the first
    // record blindly would catalog the .git metadata dir as the main checkout.
    const listedTrees =
      path.basename(commonAbs) === '.git'
        ? []
        : (git(['worktree', 'list', '--porcelain'], cwd) ?? '')
            .split('\n')
            .filter((line) => line.startsWith('worktree '))
            .map((line) => line.slice(9));
    const listedMain = listedTrees.find(
      (p) => path.basename(canon(p)) !== '.git' && !isBareGitDir(canon(p)),
    );
    const mainTree =
      path.basename(commonAbs) === '.git'
        ? path.dirname(commonAbs)
        : canon(listedMain ?? toplevel ?? cwd);
    out = {
      repo_id: commonAbs,
      repo_name: path.basename(mainTree).replace(/\.git$/, '') || path.basename(mainTree),
      worktree: toplevel ? canon(toplevel) : canon(cwd),
      main_tree: canon(mainTree),
      is_git: true,
    };
  } else {
    const c = canon(cwd);
    out = { repo_id: c, repo_name: path.basename(c), worktree: c, main_tree: c, is_git: false };
  }
  cacheSet(identityCache, cwd, out, out.is_git ? IDENTITY_TTL_MS : NEGATIVE_TTL_MS);
  return out;
}

// `fresh` bypasses the TTL cache READ (still WRITES it) — used at naming moments
// (SessionStart/agents-cli/spawn births) where a stale cached branch could
// ticket-name a session for the branch it was on 20s ago. It stays execFileSync:
// the naming paths derive a name and INSERT in one synchronous block with no
// await, and the ticket key is read off this branch in that same tick (the
// synchrony invariant). The 20s TTL is fine for the display column and the
// later rename trigger, which tolerate lag.
export function branchOf(
  cwd: string | null | undefined,
  { fresh = false }: { fresh?: boolean } = {},
): string | null {
  if (!cwd || !isDirectory(cwd)) return null;
  const now = Date.now();
  if (!fresh) {
    const hit = cacheGet(branchCache, cwd, now);
    if (hit !== undefined) return hit;
  }
  const branch = git(['rev-parse', '--abbrev-ref', 'HEAD'], cwd);
  // A null branch commonly means "not a repo yet", so it gets the same short
  // retry horizon as a negative identity instead of hiding a later git init.
  cacheSet(branchCache, cwd, branch, branch == null ? NEGATIVE_TTL_MS : BRANCH_TTL_MS, now);
  return branch;
}

// Canonicalize an edited file's path before keying so that two lexical paths
// resolving to the same inode (e.g. `alias/x.js` where `alias` is a symlink to
// `real/`) share one ledger key and the conflict radar still fires. The file
// may not exist yet (a Write creating it), so resolve the nearest existing
// ancestor and re-append the unresolved suffix; on any failure the lexical
// path stands — missing an alias is preferable to breaking keying.
function canonFile(p: string): string {
  let target = path.resolve(p);
  const suffix: string[] = [];
  for (let i = 0; i < 64; i++) {
    try {
      return path.join(fs.realpathSync(target), ...suffix.reverse());
    } catch {
      /* keep walking */
    }
    const parent = path.dirname(target);
    if (parent === target) return path.resolve(p);
    suffix.push(path.basename(target));
    target = parent;
  }
  return path.resolve(p);
}

// Ledger key for an edited file, used to detect conflicting concurrent edits:
// (repo_id, repo-relative path); absolute path fallback outside git.
// `session` (a card row) lets us skip the subprocess when the file sits inside
// the session's own worktree — the common case.
// Containment predicate for a path relative to a tree root: the file sits
// inside the tree unless the relative path escapes upward. Only the `..`
// segment itself escapes — a legitimate root file whose name merely starts
// with two dots (`..config`, `...hidden`) is still inside the tree.
function insideTree(rel: string): boolean {
  return rel !== '' && rel !== '..' && !rel.startsWith('..' + path.sep) && !path.isAbsolute(rel);
}

export function ledgerKey(absPath: string, session?: SessionRef | null): LedgerKey {
  absPath = canonFile(absPath);
  // Fast path: file inside the session's own git worktree (cache hit, no
  // subprocess). Only valid when that worktree really is git — a non-git
  // session must fall through to the absolute-path key like everyone else.
  if (session?.worktree && session.repo_id && deriveRepo(session.cwd).is_git) {
    const rel = path.relative(session.worktree, absPath);
    if (insideTree(rel)) {
      return { repo_id: session.repo_id, rel_path: rel, worktree: session.worktree };
    }
  }
  const repo = deriveRepo(path.dirname(absPath)); // cached per directory
  if (repo.is_git) {
    const rel = path.relative(repo.worktree, absPath);
    if (insideTree(rel)) {
      return { repo_id: repo.repo_id, rel_path: rel, worktree: repo.worktree };
    }
  }
  return { repo_id: '', rel_path: absPath, worktree: null }; // outside git: absolute path key
}
