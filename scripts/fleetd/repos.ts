// repos.ts — durable repository catalog/settings plus clone and branch
// materialization for repo-mode board spawns. Every git subprocess is an argv
// array through execFileP; no repository input is ever interpolated into a
// shell command.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileP, baseBranch, distillGitStderr, gitStderrDetail, redactGitText } from './exec.ts';
import type { ExecResult } from './exec.ts';
import { detectCoderWorkspaceRoot } from './config.ts';
import type { Statements } from './statements.ts';

// eslint-disable-next-line no-control-regex -- refusing NUL/C0/DEL in a repos_dir path is the entire purpose of this gate
const CONTROL_RE = /[\x00-\x1f\x7f]/;
// eslint-disable-next-line no-control-regex -- same gate plus whitespace, for repo/branch/origin argv safety
const SPACE_OR_CONTROL_RE = /[\s\x00-\x1f\x7f]/;
const FORGE_SLUG_SEGMENT_RE = /^[A-Za-z0-9._-]+$/;
// User-requested Coder seed: company workspaces type a bare repo name far more
// often than a full owner/repo slug. Precedence is persisted setting → env →
// this Coder-only seed → no default. Non-Coder installs never inherit it.
const CODER_DEFAULT_ORG = 'textemma';

// A git failure carries an HTTP status the daemon relays verbatim. The old
// namedError stamped `.status` onto a plain Error, which strict TS rejects
// (Error has no `status`); a tiny subclass carries it as a typed field instead.
// errStatus/errMessage read an unknown catch value back the way settings.ts
// does — the exact-same helpers, so the two surfaces stay behaviourally paired.
class RepoError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

function namedError(status: number, message: string): RepoError {
  return new RepoError(status, message);
}

function errStatus(err: unknown): number | undefined {
  if (typeof err === 'object' && err !== null && 'status' in err) {
    const status = err.status;
    return typeof status === 'number' ? status : undefined;
  }
  return undefined;
}

function errMessage(err: unknown): string {
  return err instanceof Error && err.message ? err.message : String(err);
}

