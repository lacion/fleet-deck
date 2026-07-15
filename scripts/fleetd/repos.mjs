// repos.mjs — durable repository catalog/settings plus clone and branch
// materialization for repo-mode board spawns. Every git subprocess is an argv
// array through execFileP; no repository input is ever interpolated into a
// shell command.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileP, baseBranch } from './exec.mjs';

const CONTROL_RE = /[\x00-\x1f\x7f]/;
const SPACE_OR_CONTROL_RE = /[\s\x00-\x1f\x7f]/;

function namedError(status, message) {
  const err = new Error(message);
  err.status = status;
  return err;
}

function repoNameOf(value) {
  const clean = String(value).replace(/[\\/]+$/, '');
  return path.basename(clean).replace(/\.git$/i, '');
}

// Any argv-relevant segment that begins with `-` is refused. We split on `@`
// as well as `/` and `:` because a scp/ssh URL hides its host behind the
// userinfo: `git@-oProxyCommand=reboot:x` would otherwise sail past — git's
// `--` protects git's OWN argv but still hands `-oProxyCommand=…` to ssh as the
// hostname (CVE-2017-1000117-class). The `@` split closes that.
function unsafeDashSegment(value) {
  return String(value).split(/[/:@]/).some(segment => segment.startsWith('-'));
}

export function parseRepoInput(input, repoHost = 'github') {
  // repo_host steers ONLY the org/repo shorthand below — it composes the forge
  // origin, so a typo'd host must fail loud rather than silently fall back to
  // github and clone the wrong forge. Every other kind (URL/ssh/scp/absolute
  // path/bare name) already carries its own host and ignores repoHost entirely.
  if (repoHost !== 'github' && repoHost !== 'gitlab') {
    return { error: `repo_host must be github or gitlab — got "${repoHost}"` };
  }
  if (typeof input !== 'string' || !input) return { error: 'repo must be a non-empty string' };
  if (SPACE_OR_CONTROL_RE.test(input)) return { error: 'repo must not contain whitespace or control characters' };
  if (input.startsWith('-') || unsafeDashSegment(input)) return { error: 'repo must not contain a path or argument segment beginning with -' };

  if (/^https:\/\//i.test(input)) {
    let url;
    try { url = new URL(input); } catch { return { error: 'repo is not a valid HTTPS URL' }; }
    const repo_name = repoNameOf(url.pathname);
    if (!repo_name) return { error: 'repository URL must name a repository' };
    return { kind: 'url', origin_url: input, repo_name };
  }
  if (/^ssh:\/\//i.test(input)) {
    let url;
    try { url = new URL(input); } catch { return { error: 'repo is not a valid SSH URL' }; }
    const repo_name = repoNameOf(url.pathname);
    if (!repo_name) return { error: 'repository URL must name a repository' };
    return { kind: 'url', origin_url: input, repo_name };
  }
  if (/^[^/@:]+@[^/:]+:.+$/.test(input)) {
    const repo_name = repoNameOf(input.slice(input.indexOf(':') + 1));
    if (!repo_name) return { error: 'scp-style repository URL must name a repository' };
    return { kind: 'url', origin_url: input, repo_name };
  }
  const scheme = /^([A-Za-z][A-Za-z0-9+.-]*):/.exec(input)?.[1]?.toLowerCase();
  if (scheme) {
    if (scheme === 'http') return { error: 'plain http repository URLs are refused — use https or ssh' };
    return { error: `repository URL scheme "${scheme}" is refused — use https or ssh` };
  }
  if (path.isAbsolute(input)) {
    const repo_name = repoNameOf(input);
    if (!repo_name) return { error: 'absolute repository path must name a repository' };
    return { kind: 'path', origin_url: null, repo_name };
  }
  // Shorthand. github addresses exactly one org level (org/repo); gitlab nests
  // subgroups (group/subgroup/…/repo), so it accepts 2+ clean segments and the
  // repo name is the LAST one. Either way the input's trailing `.git` is stripped
  // before we re-append our own — the long-standing github behaviour, kept here.
  const parts = input.split('/');
  const cleanSegments = parts.every(Boolean) && !parts.some(part => part === '.' || part === '..');
  if (cleanSegments && parts.length >= 2) {
    // repoNameOf strips a trailing '.git', so a final segment of exactly
    // '.git' ('org/.git', 'group/sub/.git') names NOTHING — downstream,
    // resolveTarget would path.join(reposDir, '') and the clone dest would
    // collapse onto the repos root itself. Refuse a nameless shorthand here,
    // exactly as the URL branches refuse a nameless pathname.
    const repo_name = repoNameOf(parts[parts.length - 1]);
    if (!repo_name) return { error: 'shorthand must end in a repository name' };
    if (repoHost === 'gitlab') {
      return {
        kind: 'shorthand',
        origin_url: `https://gitlab.com/${input.replace(/\.git$/i, '')}.git`,
        repo_name,
      };
    }
    if (parts.length === 2) {
      return {
        kind: 'shorthand',
        origin_url: `https://github.com/${input.replace(/\.git$/i, '')}.git`,
        repo_name,
      };
    }
    // github with 3+ clean segments is a group/subgroup path, not a mistaken
    // relative path — name that so the fix is obvious instead of misdirecting
    // to "relative repository paths are refused".
    return { error: 'group/subgroup paths need the gitlab host or a full repository URL' };
  }
  if (parts.length > 1 || input === '.' || input === '..' || input.startsWith('.' + path.sep)) {
    return { error: 'relative repository paths are refused — use an absolute path' };
  }
  return { kind: 'name', origin_url: null, repo_name: repoNameOf(input) };
}

