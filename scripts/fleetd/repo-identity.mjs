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
// Cache per cwd: one subprocess round per unknown cwd, not per event.

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const identityCache = new Map(); // cwd -> {repo_id, repo_name, worktree, is_git}
const branchCache = new Map();   // cwd -> {branch, at}
const BRANCH_TTL_MS = 20_000;

function git(args, cwd) {
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
  if (!cwd) return { repo_id: null, repo_name: null, worktree: null, is_git: false };
  const hit = identityCache.get(cwd);
  if (hit) return hit;

  let out;
  const common = git(['rev-parse', '--git-common-dir'], cwd);
  if (common) {
    // --git-common-dir may be relative (usually ".git"); resolve against cwd,
    // then canonicalize so every worktree of the repo lands on one repo_id.
    const commonAbs = canon(path.isAbsolute(common) ? common : path.resolve(cwd, common));
    // Main tree = parent of the common ".git" dir; a bare repo has no parent tree.
    const mainTree = path.basename(commonAbs) === '.git' ? path.dirname(commonAbs) : commonAbs;
    const toplevel = git(['rev-parse', '--show-toplevel'], cwd);
    out = {
      repo_id: commonAbs,
      repo_name: path.basename(mainTree).replace(/\.git$/, '') || path.basename(mainTree),
      worktree: toplevel ? canon(toplevel) : canon(cwd),
      is_git: true,
    };
  } else {
    const c = canon(cwd);
    out = { repo_id: c, repo_name: path.basename(c), worktree: c, is_git: false };
  }
  identityCache.set(cwd, out);
  return out;
}

export function branchOf(cwd) {
  if (!cwd) return null;
  const now = Date.now();
  const hit = branchCache.get(cwd);
  if (hit && now - hit.at < BRANCH_TTL_MS) return hit.branch;
  const branch = git(['rev-parse', '--abbrev-ref', 'HEAD'], cwd);
  branchCache.set(cwd, { branch, at: now });
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