// Anchored userinfo probe for a clone origin (not the free-text scrubber — that
// one is global and runs over stderr). Greedy to the LAST `@` before the path,
// matching scrubUrlCredentials, so `https://u:p@ss@host/x` yields `u:p@ss`.
const ORIGIN_USERINFO_RE = /^[a-z][a-z0-9+.-]{0,32}:\/\/([^/?#\s]{0,512})@/i;
// …and the OTHER place a bare credential hides in a URL: the query string or
// fragment. `?access_token=` (Gitea), `?private_token=` / `?job_token=` (GitLab)
// are real forms, and unlike userinfo they carry no positional marker a scrubber
// can key off without knowing the parameter name. scrubUrlCredentials recognises
// the common NAMES; harvesting the VALUES here as exact needles is what covers a
// name it has never heard of. Value-only, name-agnostic, and applied through the
// same >= 8 length filter as the userinfo halves below. The cost of being
// name-agnostic is accepted deliberately: an innocent `?ref=some-long-branch`
// becomes an exact needle and that branch name is redacted wherever it appears in
// the stderr. A clone origin carrying a query string at all is rare, and losing a
// branch name from a diagnostic is recoverable — losing a token is not.
const ORIGIN_PARAM_VALUE_RE = /[?&#][^=&\s]{1,128}=([^&\s]{1,512})/g;

// ONE hardening pass, BOTH human-facing outputs. Every path that turns a failed
// git step into a note AND a durable detail goes through here, and — the point of
// the rewrite — the note is derived from the SAME hardened string as the detail.
// It was not: the note used to get only the positional URL scrub while the shape
// scrub and these exact needles were applied inside gitStderrDetail, so
// `fatal: helper rejected token ghp_…` masked the token in the expander and
// printed it verbatim in the note six characters away. The note is the WORSE sink
// of the two — card note, 120-char ticker line, HTTP 409 body, and the durable
// SpawnFailed event that outlives the archived card — so it cannot be the one
// with the weaker control. The remaining git failures in this file produce a note
// only, and apply scrubUrlCredentials inline.
//
// `secrets` is the exact-needle list gitStderrDetail cannot derive for itself:
// the userinfo of the origin we were handed, its `:`-split halves, and any query
// or fragment parameter value, because git prints a bare password in
// `remote: HTTP Basic: Access denied for user '…'` where no URL and no known
// credential shape is present to key off. Components shorter than 8 characters
// are dropped on purpose — a 3-character username like `git` used as an exact
// needle would shred unrelated text (`git clone`, `.git/`) into `[redacted]` and
// destroy the remedy's legibility. ACCEPTED RESIDUAL: a sub-8-character password
// is therefore covered only in URL form, by the positional scrub.
function originSecrets(origin_url: string | null | undefined): string[] {
  const origin = origin_url ?? '';
  const userinfo = ORIGIN_USERINFO_RE.exec(origin)?.[1] ?? '';
  const parts = userinfo ? [userinfo, ...userinfo.split(':')] : [];
  for (const match of origin.matchAll(ORIGIN_PARAM_VALUE_RE)) parts.push(match[1] ?? '');
  return [...new Set(parts.filter((part) => part.length >= 8))];
}

function gitFailureText(
  err: string,
  origin_url: string | null = null,
): { note: string; detail: string | null } {
  const secrets = originSecrets(origin_url);
  const hardened = redactGitText(err, secrets);
  // Re-passing `secrets` is deliberate and harmless: gitStderrDetail runs the
  // same idempotent pass over its input, and a caller that ever hands it raw
  // stderr directly still gets the needles applied.
  return { note: distillGitStderr(hardened), detail: gitStderrDetail(hardened, { secrets }) };
}

function repoNameOf(value: string): string {
  const clean = value.replace(/[\\/]+$/, '');
  return path.basename(clean).replace(/\.git$/i, '');
}

// Any argv-relevant segment that begins with `-` is refused. We split on `@`
// as well as `/` and `:` because a scp/ssh URL hides its host behind the
// userinfo: `git@-oProxyCommand=reboot:x` would otherwise sail past — git's
// `--` protects git's OWN argv but still hands `-oProxyCommand=…` to ssh as the
// hostname (CVE-2017-1000117-class). The `@` split closes that.
function unsafeDashSegment(value: string): boolean {
  return value.split(/[/:@]/).some((segment) => segment.startsWith('-'));
}

type RepoKind = 'url' | 'path' | 'shorthand' | 'name';
interface RepoParsed {
  kind: RepoKind;
  origin_url: string | null;
  repo_name: string;
}
interface RepoParseError {
  error: string;
}
type RepoParseResult = RepoParseError | RepoParsed;

// A default forge namespace: `owner` on GitHub, `group/subgroup` on GitLab.
// Return a reason (pure, testable) rather than throwing — the settings surface
// and the spawn resolver use the same gate and choose their own HTTP wording.
export function repoDefaultOrgChoice({
  setting = null,
  env = null,
  coder = false,
}: { setting?: string | null; env?: string | null; coder?: boolean } = {}): {
  value: string | null;
  source: string;
} {
  if (setting) return { value: setting, source: 'override' };
  if (env) return { value: env, source: 'env' };
  if (coder) return { value: CODER_DEFAULT_ORG, source: 'coder' };
  return { value: null, source: 'default' };
}

export function repoDefaultOrgProblem(value: unknown): string | null {
  if (typeof value !== 'string' || !value)
    return 'default org must be a non-empty owner or group path';
  if (value.length > 200) return 'default org must be 200 characters or fewer';
  if (SPACE_OR_CONTROL_RE.test(value))
    return 'default org must not contain whitespace or control characters';
  if (value.startsWith('-') || unsafeDashSegment(value))
    return 'default org must not contain a segment beginning with -';
  const parts = value.split('/');
  if (!parts.every(Boolean) || parts.some((p) => p === '.' || p === '..')) {
    return 'default org must be an owner or clean group/subgroup path';
  }
  if (!parts.every((part) => FORGE_SLUG_SEGMENT_RE.test(part))) {
    return 'default org segments may contain only letters, numbers, dots, underscores, and hyphens';
  }
  return null;
}

export function parseRepoInput(
  input: unknown,
  repoHost = 'github',
  repoTransport = 'https',
): RepoParseResult {
  // repo_host steers ONLY the org/repo shorthand below — it composes the forge
  // origin, so a typo'd host must fail loud rather than silently fall back to
  // github and clone the wrong forge. Every other kind (URL/ssh/scp/absolute
  // path/bare name) already carries its own host and ignores repoHost entirely.
  if (repoHost !== 'github' && repoHost !== 'gitlab') {
    return { error: `repo_host must be github or gitlab — got "${repoHost}"` };
  }
  // repo_transport steers the SAME shorthand: ssh composes the scp-style
  // git@{host}:{slug}.git, https the https://{host}/{slug}.git. A typo'd
  // transport fails loud like repo_host. The param default stays https so this
  // PURE function is byte-stable for every existing two-arg caller — the daemon
  // SETTING owns the ssh default (see resolveTarget/resolveRepoTransport), not
  // this function.
  if (repoTransport !== 'ssh' && repoTransport !== 'https') {
    return { error: `repo_transport must be ssh or https — got "${repoTransport}"` };
  }
  if (typeof input !== 'string' || !input) return { error: 'repo must be a non-empty string' };
  if (SPACE_OR_CONTROL_RE.test(input))
    return { error: 'repo must not contain whitespace or control characters' };
  if (input.startsWith('-') || unsafeDashSegment(input))
    return { error: 'repo must not contain a path or argument segment beginning with -' };

  if (/^https:\/\//i.test(input)) {
    let url;
    try {
      url = new URL(input);
    } catch {
      return { error: 'repo is not a valid HTTPS URL' };
    }
    const repo_name = repoNameOf(url.pathname);
    if (!repo_name) return { error: 'repository URL must name a repository' };
    return { kind: 'url', origin_url: input, repo_name };
  }
  if (/^ssh:\/\//i.test(input)) {
    let url;
    try {
      url = new URL(input);
    } catch {
      return { error: 'repo is not a valid SSH URL' };
    }
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
    if (scheme === 'http')
      return { error: 'plain http repository URLs are refused — use https or ssh' };
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
  const cleanSegments =
    parts.every(Boolean) && !parts.some((part) => part === '.' || part === '..');
  if (
    cleanSegments &&
    parts.length >= 2 &&
    !parts.every((part) => FORGE_SLUG_SEGMENT_RE.test(part))
  ) {
    return {
      error:
        'shorthand path segments may contain only letters, numbers, dots, underscores, and hyphens',
    };
  }
  if (cleanSegments && parts.length >= 2) {
    // repoNameOf strips a trailing '.git', so a final segment of exactly
    // '.git' ('org/.git', 'group/sub/.git') names NOTHING — downstream,
    // resolveTarget would path.join(reposDir, '') and the clone dest would
    // collapse onto the repos root itself. Refuse a nameless shorthand here,
    // exactly as the URL branches refuse a nameless pathname.
    const repo_name = repoNameOf(parts[parts.length - 1] ?? '');
    if (!repo_name) return { error: 'shorthand must end in a repository name' };
    // Injection-safe by construction: the host is a constant, and `slug` is the
    // already-gated input (SPACE_OR_CONTROL_RE + unsafeDashSegment above) with
    // only its trailing `.git` stripped before we re-append our own. cloneRepo
    // re-gates the composed argv, and normalizeRemoteOrigin unifies the ssh/scp
    // spelling with https for reuse — so an ssh clone still proves an https
    // shorthand request and vice-versa.
    if (repoHost === 'gitlab') {
      const slug = input.replace(/\.git$/i, '');
      return {
        kind: 'shorthand',
        origin_url:
          repoTransport === 'ssh' ? `git@gitlab.com:${slug}.git` : `https://gitlab.com/${slug}.git`,
        repo_name,
      };
    }
    if (parts.length === 2) {
      const slug = input.replace(/\.git$/i, '');
      return {
        kind: 'shorthand',
        origin_url:
          repoTransport === 'ssh' ? `git@github.com:${slug}.git` : `https://github.com/${slug}.git`,
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

export function quickBranchCheck(branch: unknown): string | null {
  if (typeof branch !== 'string' || !branch) return 'branch must be a non-empty string';
  if (branch.length > 200) return 'branch must be 200 characters or fewer';
  if (branch.startsWith('-')) return 'branch must not begin with -';
  if (SPACE_OR_CONTROL_RE.test(branch))
    return 'branch must not contain whitespace or control characters';
  if (branch.includes('..')) return 'branch must not contain ..';
  if (branch.includes('@{')) return 'branch must not contain @{';
  if (branch.endsWith('.lock')) return 'branch must not end with .lock';
  if (branch.startsWith('/') || branch.endsWith('/')) return 'branch must not begin or end with /';
  return null;
}

function expandHome(value: string): string {
  if (value === '~') return os.homedir();
  if (value.startsWith('~/')) return path.join(os.homedir(), value.slice(2));
  return value;
}

// A remote's identity on a RECOGNIZED forge is its host+path, not its transport
// spelling: `https://gitlab.com/org/repo.git`, `ssh://git@gitlab.com/org/repo.git`
// and `git@gitlab.com:org/repo.git` are three doors into ONE repository. The
// reuse guard in resolveTarget compares origins to prove a same-named checkout
// really IS the requested repo; comparing raw strings made an ssh-cloned
// checkout invisible to an https/shorthand spawn (409 "exists and is not", or a
// duplicate clone). Reducing the three shapes to one lowercase `//host/path`
// key widens reuse ONLY across spellings — two origins with a different host or
// path still never match, so an unrelated tree remains exactly as un-reusable
// as before.
//
// That unification is sound ONLY where the service's own semantics guarantee
// it, so it applies to the explicitly recognized forges (github.com,
// gitlab.com): both are case-insensitive in owner and repo name (you cannot
// register `Org/repo` AND `org/Repo`), and both front every transport with the
// single account-agnostic `git` ssh user, so no spelling of userinfo picks a
// different repository. Everywhere else the "three doors" assumption is NOT
// ours to make: a generic ssh host authenticates BY user (`alice@host` and
// `bob@host` can be different accounts with different filesystems), an https
// path can be case-sensitive, and unifying any of that would "prove" a checkout
// is an unrelated repo — reuse would then edit, commit, and run hooks in the
// WRONG tree across a credential boundary. Generic hosts therefore keep the
// conservative comparison: scheme-agnostic (an ssh spelling of an https URL
// string still matches, so a missed match is at worst the OLD 409/spare-clone
// behaviour, never a wrong reuse) but username- and case-PRESERVING, with only
// the hostname lowercased (DNS is case-insensitive everywhere).
// Conservative by construction:
//  - only https://, unported ssh://, and scp-style origins normalize; any other
//    shape returns null and keeps the old fallback string comparison — the
//    worst outcome of a missed match is the OLD behaviour (a spare clone or a
//    409), never a wrong reuse;
//  - an ssh:// URL with an explicit port is NOT normalized: a nonstandard port
//    can front a different server on the same hostname (forwards, multiplexed
//    bastions), and proving it equal to the https/:22 repo is not ours to assume;
//  - the `//host/path` key cannot collide with the other key families: realpath
//    keys start with a single `/`, and an origin string starting with `//` is
//    posix-absolute so it takes the realpath branch, never the fallback.
const FORGE_HOSTS = new Set(['github.com', 'gitlab.com']);

function normalizeRemoteOrigin(value: string): string | null {
  const input = value;
  // No initializer: both the url and scp branches assign `user` before it is
  // read, so a `= null` seed would be dead (no-useless-assignment).
  let user: string | null;
  let host: string;
  let rest: string;
  const url = /^(?:https|ssh):\/\/(?:([^/?#@]*)@)?([^/?#]+)(\/[^?#]*)$/i.exec(input);
  if (url) {
    // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing
    user = url[1] || null;
    const h = url[2];
    if (h === undefined || h.includes(':')) return null; // ported (or hostless) — fall back
    host = h;
    rest = url[3] ?? '';
  } else {
    if (input.includes('://')) return null; // some other scheme — fall back
    const scp = /^(?:([^/@:]+)@)?([^/:@]+):(.+)$/.exec(input);
    if (!scp) return null;
    // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing
    user = scp[1] || null;
    const h = scp[2];
    if (h === undefined) return null;
    host = h;
    rest = scp[3] ?? '';
  }
  const cleaned = rest
    .replace(/^\/+/, '')
    .replace(/[\\/]+$/, '')
    .replace(/\.git$/i, '');
  if (!cleaned) return null;
  if (FORGE_HOSTS.has(host.toLowerCase())) return `//${host}/${cleaned}`.toLowerCase();
  return `//${host.toLowerCase()}/${user == null ? '' : `${user}@`}${cleaned}`;
}

// Split an origin's case-insensitive leading component (URL scheme, plus the
// scp-style userinfo@host up to the `:`) from the remainder. Only the former
// may be case-folded — a scheme is case-insensitive by RFC 3986, and the
// scp-prefix lowercases the hostname with it. The remainder is a repository
// path (or an opaque non-URL string), whose case can distinguish repositories.
function foldOriginCaseInsensitivePrefix(origin: string): string {
  const scp = /^[^/@:]+@/.exec(origin)?.[0];
  if (scp !== undefined) return scp.toLowerCase() + origin.slice(scp.length);
  const scheme = /^[A-Za-z][A-Za-z0-9+.-]{0,32}:/.exec(origin)?.[0];
  if (scheme !== undefined) return scheme.toLowerCase() + origin.slice(scheme.length);
  return origin;
}

function comparableOrigin(value: string | null | undefined): string | null {
  if (!value) return null;
  if (path.isAbsolute(value)) {
    try {
      return fs.realpathSync(value);
    } catch {
      return path.resolve(value);
    }
  }
  return (
    normalizeRemoteOrigin(value) ??
    foldOriginCaseInsensitivePrefix(value.replace(/[\\/]+$/, '').replace(/\.git$/i, ''))
  );
}

function exists(pathname: string): boolean {
  try {
    return fs.existsSync(pathname);
  } catch {
    return false;
  }
}

function isDirectory(pathname: string): boolean {
  try {
    return fs.statSync(pathname).isDirectory();
  } catch {
    return false;
  }
}

async function gitRepoKind(root: string): Promise<'bare' | 'worktree' | null> {
  if (!isDirectory(root)) return null;
  const probe = await execFileP('git', ['-C', root, 'rev-parse', '--is-bare-repository'], {
    timeout: 5_000,
  });
  if (!probe.ok) return null;
  return probe.out.trim() === 'true' ? 'bare' : 'worktree';
}

async function originOf(root: string): Promise<string | null> {
  const result = await execFileP('git', ['-C', root, 'remote', 'get-url', 'origin'], {
    timeout: 5_000,
  });

  return result.ok ? result.out.trim() || null : null;
}

interface WorktreeEntry {
  path: string;
  branch: string | null;
}

function parseWorktrees(porcelain: string): WorktreeEntry[] {
  const rows: WorktreeEntry[] = [];
  let row: WorktreeEntry | null = null;
  for (const line of porcelain.split('\n')) {
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

function dirtyNames(porcelain: string): string[] {
  return porcelain
    .split('\n')
    .filter(Boolean)
    .map((line) => line.slice(3).trim())
    .filter(Boolean);
}

interface ReposCtx {
  q: Statements['q'];
}

type ResolvedTarget =
  | {
      mode: 'local';
      root: string;
      dest: string;
      origin_url: string | null;
      repo_name: string;
      kind: RepoKind;
    }
  | { mode: 'clone'; origin_url: string; dest: string; repo_name: string; kind: RepoKind };

interface ResolveTargetBody {
  repo: string;
  repo_host?: string | null;
  repo_transport?: string | null;
  repo_org?: string | null;
}

interface TouchRepoArgs {
  repo_id?: string | null;
  repo_name?: string | null;
  root?: string | null;
  origin_url?: string | null;
  default_branch?: string | null;
  source?: string | null;
}

export function createRepos(ctx: ReposCtx) {
  const { q } = ctx;
  const touchedAt = new Map<string, number>();
  const provisioningTargets = new Map<string, string>();

  // A clone is an unbounded, minutes-long subprocess that writes to disk;
  // single-flight only dedupes the SAME destination, so without a ceiling a
  // token holder could fire many distinct-URL clones at once and fill the disk.
  // The cap is on concurrent clones only — local materialization is cheap and
  // uncapped. Reservation is synchronous (no await between check and bump) so
  // two racing requests can't both slip past a full pool.
  const cloneCap = (() => {
    const n = Number(process.env['FLEETDECK_CLONE_CONCURRENCY']);
    return Number.isInteger(n) && n > 0 ? n : 3;
  })();
  let clonesInFlight = 0;
  function reserveCloneSlot(): () => void {
    if (clonesInFlight >= cloneCap) {
      throw namedError(
        429,
        `too many repositories are cloning right now (${clonesInFlight}/${cloneCap}) — retry in a moment`,
      );
    }
    clonesInFlight += 1;
    let released = false;
    return () => {
      if (!released) {
        released = true;
        clonesInFlight -= 1;
      }
    };
  }

  // Persist a failed git step's redacted excerpt on the spawn row that asked for
  // it. The `spawn_id` guard is the point of the wrapper: materializeBranch is
  // also reachable from paths that have no spawns row at all (its spawn_id
  // defaults to ''), where the UPDATE would be a silent no-op — an intent worth
  // stating rather than leaving to SQLite's zero-changes result.
  //
  // BEST EFFORT, and the try/catch is the load-bearing part. Both call sites sit
  // inside cloneRepo's try block, whose catch rewrites any error without a
  // `.status` into namedError(409, err.message) — so a throwing UPDATE (a busy or
  // closing DB during shutdown, a statement error) would replace git's distilled
  // verdict with a SQLite message. That is precisely the regression this whole
  // change exists to prevent, in the one code path it owns: a failed attempt to
  // RECORD a diagnostic must never DISPLACE the diagnostic.
  function recordFailDetail(spawn_id: string, detail: string | null): void {
    if (!spawn_id) return;
    try {
      q.setSpawnFailDetail.run(detail, spawn_id);
    } catch (err) {
      console.error(`fleetd could not record fail_detail for ${spawn_id}: ${errMessage(err)}`);
    }
  }

  async function validateBranch(branch: string): Promise<string> {
    const quick = quickBranchCheck(branch);
    if (quick) throw namedError(400, quick);
    const result = await execFileP('git', ['check-ref-format', '--branch', branch], {
      timeout: 5_000,
    });

    if (!result.ok) throw namedError(400, result.err || 'branch is not a valid git branch name');
    return branch;
  }

  function resolveReposDir(): { value: string; source: string; resolved: string } {
    const override = q.getSetting.get('repos_dir')?.value;
    if (override != null) {
      return { value: override, source: 'override', resolved: path.resolve(expandHome(override)) };
    }
    const envDir = process.env['FLEETDECK_REPOS_DIR'];
    if (envDir) {
      return { value: envDir, source: 'env', resolved: path.resolve(expandHome(envDir)) };
    }
    // Default: `/workspace` DIRECTLY on a Coder box (its persisted disk — not a
    // subfolder, per the locked decision), else ~/projects everywhere else.
    const value = detectCoderWorkspaceRoot() ?? path.join(os.homedir(), 'projects');
    return { value, source: 'default', resolved: value };
  }

  // The single read that STEERS a shorthand spawn's transport (D1/D2). ssh is
  // the default here — the SETTING owns it — while parseRepoInput's own param
  // default stays https. spawns.mjs never re-reads this: it relies on
  // resolveTarget, which passes an explicit transport to parseRepoInput below.
  function resolveRepoTransport(): string {
    return q.getSetting.get('repo_transport')?.value ?? 'ssh';
  }

  // A bare repo name (`earm-module`) first keeps the long-standing local-catalog
  // behavior. Only when no known checkout exists does this namespace turn it
  // into `org/earm-module` and clone. Precedence mirrors the other settings:
  // a persisted human choice wins, then an environment/template choice, then
  // the Coder-only company seed requested by the user. No default elsewhere.
  function resolveRepoDefaultOrg(): { value: string | null; source: string } {
    return repoDefaultOrgChoice({
      setting: q.getSetting.get('repo_default_org')?.value ?? null,
      env: process.env['FLEETDECK_DEFAULT_ORG'] ?? null,
      coder: !!detectCoderWorkspaceRoot(),
    });
  }

  function validateRepoDefaultOrg(value: string | null): string | null {
    if (value == null) return null;
    const problem = repoDefaultOrgProblem(value);
    if (problem) throw namedError(400, problem);
    return value;
  }

  function setReposDir(value: unknown): { value: string; source: string; resolved: string } {
    if (value === null) {
      q.setSetting.run('repos_dir', null, Date.now());
      return resolveReposDir();
    }
    if (typeof value !== 'string' || !value)
      throw namedError(400, 'repos_dir must be an absolute path or null');
    if (CONTROL_RE.test(value))
      throw namedError(400, 'repos_dir must not contain NUL or control characters');
    // The value ITSELF (post ~ expansion) must be absolute, checked BEFORE
    // path.resolve — resolve() absolutizes ANY relative string against the
    // daemon's cwd, so the old isAbsolute(resolved) check was a tautology and
    // "." would have persisted as a cwd-dependent repos root.
    const expanded = expandHome(value);
    if (!path.isAbsolute(expanded))
      throw namedError(400, 'repos_dir must be an absolute path (or begin with ~/)');
    const resolved = path.resolve(expanded);
    if (path.dirname(resolved) === resolved)
      throw namedError(400, 'repos_dir must not be the filesystem root');
    try {
      if (fs.existsSync(resolved)) {
        // Follow symlinks and require a real directory: clones land under this
        // path and persist, so a file (or a symlink to one) is refused up front
        // rather than surfacing as a confusing clone failure later.
        if (!fs.statSync(resolved).isDirectory())
          throw namedError(400, 'repos_dir points to an existing file');
      }
    } catch (err) {
      if (errStatus(err) !== undefined) throw err;
      throw namedError(400, `cannot inspect repos_dir: ${errMessage(err)}`);
    }
    q.setSetting.run('repos_dir', value, Date.now());
    return resolveReposDir();
  }

  function touchRepo({
    repo_id,
    repo_name,
    root,
    origin_url = null,
    default_branch = null,
    source,
  }: TouchRepoArgs): void {
    if (!repo_id || !repo_name || !root) return;
    const now = Date.now();
    if (now - (touchedAt.get(repo_id) ?? 0) < 60_000) return;
    touchedAt.set(repo_id, now);
    q.upsertRepo.run(
      repo_id,
      repo_name,
      root,
      origin_url,
      default_branch,
      now,
      now,
      source ?? null,
    );
    if (!origin_url) {
      // Capture the narrowed strings before the closure: a destructured
      // parameter is not re-narrowed inside a later callback, so read them into
      // locals the Promise chain can rely on.
      const rootDir = root;
      const repoId = repo_id;
      Promise.resolve()
        .then(() => originOf(rootDir))
        .then((found) => {
          if (found) q.setRepoOrigin.run(found, repoId);
        })
        .catch((err: unknown) => {
          console.error('fleetd repo origin backfill error:', err);
        });
    }
  }

  async function resolveTarget(body: ResolveTargetBody): Promise<ResolvedTarget> {
    // `?? undefined` lets the parseRepoInput default ('github') apply when the
    // caller omits repo_host or sends an explicit null — spawns.mjs has already
    // rejected any other non-string/unknown value by the time we get here.
    // Transport resolves in ONE place: the explicit body field wins, else the
    // persisted setting (default ssh). We pass a concrete transport, never
    // parseRepoInput's https default — the setting owns the ssh default.
    const transport = body.repo_transport ?? resolveRepoTransport();
    const host = body.repo_host ?? undefined;
    let parsed = parseRepoInput(body.repo, host, transport);
    if ('error' in parsed) throw namedError(400, parsed.error);
    if (body.repo_org != null && parsed.kind !== 'name') {
      throw namedError(400, 'repo_org applies only to a bare repo name');
    }
    let origin_url = parsed.origin_url;
    const catalogRows = q.repoByName.all(parsed.repo_name);
    let dest: string;

    if (parsed.kind === 'name') {
      const roots = [
        ...new Set(
          catalogRows.map((row) => row.root).filter((root): root is string => Boolean(root)),
        ),
      ];
      if (roots.length > 1)
        throw namedError(
          409,
          `more than one known repo named "${parsed.repo_name}": ${roots.join(', ')}`,
        );
      const soleRoot = roots[0];
      if (soleRoot !== undefined) {
        const row = catalogRows.find((item) => item.root === soleRoot);
        dest = soleRoot;
        origin_url = row?.origin_url ?? null;
      } else {
        // No local/catalog hit: a configured default namespace promotes the bare
        // name to real forge shorthand. Run the composed value through the SAME
        // parseRepoInput safety/host/transport path as explicit org/repo input —
        // never hand-compose a git URL here.
        const org =
          body.repo_org != null
            ? { value: validateRepoDefaultOrg(body.repo_org), source: 'request' }
            : resolveRepoDefaultOrg();
        if (!org.value) {
          throw namedError(
            404,
            `no known repo named "${parsed.repo_name}" — paste owner/repo, a URL, or set a default org`,
          );
        }
        const orgProblem = repoDefaultOrgProblem(org.value);
        if (orgProblem)
          throw namedError(400, `configured ${org.source} default org is invalid — ${orgProblem}`);
        // A multi-segment namespace is a GitLab group/subgroup by definition.
        // Infer that forge only when the caller omitted repo_host; an explicit
        // github choice still fails loud instead of silently changing hosts.
        const promotedHost = host ?? (org.value.includes('/') ? 'gitlab' : undefined);
        const expanded = parseRepoInput(
          `${org.value}/${parsed.repo_name}`,
          promotedHost,
          transport,
        );
        if ('error' in expanded)
          throw namedError(
            400,
            `default org "${org.value}" cannot resolve this repo — ${expanded.error}`,
          );
        parsed = expanded;
        origin_url = parsed.origin_url;
        dest = path.join(resolveReposDir().resolved, parsed.repo_name);
      }
    } else if (parsed.kind === 'path') {
      dest = path.resolve(body.repo);
    } else {
      dest = path.join(resolveReposDir().resolved, parsed.repo_name);
    }

    if (parsed.kind === 'path') {
      const kind = await gitRepoKind(dest);
      if (kind === 'worktree') {
        return {
          mode: 'local',
          root: dest,
          dest,
          origin_url: await originOf(dest),
          repo_name: parsed.repo_name,
          kind: parsed.kind,
        };
      }
      if (exists(dest) && kind !== 'bare')
        throw namedError(409, `${dest} exists and is not ${body.repo}`);
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
        if (candidate === dest)
          throw namedError(409, `${candidate} exists and is not ${body.repo}`);
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
        const matches =
          knownOrigin && comparableOrigin(origin_url) === comparableOrigin(knownOrigin);
        if (!matches) {
          if (candidate === dest) throw namedError(409, `${dest} exists and is not ${body.repo}`);
          continue;
        }
      }
      return {
        mode: 'local',
        root: candidate,
        dest: candidate,
        origin_url: origin_url ?? knownOrigin,
        repo_name: parsed.repo_name,
        kind: parsed.kind,
      };
    }

    if (!origin_url) throw namedError(404, `no usable checkout is known for "${parsed.repo_name}"`);
    return { mode: 'clone', origin_url, dest, repo_name: parsed.repo_name, kind: parsed.kind };
  }

  async function cloneRepo({
    origin_url,
    dest,
    spawn_id,
  }: {
    origin_url: unknown;
    dest: string;
    spawn_id: string;
  }): Promise<string> {
    // Defence in depth: an origin reached via the catalog (a bare-name spawn, or
    // a `git remote get-url` backfill) never passed through parseRepoInput, so
    // re-apply the argv-safety gate here before it becomes a clone argument.
    if (
      typeof origin_url !== 'string' ||
      !origin_url ||
      SPACE_OR_CONTROL_RE.test(origin_url) ||
      origin_url.startsWith('-') ||
      unsafeDashSegment(origin_url)
    ) {
      throw namedError(409, 'refusing to clone an unsafe origin URL');
    }
    const reposDir = resolveReposDir().resolved;
    fs.mkdirSync(reposDir, { recursive: true });
    const temp = `${dest}.fd-cloning-${spawn_id.slice(0, 8)}`;
    try {
      fs.rmSync(temp, { recursive: true, force: true });
      const configuredTimeout = Number(process.env['FLEETDECK_CLONE_TIMEOUT_MS']);
      const timeout =
        Number.isFinite(configuredTimeout) && configuredTimeout > 0 ? configuredTimeout : 600_000;
      const result = await execFileP('git', ['clone', '--', origin_url, temp], {
        timeout,
        env: { GIT_TERMINAL_PROMPT: '0' },
      });
      if (!result.ok) {
        // The full stderr goes to fleetd.log for diagnosis; the human-facing
        // failure carries git's own distilled `fatal:`/`error:` line, so a
        // private-repo auth failure no longer hides behind "Cloning into '…'".
        // This log line keeps the RAW stderr and the RAW (possibly credentialed)
        // origin_url on purpose — fleetd.log is 0600 and is the documented place
        // where credentialed URLs are allowed to land. Nothing below it, in THIS
        // file, is raw — scope that claim to repos.ts and do not read it as a
        // repo-wide invariant: spawns.mjs still returns the raw origin_url in its
        // 202 spawn response (an accepted residual, since that value came from the
        // caller in the same request).
        if (result.err) console.error(`fleetd clone failed — ${origin_url}\n${result.err}`);
        const { note, detail } = gitFailureText(result.err, origin_url);
        // …but the distilled line was ALL a human ever saw, and on the Coder
        // workspace that motivated this it read "fatal: Could not read from
        // remote repository." while the public key and the settings/ssh/new URL
        // to register it sat one line above, in this same stderr, discarded.
        // Persist the bounded excerpt now, while the provisional row is still
        // here: compensation flips it to 'gone' but never deletes it, so the
        // snapshot can offer it on the tombstone.
        recordFailDetail(spawn_id, detail);

        throw namedError(409, note || 'git clone failed');
      }
      fs.renameSync(temp, dest);
      return dest;
    } catch (err) {
      try {
        fs.rmSync(temp, { recursive: true, force: true });
      } catch {
        /* best effort */
      }
      throw errStatus(err) !== undefined ? err : namedError(409, errMessage(err));
    }
  }

  async function materializeBranch({
    root,
    branch,
    mode,
    spawn_id = '',
    sid = spawn_id,
    clone = false,
  }: {
    root: string;
    branch: string;
    mode?: string;
    spawn_id?: string;
    sid?: string;
    clone?: boolean;
  }): Promise<{ runCwd: string; created: { clone: boolean; worktree: boolean }; reused: boolean }> {
    const localBefore = await execFileP(
      'git',
      ['-C', root, 'show-ref', '--verify', '--quiet', `refs/heads/${branch}`],
      { timeout: 5_000 },
    );
    // Best-effort: a fetch failure is fine as long as the branch (or a base to
    // cut it from) already resolves locally — an origin-less local repo can
    // still create a new branch from its own main.
    const fetched = await execFileP('git', ['-C', root, 'fetch', 'origin', '--prune'], {
      timeout: 120_000,
      env: { GIT_TERMINAL_PROMPT: '0' },
    });
    const local = localBefore.ok
      ? localBefore
      : await execFileP(
          'git',
          ['-C', root, 'show-ref', '--verify', '--quiet', `refs/heads/${branch}`],
          { timeout: 5_000 },
        );
    const remote = await execFileP(
      'git',
      ['-C', root, 'show-ref', '--verify', '--quiet', `refs/remotes/origin/${branch}`],
      { timeout: 5_000 },
    );
    const base = await baseBranch(root);
    // `base` is proven non-null on the create paths only by the compound guard
    // below (which throws when local, remote AND base are all absent). TS cannot
    // carry that fact to the `switch -c`/`worktree add -b` argv literals, so this
    // closure over the `const base` re-asserts it at each use — the throw is
    // unreachable at runtime once the guard has run.
    const requireBaseRef = (): string => {
      if (!base)
        throw namedError(409, `branch ${branch} cannot be created — no base branch is available`);
      return base.ref;
    };
    if (!local.ok && !remote.ok && !base) {
      if (fetched.ok) {
        throw namedError(
          409,
          `branch ${branch} does not exist and no base branch is available to create it from`,
        );
      }
      // This branch was STRICTLY WORSE than the clone bug being fixed here: the
      // fetch is not one of this file's console.error sites, so its stderr was
      // destroyed outright — distilled to one line for the card and otherwise
      // gone, not even in fleetd.log. Same treatment as the clone now. The `||`
      // fallback is new too: an empty stderr used to produce a dangling
      // "fetch failed: " with nothing after the colon.
      //
      // Unlike the clone, this path was NEVER handed an origin_url — the remote
      // lives in the checkout's own .git/config, which fleetd never wrote and
      // never validated. Reading it back is what gives the exact-needle layer
      // something to work with; without it a bare, shapeless password in the
      // fetch stderr had no covering layer at all. argv only, no shell, local
      // config read (no network), failure path only, and `.ok` is not required —
      // a missing origin simply yields no needles.
      const originUrl = await execFileP('git', ['-C', root, 'remote', 'get-url', 'origin'], {
        timeout: 5_000,
      });
      const { note, detail } = gitFailureText(
        fetched.err,
        originUrl.ok ? originUrl.out.trim() : null,
      );
      recordFailDetail(spawn_id, detail);

      throw namedError(
        409,
        `branch ${branch} not found locally and fetch failed: ${note || 'git fetch failed'}`,
      );
    }

    if (mode === 'in-place') {
      const status = await execFileP('git', ['-C', root, 'status', '--porcelain'], {
        timeout: 30_000,
      });
      // This and the three like it in the rest of this function (switch,
      // worktree list, worktree add) put UNDISTILLED git stderr straight into a
      // card note and an HTTP body. A credential control applied to the clone
      // path but not to its immediate neighbours would be close to no control at
      // all — a `remote:` line or a repository URL echoed by any of them can carry
      // userinfo just as easily. They stay undistilled on purpose (a dirty-checkout
      // or locked-worktree message is only useful in full); they just stop being
      // unscrubbed. spawns.mjs' `git worktree add` and worktrees.mjs' `worktree
      // prune` were swept into the same convention for uniformity, but this is a
      // convention, not an enforced invariant: a new git call site does not
      // inherit it automatically.
      //
      // redactGitText, not bare scrubUrlCredentials: a repo hook or git
      // extension can print a STANDALONE token (`remote: helper rejected token
      // ghp_…`) that the positional URL scrub provably cannot see — it would
      // ride this throw into the HTTP body, card note, ticker, event log and
      // state snapshot verbatim. Clone/fetch go through gitFailureText, which
      // hardens with the same pass; these sites stay undistilled, so they call
      // it directly instead of paying gitFailureText's distill+detail pair.

      if (!status.ok) throw namedError(409, redactGitText(status.err) || 'git status failed');
      const dirty = dirtyNames(status.out);
      if (dirty.length) {
        const shown = dirty.slice(0, 3).join(', ');
        throw namedError(
          409,
          `checkout is dirty (${dirty.length} files: ${shown}${dirty.length > 3 ? '…' : ''}) — use worktree mode or commit`,
        );
      }
      const args = local.ok
        ? ['-C', root, 'switch', branch]
        : remote.ok
          ? ['-C', root, 'switch', '--track', `origin/${branch}`]
          : ['-C', root, 'switch', '-c', branch, requireBaseRef()];
      const switched = await execFileP('git', args, { timeout: 30_000 });

      if (!switched.ok) throw namedError(409, redactGitText(switched.err) || 'git switch failed');
      return { runCwd: root, created: { clone, worktree: false }, reused: false };
    }

    const listed = await execFileP('git', ['-C', root, 'worktree', 'list', '--porcelain'], {
      timeout: 10_000,
    });

    if (!listed.ok) throw namedError(409, redactGitText(listed.err) || 'git worktree list failed');
    const existing = parseWorktrees(listed.out).find((row) => row.branch === branch);
    if (existing)
      return { runCwd: existing.path, created: { clone, worktree: false }, reused: true };

    const safeBranch = branch.replaceAll('/', '-');
    const basePath = path.join(path.dirname(root), `${path.basename(root)}--fd-${safeBranch}`);

    const dedupPath = `${basePath}-${sid.slice(0, 4) || 'repo'}`;
    const candidates = exists(basePath) ? [dedupPath] : [basePath, dedupPath];
    let last: ExecResult | null = null;
    for (const candidate of candidates) {
      const existedBefore = exists(candidate);
      const args = local.ok
        ? ['-C', root, 'worktree', 'add', candidate, branch]
        : remote.ok
          ? ['-C', root, 'worktree', 'add', '--track', '-b', branch, candidate, `origin/${branch}`]
          : ['-C', root, 'worktree', 'add', '-b', branch, candidate, requireBaseRef()];
      last = await execFileP('git', args, { timeout: 30_000 });
      if (last.ok) return { runCwd: candidate, created: { clone, worktree: true }, reused: false };
      // A failed worktree add can leave a directory/admin record behind. Only
      // unwind it when this attempt observed the path absent beforehand.
      if (!existedBefore && exists(candidate)) {
        await execFileP('git', ['-C', root, 'worktree', 'remove', '--force', candidate], {
          timeout: 30_000,
        });
        try {
          fs.rmSync(candidate, { recursive: true, force: true });
        } catch {
          /* best effort */
        }
        await execFileP('git', ['-C', root, 'worktree', 'prune'], { timeout: 30_000 });
      }
    }

    throw namedError(
      409,
      // `last`, when non-null after the loop, is already narrowed to the
      // failure variant (a success returns inside the loop), so `!last.ok`
      // would be a provably-true redundant check.
      redactGitText(last ? last.err : null) || 'git worktree add failed',
    );
  }

  function canonicalTarget(dest: string): string {
    try {
      return fs.realpathSync(dest);
    } catch {
      return path.resolve(dest);
    }
  }

  function claimTarget(dest: string, callsign: string): () => void {
    const canonical = canonicalTarget(dest);
    const owner = provisioningTargets.get(canonical);
    if (owner) throw namedError(409, `${canonical} is already being provisioned by ${owner}`);
    provisioningTargets.set(canonical, callsign);
    return () => provisioningTargets.delete(canonical);
  }

  function targetOwner(dest: string): string | null {
    return provisioningTargets.get(canonicalTarget(dest)) ?? null;
  }

  return {
    validateBranch,
    resolveReposDir,
    setReposDir,
    touchRepo,
    resolveRepoDefaultOrg,
    validateRepoDefaultOrg,
    resolveTarget,
    cloneRepo,
    materializeBranch,
    claimTarget,
    targetOwner,
    reserveCloneSlot,
  };
}