export function quickBranchCheck(branch) {
  if (typeof branch !== 'string' || !branch) return 'branch must be a non-empty string';
  if (branch.length > 200) return 'branch must be 200 characters or fewer';
  if (branch.startsWith('-')) return 'branch must not begin with -';
  if (SPACE_OR_CONTROL_RE.test(branch)) return 'branch must not contain whitespace or control characters';
  if (branch.includes('..')) return 'branch must not contain ..';
  if (branch.includes('@{')) return 'branch must not contain @{';
  if (branch.endsWith('.lock')) return 'branch must not end with .lock';
  if (branch.startsWith('/') || branch.endsWith('/')) return 'branch must not begin or end with /';
  return null;
}

function expandHome(value) {
  if (value === '~') return os.homedir();
  if (value.startsWith('~/')) return path.join(os.homedir(), value.slice(2));
  return value;
}

// A remote's identity is its host+path, not its transport spelling: on a forge,
// `https://gitlab.com/org/repo.git`, `ssh://git@gitlab.com/org/repo.git` and
// `git@gitlab.com:org/repo.git` are three doors into ONE repository. The reuse
// guard in resolveTarget compares origins to prove a same-named checkout really
// IS the requested repo; comparing raw strings made an ssh-cloned checkout
// invisible to an https/shorthand spawn (409 "exists and is not", or a duplicate
// clone). Reducing the three shapes to one lowercase `//host/path` key widens
// reuse ONLY across spellings — two origins with a different host or path still
// never match, so an unrelated tree remains exactly as un-reusable as before.
// Conservative by construction:
//  - only https://, unported ssh://, and scp-style origins normalize; any other
//    shape returns null and keeps the old lowercase string comparison — the
//    worst outcome of a missed match is the OLD behaviour (a spare clone or a
//    409), never a wrong reuse;
//  - an ssh:// URL with an explicit port is NOT normalized: a nonstandard port
//    can front a different server on the same hostname (forwards, multiplexed
//    bastions), and proving it equal to the https/:22 repo is not ours to assume;
//  - userinfo is dropped (it may carry credentials, and `git@` vs `oauth2@` does
//    not change which repo is behind the door);
//  - the `//host/path` key cannot collide with the other key families: realpath
//    keys start with a single `/`, and an origin string starting with `//` is
//    posix-absolute so it takes the realpath branch, never the fallback.
function normalizeRemoteOrigin(value) {
  const input = String(value);
  let host;
  let rest;
  const url = /^(?:https|ssh):\/\/([^/?#]+)(\/[^?#]*)$/i.exec(input);
  if (url) {
    host = url[1];
    const at = host.lastIndexOf('@');
    if (at !== -1) host = host.slice(at + 1);
    if (!host || host.includes(':')) return null; // ported (or hostless) — fall back
    rest = url[2];
  } else {
    if (input.includes('://')) return null; // some other scheme — fall back
    const scp = /^(?:[^/@:]+@)?([^/:@]+):(.+)$/.exec(input);
    if (!scp) return null;
    [, host, rest] = scp;
  }
  const cleaned = rest.replace(/^\/+/, '').replace(/[\\/]+$/, '').replace(/\.git$/i, '');
  if (!cleaned) return null;
  return `//${host}/${cleaned}`.toLowerCase();
}

function comparableOrigin(value) {
  if (!value) return null;
  if (path.isAbsolute(value)) {
    try { return fs.realpathSync(value); } catch { return path.resolve(value); }
  }
  return normalizeRemoteOrigin(value)
    ?? String(value).replace(/[\\/]+$/, '').replace(/\.git$/i, '').toLowerCase();
}

function exists(pathname) {
  try { return fs.existsSync(pathname); } catch { return false; }
}

function isDirectory(pathname) {
  try { return fs.statSync(pathname).isDirectory(); } catch { return false; }
}

async function gitRepoKind(root) {
  if (!isDirectory(root)) return null;
  const probe = await execFileP('git', ['-C', root, 'rev-parse', '--is-bare-repository'], { timeout: 5_000 });
  if (!probe.ok) return null;
  return probe.out.trim() === 'true' ? 'bare' : 'worktree';
}

async function originOf(root) {
  const result = await execFileP('git', ['-C', root, 'remote', 'get-url', 'origin'], { timeout: 5_000 });
  return result.ok ? result.out.trim() || null : null;
}

function parseWorktrees(porcelain) {
  const rows = [];
  let row = null;
  for (const line of String(porcelain || '').split('\n')) {
    if (line.startsWith('worktree ')) {
      if (row) rows.push(row);
      row = { path: line.slice(9), branch: null };
    } else if (row && line.startsWith('branch refs/heads/')) {
      row.branch = line.slice('branch refs/heads/'.length);
    } else if (!line && row) {
      rows.push(row);
      row = null;
    }
  }
  if (row) rows.push(row);
  return rows;
}

function dirtyNames(porcelain) {
  return String(porcelain || '').split('\n').filter(Boolean).map(line => line.slice(3).trim()).filter(Boolean);
}

export function createRepos(ctx) {
  const { q, onMutate } = ctx;
  const touchedAt = new Map();
  const provisioningTargets = new Map();

  // A clone is an unbounded, minutes-long subprocess that writes to disk;
  // single-flight only dedupes the SAME destination, so without a ceiling a
  // token holder could fire many distinct-URL clones at once and fill the disk.
  // The cap is on concurrent clones only — local materialization is cheap and
  // uncapped. Reservation is synchronous (no await between check and bump) so
  // two racing requests can't both slip past a full pool.
  const cloneCap = (() => {
    const n = Number(process.env.FLEETDECK_CLONE_CONCURRENCY);
    return Number.isInteger(n) && n > 0 ? n : 3;
  })();
  let clonesInFlight = 0;
  function reserveCloneSlot() {
    if (clonesInFlight >= cloneCap) {
      throw namedError(429, `too many repositories are cloning right now (${clonesInFlight}/${cloneCap}) — retry in a moment`);
    }
    clonesInFlight += 1;
    let released = false;
    return () => { if (!released) { released = true; clonesInFlight -= 1; } };
  }

  async function validateBranch(branch) {
    const quick = quickBranchCheck(branch);
    if (quick) throw namedError(400, quick);
    const result = await execFileP('git', ['check-ref-format', '--branch', branch], { timeout: 5_000 });
    if (!result.ok) throw namedError(400, result.err || 'branch is not a valid git branch name');
    return branch;
  }

  function resolveReposDir() {
    const override = q.getSetting.get('repos_dir')?.value;
    if (override != null) {
      return { value: override, source: 'override', resolved: path.resolve(expandHome(override)) };
    }
    if (process.env.FLEETDECK_REPOS_DIR) {
      const value = process.env.FLEETDECK_REPOS_DIR;
      return { value, source: 'env', resolved: path.resolve(expandHome(value)) };
    }
    const value = path.join(os.homedir(), 'projects');
    return { value, source: 'default', resolved: value };
  }

  function setReposDir(value) {
    if (value === null) {
      q.setSetting.run('repos_dir', null, Date.now());
      return resolveReposDir();
    }
    if (typeof value !== 'string' || !value) throw namedError(400, 'repos_dir must be an absolute path or null');
    if (CONTROL_RE.test(value)) throw namedError(400, 'repos_dir must not contain NUL or control characters');
    const resolved = path.resolve(expandHome(value));
    if (!path.isAbsolute(resolved)) throw namedError(400, 'repos_dir must be an absolute path (or begin with ~/)');
    if (path.dirname(resolved) === resolved) throw namedError(400, 'repos_dir must not be the filesystem root');
    try {
      if (fs.existsSync(resolved)) {
        // Follow symlinks and require a real directory: clones land under this
        // path and persist, so a file (or a symlink to one) is refused up front
        // rather than surfacing as a confusing clone failure later.
        if (!fs.statSync(resolved).isDirectory()) throw namedError(400, 'repos_dir points to an existing file');
      }
    } catch (err) {
      if (err?.status) throw err;
      throw namedError(400, `cannot inspect repos_dir: ${err.message || err}`);
    }
    q.setSetting.run('repos_dir', value, Date.now());
    return resolveReposDir();
  }

  function setSettings(body) {
    if (!body || !Object.prototype.hasOwnProperty.call(body, 'repos_dir')) {
      return { status: 400, body: { ok: false, reason: 'repos_dir is required' } };
    }
    try {
      const repos_dir = setReposDir(body.repos_dir);
      onMutate();
      return { status: 200, body: { ok: true, settings: { repos_dir } } };
    } catch (err) {
      return { status: err.status || 400, body: { ok: false, reason: err.message || String(err) } };
    }
  }

  function touchRepo({ repo_id, repo_name, root, origin_url = null, default_branch = null, source }) {
    if (!repo_id || !repo_name || !root) return;
    const now = Date.now();
    if (now - (touchedAt.get(repo_id) ?? 0) < 60_000) return;
    touchedAt.set(repo_id, now);
    q.upsertRepo.run(repo_id, repo_name, root, origin_url, default_branch, now, now, source ?? null);
    if (!origin_url) {
      Promise.resolve()
        .then(() => originOf(root))
        .then(found => { if (found) q.setRepoOrigin.run(found, repo_id); })
        .catch(err => console.error('fleetd repo origin backfill error:', err));
    }
  }

  async function resolveTarget(body) {
    // `?? undefined` lets the parseRepoInput default ('github') apply when the
    // caller omits repo_host or sends an explicit null — spawns.mjs has already
    // rejected any other non-string/unknown value by the time we get here.
    const parsed = parseRepoInput(body?.repo, body?.repo_host ?? undefined);
    if (parsed.error) throw namedError(400, parsed.error);
    let origin_url = parsed.origin_url;
    let catalogRows = q.repoByName.all(parsed.repo_name);
    let dest;

    if (parsed.kind === 'name') {
      const roots = [...new Set(catalogRows.map(row => row.root).filter(Boolean))];
      if (!roots.length) throw namedError(404, `no known repo named "${parsed.repo_name}" — paste a URL or a path`);
      if (roots.length > 1) throw namedError(409, `more than one known repo named "${parsed.repo_name}": ${roots.join(', ')}`);
      const row = catalogRows.find(item => item.root === roots[0]);
      dest = roots[0];
      origin_url = row?.origin_url ?? null;
    } else if (parsed.kind === 'path') {
      dest = path.resolve(body.repo);
    } else {
      dest = path.join(resolveReposDir().resolved, parsed.repo_name);
    }

    if (parsed.kind === 'path') {
      const kind = await gitRepoKind(dest);
      if (kind === 'worktree') {
        return { mode: 'local', root: dest, dest, origin_url: await originOf(dest), repo_name: parsed.repo_name };
      }
      if (exists(dest) && kind !== 'bare') throw namedError(409, `${dest} exists and is not ${body.repo}`);
      origin_url = dest;
      dest = path.join(resolveReposDir().resolved, parsed.repo_name);
    }

    const candidates = [dest];
    if (parsed.kind !== 'path') {
      for (const row of catalogRows) {
        if (row.root && !candidates.includes(row.root)) candidates.push(row.root);
      }
    }
    for (const candidate of candidates) {
      if (!exists(candidate)) continue;
      const kind = await gitRepoKind(candidate);
      if (kind !== 'worktree') {
        if (candidate === dest) throw namedError(409, `${candidate} exists and is not ${body.repo}`);
        continue;
      }
      const knownOrigin = await originOf(candidate);
      // When the REQUEST names a concrete origin (a URL, org/repo shorthand, or
      // a local-path clone), only reuse a checkout that PROVABLY is that origin.
      // A same-named checkout with no remote — or a different one — is not ours
      // to reuse: doing so would silently run the agent in an unrelated tree
      // (and lets a poisoned no-origin catalog row redirect a spawn). An unknown
      // origin is treated as a non-match, not a maybe. A bare-name/path spawn
      // (origin_url null) still resolves by name as before. Origins compare
      // transport-insensitively (see normalizeRemoteOrigin): https/ssh/scp
      // spellings of one host+path are one repo, so an ssh-cloned checkout
      // proves an https-shorthand request — different host or path never match.
      if (origin_url) {
        const matches = knownOrigin && comparableOrigin(origin_url) === comparableOrigin(knownOrigin);
        if (!matches) {
          if (candidate === dest) throw namedError(409, `${dest} exists and is not ${body.repo}`);
          continue;
        }
      }
      return { mode: 'local', root: candidate, dest: candidate, origin_url: origin_url ?? knownOrigin, repo_name: parsed.repo_name };
    }

    if (!origin_url) throw namedError(404, `no usable checkout is known for "${parsed.repo_name}"`);
    return { mode: 'clone', origin_url, dest, repo_name: parsed.repo_name };
  }

  async function cloneRepo({ origin_url, dest, spawn_id }) {
    // Defence in depth: an origin reached via the catalog (a bare-name spawn, or
    // a `git remote get-url` backfill) never passed through parseRepoInput, so
    // re-apply the argv-safety gate here before it becomes a clone argument.
    if (typeof origin_url !== 'string' || !origin_url
        || SPACE_OR_CONTROL_RE.test(origin_url)
        || origin_url.startsWith('-') || unsafeDashSegment(origin_url)) {
      throw namedError(409, 'refusing to clone an unsafe origin URL');
    }
    const reposDir = resolveReposDir().resolved;
    fs.mkdirSync(reposDir, { recursive: true });
    const temp = `${dest}.fd-cloning-${String(spawn_id).slice(0, 8)}`;
    try {
      fs.rmSync(temp, { recursive: true, force: true });
      const configuredTimeout = Number(process.env.FLEETDECK_CLONE_TIMEOUT_MS);
      const timeout = Number.isFinite(configuredTimeout) && configuredTimeout > 0
        ? configuredTimeout
        : 600_000;
      const result = await execFileP('git', ['clone', '--', origin_url, temp], {
        timeout,
        env: { GIT_TERMINAL_PROMPT: '0' },
      });
      if (!result.ok) throw namedError(409, result.err || 'git clone failed');
      fs.renameSync(temp, dest);
      return dest;
    } catch (err) {
      try { fs.rmSync(temp, { recursive: true, force: true }); } catch { /* best effort */ }
      throw err?.status ? err : namedError(409, err.message || String(err));
    }
  }

  async function materializeBranch({ root, branch, mode, spawn_id = '', sid = spawn_id, clone = false }) {
    const localBefore = await execFileP('git', ['-C', root, 'show-ref', '--verify', '--quiet', `refs/heads/${branch}`], { timeout: 5_000 });
    // Best-effort: a fetch failure is fine as long as the branch (or a base to
    // cut it from) already resolves locally — an origin-less local repo can
    // still create a new branch from its own main.
    const fetched = await execFileP('git', ['-C', root, 'fetch', 'origin', '--prune'], {
      timeout: 120_000,
      env: { GIT_TERMINAL_PROMPT: '0' },
    });
    const local = localBefore.ok ? localBefore
      : await execFileP('git', ['-C', root, 'show-ref', '--verify', '--quiet', `refs/heads/${branch}`], { timeout: 5_000 });
    const remote = await execFileP('git', ['-C', root, 'show-ref', '--verify', '--quiet', `refs/remotes/origin/${branch}`], { timeout: 5_000 });
    const base = await baseBranch(root);
    if (!local.ok && !remote.ok && !base) {
      throw namedError(409, fetched.ok
        ? `branch ${branch} does not exist and no base branch is available to create it from`
        : `branch ${branch} not found locally and fetch failed: ${fetched.err}`);
    }

    if (mode === 'in-place') {
      const status = await execFileP('git', ['-C', root, 'status', '--porcelain'], { timeout: 30_000 });
      if (!status.ok) throw namedError(409, status.err || 'git status failed');
      const dirty = dirtyNames(status.out);
      if (dirty.length) {
        const shown = dirty.slice(0, 3).join(', ');
        throw namedError(409, `checkout is dirty (${dirty.length} files: ${shown}${dirty.length > 3 ? '…' : ''}) — use worktree mode or commit`);
      }
      const args = local.ok ? ['-C', root, 'switch', branch]
        : remote.ok ? ['-C', root, 'switch', '--track', `origin/${branch}`]
          : ['-C', root, 'switch', '-c', branch, base.ref];
      const switched = await execFileP('git', args, { timeout: 30_000 });
      if (!switched.ok) throw namedError(409, switched.err || 'git switch failed');
      return { runCwd: root, created: { clone: !!clone, worktree: false }, reused: false };
    }

    const listed = await execFileP('git', ['-C', root, 'worktree', 'list', '--porcelain'], { timeout: 10_000 });
    if (!listed.ok) throw namedError(409, listed.err || 'git worktree list failed');
    const existing = parseWorktrees(listed.out).find(row => row.branch === branch);
    if (existing) return { runCwd: existing.path, created: { clone: !!clone, worktree: false }, reused: true };

    const safeBranch = branch.replaceAll('/', '-');
    const basePath = path.join(path.dirname(root), `${path.basename(root)}--fd-${safeBranch}`);
    const dedupPath = `${basePath}-${String(sid).slice(0, 4) || 'repo'}`;
    const candidates = exists(basePath) ? [dedupPath] : [basePath, dedupPath];
    let last = null;
    for (const candidate of candidates) {
      const existedBefore = exists(candidate);
      const args = local.ok
        ? ['-C', root, 'worktree', 'add', candidate, branch]
        : remote.ok
          ? ['-C', root, 'worktree', 'add', '--track', '-b', branch, candidate, `origin/${branch}`]
          : ['-C', root, 'worktree', 'add', '-b', branch, candidate, base.ref];
      last = await execFileP('git', args, { timeout: 30_000 });
      if (last.ok) return { runCwd: candidate, created: { clone: !!clone, worktree: true }, reused: false };
      // A failed worktree add can leave a directory/admin record behind. Only
      // unwind it when this attempt observed the path absent beforehand.
      if (!existedBefore && exists(candidate)) {
        await execFileP('git', ['-C', root, 'worktree', 'remove', '--force', candidate], { timeout: 30_000 });
        try { fs.rmSync(candidate, { recursive: true, force: true }); } catch { /* best effort */ }
        await execFileP('git', ['-C', root, 'worktree', 'prune'], { timeout: 30_000 });
      }
    }
    throw namedError(409, last?.err || 'git worktree add failed');
  }

  function canonicalTarget(dest) {
    try { return fs.realpathSync(dest); } catch { return path.resolve(dest); }
  }

  function claimTarget(dest, callsign) {
    const canonical = canonicalTarget(dest);
    const owner = provisioningTargets.get(canonical);
    if (owner) throw namedError(409, `${canonical} is already being provisioned by ${owner}`);
    provisioningTargets.set(canonical, callsign);
    return () => provisioningTargets.delete(canonical);
  }

  function targetOwner(dest) {
    return provisioningTargets.get(canonicalTarget(dest)) ?? null;
  }

  return {
    validateBranch, resolveReposDir, setReposDir, setSettings, touchRepo,
    resolveTarget, cloneRepo, materializeBranch, claimTarget, targetOwner,
    reserveCloneSlot,
  };
}
