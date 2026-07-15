// repo-identity.mjs — derive {repo_id, repo_name, worktree, branch} from a cwd.
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

const identityCache = new Map(); // cwd -> {value, expiresAt}; insertion order is LRU order
const branchCache = new Map();   // cwd -> {value, expiresAt}; insertion order is LRU order
const CACHE_MAX = 512;
const IDENTITY_TTL_MS = 5 * 60_000;
// A directory can become a repository in place (`git init`). Keeping that
// negative answer for minutes caused worktree spawn requests to keep returning
// 409 until daemon restart, so absence gets only a short quiet-period cache.
const NEGATIVE_TTL_MS = 2_000;
const BRANCH_TTL_MS = 20_000;

function cacheGet(cache, key, now = Date.now()) {
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

function cacheSet(cache, key, value, ttlMs, now = Date.now()) {
  cache.delete(key);
  cache.set(key, { value, expiresAt: now + ttlMs });
  while (cache.size > CACHE_MAX) cache.delete(cache.keys().next().value);
}

function isDirectory(cwd) {
  try { return fs.statSync(cwd).isDirectory(); } catch { return false; }
}

function git(args, cwd) {
  // These calls intentionally remain synchronous for now: derive.mjs consumes
  // deriveRepo()/branchOf() inline while constructing SQL updates. Moving git
  // off the daemon event loop is worthwhile, but requires making that caller
  // chain async as one coordinated change; silently changing this module's
  // return contract would instead write Promises into session state.
  try {
    const out = execFileSync('git', args, {
      cwd, timeout: 1500, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
    return out || null;
  } catch {
    return null;
  }
}

function canon(p) {
  try { return fs.realpathSync(p); } catch { return path.resolve(p); }
}

export function deriveRepo(cwd) {
  if (!cwd) return { repo_id: null, repo_name: null, worktree: null, main_tree: null, is_git: false };
  // Validate before consulting the cache too: a formerly valid directory may
  // have been removed, and git's 1.5 s timeout is wasted work for files/missing
  // paths. Invalid paths are not cached so a later mkdir is noticed at once.
  if (!isDirectory(cwd)) {
    const c = canon(cwd);
    return { repo_id: c, repo_name: path.basename(c), worktree: c, main_tree: c, is_git: false };
  }
  const hit = cacheGet(identityCache, cwd);
  if (hit !== undefined) return hit;

  let out;
  const common = git(['rev-parse', '--git-common-dir'], cwd);
  if (common) {
    // --git-common-dir may be relative (usually ".git"); resolve against cwd,
    // then canonicalize so every worktree of the repo lands on one repo_id.
    const commonAbs = canon(path.isAbsolute(common) ? common : path.resolve(cwd, common));
    const toplevel = git(['rev-parse', '--show-toplevel'], cwd);
    // Normal repositories and linked worktrees share <main>/.git. For less
    // conventional layouts, git worktree list is authoritative about which
    // checkout is the main tree (its first porcelain record).
    const listedMain = path.basename(commonAbs) === '.git'
      ? null
      : git(['worktree', 'list', '--porcelain'], cwd)
        ?.split('\n').find(line => line.startsWith('worktree '))?.slice(9);
    const mainTree = path.basename(commonAbs) === '.git'
      ? path.dirname(commonAbs)
      : canon(listedMain || toplevel || cwd);
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
export function branchOf(cwd, { fresh = false } = {}) {
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

// Ledger key for an edited file, used to detect conflicting concurrent edits:
// (repo_id, repo-relative path); absolute path fallback outside git.
// `session` (a card row) lets us skip the subprocess when the file sits inside
// the session's own worktree — the common case.
export function ledgerKey(absPath, session) {
  // Fast path: file inside the session's own git worktree (cache hit, no
  // subprocess). Only valid when that worktree really is git — a non-git
  // session must fall through to the absolute-path key like everyone else.
  if (session?.worktree && session.repo_id && deriveRepo(session.cwd).is_git) {
    const rel = path.relative(session.worktree, absPath);
    if (rel && !rel.startsWith('..') && !path.isAbsolute(rel)) {
      return { repo_id: session.repo_id, rel_path: rel, worktree: session.worktree };
    }
  }
  const repo = deriveRepo(path.dirname(absPath)); // cached per directory
  if (repo.is_git) {
    const rel = path.relative(repo.worktree, absPath);
    if (rel && !rel.startsWith('..') && !path.isAbsolute(rel)) {
      return { repo_id: repo.repo_id, rel_path: rel, worktree: repo.worktree };
    }
  }
  return { repo_id: '', rel_path: absPath, worktree: null }; // outside git: absolute path key
}
