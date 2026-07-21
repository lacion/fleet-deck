// Working-tree browsing for one fleet session.
//
// This surface exposes working-tree names and bytes to any token-holder in LAN
// mode. The HTTP token and Host-header walls therefore remain mandatory, while
// this module supplies the filesystem walls: roots come only from durable
// session/spawn state, traversal is rejected before filesystem access, real
// paths may not escape the root, symlinks are never opened, walks are bounded,
// and every subprocess is spawned directly with an argv array and hard output
// and time limits. The endpoints are read-only and never invoke a shell.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { deriveRepo } from './repo-identity.mjs';

function envInt(name, fallback, { min = 1 } = {}) {
  const n = Number(process.env[name]);
  return Number.isInteger(n) && n >= min ? n : fallback;
}

const LIST_MAX = envInt('FLEETDECK_FS_LIST_MAX', 1000);
const READ_MAX = envInt('FLEETDECK_FS_READ_MAX', 512 * 1024);
const SEARCH_HITS = envInt('FLEETDECK_FS_SEARCH_HITS', 200);
const SEARCH_TIMEOUT_MS = envInt('FLEETDECK_FS_SEARCH_TIMEOUT_MS', 5000);
const SEARCH_OUTPUT_MAX = 4 * 1024 * 1024;
const BINARY_SNIFF_BYTES = 8000;
const WALK_DEPTH_MAX = 12;
const WALK_ENTRY_MAX = 20_000;
const WALK_FILE_MAX = 2000;
const WALK_FILE_BYTES = 1024 * 1024;
const PER_FILE_HITS = 5;

let searchesInFlight = 0;

class PathError extends Error {
  constructor(status, reason) {
    super(reason);
    this.status = status;
    this.reason = reason;
  }
}

function within(root, candidate) {
  return candidate === root || candidate.startsWith(root + path.sep);
}

function validateRelPath(relPath) {
  if (typeof relPath !== 'string' || relPath.length > 4096 || relPath.includes('\0')
      || path.isAbsolute(relPath) || relPath.split(/[\\/]/).includes('..')) {
    throw new PathError(400, 'invalid path');
  }
  // `.git` is refused HERE, once, so list/read/search all agree: the tree view
  // hides it (readdir filter) and the search backends skip it, but without this
  // gate `fs/read?path=.git/config` (embedded remote credentials, on a plain
  // clone) and `fs/list?path=.git` (the whole object store) would still answer.
  // The wall belongs at the door every operation shares, not on each of them.
  //
  // 0.16.0 CREDENTIAL PATHS join the refusal, same shape: the global explorer
  // roots at the daemon user's HOME by default, and a bearer holder (a phone on
  // LAN, a fleet agent with the token) has no legitimate reason to read private
  // keys, cloud credentials or the daemon's own token through it. Whole-segment
  // names are refused anywhere in the tree (they are near-unique to credential
  // dirs); `.docker` is refused only for its config.json — images.list and
  // friends are harmless to browse. Compared case-insensitively: the default
  // root is HOME on exactly the platforms (macOS, Windows) whose filesystems
  // fold case.
  const segments = relPath.toLowerCase().split(/[\\/]/);
  if (segments.includes('.git')) throw new PathError(404, 'not found');
  if (segments.some(s => CREDENTIAL_SEGMENTS.has(s))) throw new PathError(404, 'not found');
  const dockerAt = segments.indexOf('.docker');
  if (dockerAt !== -1 && segments[dockerAt + 1] === 'config.json') throw new PathError(404, 'not found');
}

const CREDENTIAL_SEGMENTS = new Set(['.ssh', '.aws', '.gnupg', '.netrc', '.kube']);

// 0.16.0: is this entry NAME itself a denied credential path? Used by the
// listing filter and the search walker, which never build a relPath through
// validateRelPath for each entry.
function deniedName(name) {
  return CREDENTIAL_SEGMENTS.has(String(name).toLowerCase());
}

// Pure lexical containment. Callers perform the realpath checks appropriate to
// their operation (the target for list, and the parent before a file open).
export function safeJoin(realRoot, relPath) {
  validateRelPath(relPath);
  const abs = path.resolve(realRoot, relPath || '.');
  if (!within(realRoot, abs)) throw new PathError(400, 'invalid path');
  return abs;
}

export function isBinary(buf) {
  const end = Math.min(buf.length, BINARY_SNIFF_BYTES);
  for (let i = 0; i < end; i += 1) if (buf[i] === 0) return true;
  return false;
}

export function fileType(st) {
  if (st.isSymbolicLink()) return 'symlink';
  if (st.isDirectory()) return 'dir';
  if (st.isFile()) return 'file';
  return 'other';
}

export function clipText(text, max = 400) {
  if (text.length <= max) return text;
  let clipped = text.slice(0, max);
  const last = clipped.charCodeAt(clipped.length - 1);
  if (last >= 0xd800 && last <= 0xdbff) clipped = clipped.slice(0, -1);
  return clipped;
}

function failure(err, fallback = 'not found') {
  if (err instanceof PathError) {
    return { status: err.status, body: { ok: false, reason: err.reason } };
  }
  return { status: 404, body: { ok: false, reason: fallback } };
}

function realpathInside(realRoot, target) {
  let real;
  try { real = fs.realpathSync(target); } catch { throw new PathError(404, 'not found'); }
  if (!within(realRoot, real)) throw new PathError(404, 'not found');
  // 0.16.0: the daemon's own state dir (token, db, captures) must never be
  // readable through the explorer — a symlink inside a browse root pointing
  // into it, or a HOME-rooted path naming it directly, resolves here either
  // way. Resolved once per process; FLEETDECK_HOME does not move mid-flight.
  if (fleetHomeReal === undefined) {
    try { fleetHomeReal = fs.realpathSync(process.env.FLEETDECK_HOME || path.join(os.homedir() || '/tmp', '.fleetdeck')); }
    catch { fleetHomeReal = null; }
  }
  if (fleetHomeReal && within(fleetHomeReal, real)) throw new PathError(404, 'not found');
  // 0.16.0 (adversarial review): the lexical validateRelPath gate only sees
  // the REQUESTED path. A symlink inside the root — work/ssh-link -> ~/.ssh —
  // passes it with segments like ssh-link/id_ed25519 while resolving into a
  // denied directory. Check the RESOLVED path's segments against the same
  // denylist, so credential dirs are refused no matter which name points at
  // them.
  const realSegs = real.toLowerCase().split(path.sep);
  if (realSegs.some(s => CREDENTIAL_SEGMENTS.has(s))
    || (realSegs.includes('.docker') && realSegs[realSegs.length - 1] === 'config.json')) {
    throw new PathError(404, 'not found');
  }
  return real;
}

let fleetHomeReal;

function resolveRoot(ctx, sid) {
  const session = ctx.q.getSession.get(sid);
  if (!session) return { error: { status: 404, body: { ok: false, reason: 'unknown session' } } };
  const spawnRow = ctx.q.spawnBySession.get(sid);
  const candidate = spawnRow?.worktree_path ?? session.worktree ?? session.cwd;
  try {
    if (!candidate || !fs.statSync(candidate).isDirectory()) throw new Error('not a directory');
    const root = fs.realpathSync(candidate);
    return { root, git: deriveRepo(root).is_git };
  } catch {
    return { error: { status: 410, body: { ok: false, reason: 'working tree no longer exists' } } };
  }
}

// The global explorer's root (D4): settings.mjs owns the precedence (browse_root
// setting → FLEETDECK_BROWSE_ROOT env → Coder /workspace → the daemon user's
// home) via browseRootChoice; this thunk resolves it server-side and enforces
// the SAME containment as a session root — the browser names a path relative to
// whatever wins and nothing else. A token-holder in LAN mode can read what the
// daemon user can, so a CONFIGURED root widens exposure and stays auth-gated.
// Fail-loud: a configured root that has vanished returns 410 NAMING its source —
// never a silent fall-through to home (which would leak a different tree than
// the operator pinned).
function resolveBrowseRoot(ctx) {
  const { source, resolved } = ctx.browseRootChoice();
  let root;
  try {
    if (!resolved || !fs.statSync(resolved).isDirectory()) throw new Error('missing');
    root = fs.realpathSync(resolved);
  } catch {
    return { error: { status: 410, body: { ok: false, reason: browseRootGoneReason(source, resolved) } } };
  }
  // The settings validator already bans a CONFIGURED root of / (lexically and
  // by realpath), but the env path is unvalidated — FLEETDECK_BROWSE_ROOT=/
  // (or an alias that realpaths there) would otherwise serve a LAN
  // token-holder the ENTIRE filesystem. Refuse it here, naming the source,
  // exactly like a vanished root: fail loud, never serve.
  if (path.dirname(root) === root) {
    return {
      error: {
        status: 410,
        body: { ok: false, reason: `${browseRootSourceName(source)} must not be the filesystem root` },
      },
    };
  }
  return { root, git: deriveRepo(root).is_git };
}

function browseRootSourceName(source) {
  switch (source) {
    case 'override': return 'the browse_root setting';
    case 'env': return 'FLEETDECK_BROWSE_ROOT';
    case 'detected': return 'the detected Coder workspace root';
    default: return 'the home directory';
  }
}

function browseRootGoneReason(source, resolved) {
  switch (source) {
    case 'override': return `browse_root setting points to a directory that no longer exists: ${resolved}`;
    case 'env': return `FLEETDECK_BROWSE_ROOT points to a directory that no longer exists: ${resolved}`;
    case 'detected': return `the detected Coder workspace root no longer exists: ${resolved}`;
    default: return 'home directory is unavailable';
  }
}

// Spawn with no shell, retaining partial stdout when a child reaches its byte
// or time cap. stderr shares the cap so a failing command cannot bypass it.
export function runBounded(cmd, args, {
  cwd, timeoutMs, maxBytes, input = null,
} = {}) {
  return new Promise(resolve => {
    let child;
    try {
      child = spawn(cmd, args, {
        cwd,
        windowsHide: true,
        stdio: [input == null ? 'ignore' : 'pipe', 'pipe', 'pipe'],
      });
    } catch (error) {
      resolve({ code: null, stdout: Buffer.alloc(0), stderr: String(error?.message || error), truncated: false, timedOut: false });
      return;
    }

    const stdout = [];
    const stderr = [];
    let bytes = 0;
    let truncated = false;
    let timedOut = false;
    let settled = false;
    let spawnError = null;

    const take = (chunks, chunk) => {
      const remaining = Math.max(0, maxBytes - bytes);
      if (remaining) chunks.push(chunk.subarray(0, remaining));
      bytes += chunk.length;
      if (bytes > maxBytes && !truncated) {
        truncated = true;
        try { child.kill('SIGKILL'); } catch { /* already gone */ }
      }
    };
    child.stdout.on('data', chunk => take(stdout, chunk));
    child.stderr.on('data', chunk => take(stderr, chunk));
    child.on('error', error => { spawnError = error; });

    const timer = setTimeout(() => {
      timedOut = true;
      truncated = true;
      try { child.kill('SIGKILL'); } catch { /* already gone */ }
    }, timeoutMs);
    timer.unref?.();

    child.on('close', code => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve({
        code,
        stdout: Buffer.concat(stdout),
        stderr: spawnError ? String(spawnError.message || spawnError) : Buffer.concat(stderr).toString('utf8'),
        truncated,
        timedOut,
      });
    });

    if (input != null) {
      child.stdin.on('error', () => { /* early child exit */ });
      child.stdin.end(input);
    }
  });
}

function entryPath(relDir, name) {
  return relDir ? path.posix.join(relDir.split(path.sep).join('/'), name) : name;
}

async function ignoredPaths(root, paths, timeoutMs) {
  if (!paths.length) return new Set();
  const input = Buffer.from(`${paths.join('\0')}\0`);
  const out = await runBounded('git', ['check-ignore', '-z', '--stdin'], {
    cwd: root, timeoutMs, maxBytes: SEARCH_OUTPUT_MAX, input,
  });
  return new Set(out.stdout.toString('utf8').split('\0').filter(Boolean));
}

function readOpenFile(abs, maxBytes) {
  let fd;
  try {
    fd = fs.openSync(abs, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0));
    const st = fs.fstatSync(fd);
    if (!st.isFile()) return { st, notFile: true };
    const wanted = Math.min(st.size, Math.max(BINARY_SNIFF_BYTES, maxBytes));
    const buf = Buffer.alloc(wanted);
    let offset = 0;
    while (offset < wanted) {
      const n = fs.readSync(fd, buf, offset, wanted - offset, offset);
      if (!n) break;
      offset += n;
    }
    return { st, buf: buf.subarray(0, offset) };
  } finally {
    if (fd != null) fs.closeSync(fd);
  }
}

function parseGitGrep(raw, hitCap) {
  const hits = [];
  const perFile = new Map();
  let overflow = false;
  for (const record of raw.toString('utf8').split('\n')) {
    if (!record) continue;
    const match = /^(.*?):([0-9]+):(.*)$/.exec(record);
    if (!match) continue;
    const count = perFile.get(match[1]) || 0;
    if (count >= PER_FILE_HITS) continue;
    perFile.set(match[1], count + 1);
    if (hits.length >= hitCap) { overflow = true; break; }
    hits.push({ path: match[1], line: Number(match[2]), text: clipText(match[3]) });
  }
  return { hits, overflow };
}

async function gitSearch(root, q, mode, deadline) {
  const remaining = () => Math.max(1, deadline - Date.now());
  if (mode === 'name') {
    const out = await runBounded('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard'], {
      cwd: root, timeoutMs: remaining(), maxBytes: SEARCH_OUTPUT_MAX,
    });
    const needle = q.toLocaleLowerCase();
    const matches = out.stdout.toString('utf8').split('\0').filter(Boolean)
      .filter(name => name.toLocaleLowerCase().includes(needle));
    return {
      hits: matches.slice(0, SEARCH_HITS).map(file => ({ path: file })),
      truncated: out.truncated || out.code !== 0 || matches.length > SEARCH_HITS,
    };
  }

  const baseArgs = ['grep', '-I', '-n', '-i', '-F', '--untracked'];
  let out = await runBounded('git', [...baseArgs, '--max-count=5', '-e', q, '--'], {
    cwd: root, timeoutMs: remaining(), maxBytes: SEARCH_OUTPUT_MAX,
  });
  const optionError = out.code !== 0 && out.code !== 1 && /max-count|unknown option|unrecognized option/i.test(out.stderr);
  if (optionError && Date.now() < deadline) {
    out = await runBounded('git', [...baseArgs, '-e', q, '--'], {
      cwd: root, timeoutMs: remaining(), maxBytes: SEARCH_OUTPUT_MAX,
    });
  }
  const parsed = parseGitGrep(out.stdout, SEARCH_HITS);
  return {
    hits: parsed.hits,
    truncated: out.truncated || (out.code !== 0 && out.code !== 1) || parsed.overflow,
  };
}

// Async on purpose: every operation here is a *sync* fs call (readdir, lstat,
// read) plus per-line CPU, so a big non-git tree would otherwise pin the
// daemon's single thread for the whole deadline — freezing hooks, the board and
// every other session — and, worse, make the 2-in-flight cap meaningless (a
// synchronous walk never yields, so a second search can't run to trip it). We
// hand the event loop back every WALK_YIELD_EVERY entries; the cost is a few
// setImmediate hops, the win is a daemon that still breathes mid-search.
const WALK_YIELD_EVERY = 512;
const yieldToLoop = () => new Promise(resolve => { setImmediate(resolve); });

async function walkSearch(root, q, mode, deadline) {
  const hits = [];
  const needle = q.toLocaleLowerCase();
  const stack = [{ dir: root, rel: '', depth: 0 }];
  let visited = 0;
  let scannedFiles = 0;
  let truncated = false;
  let stop = false;

  while (stack.length && !stop) {
    if (Date.now() >= deadline) { truncated = true; stop = true; break; }
    const current = stack.pop();
    let names;
    try { names = fs.readdirSync(current.dir).sort((a, b) => a.localeCompare(b)); } catch { continue; }
    for (let i = names.length - 1; i >= 0; i -= 1) {
      const name = names[i];
      if (name === '.git') continue;
      // 0.16.0: the search walker never runs validateRelPath per entry, so the
      // credential denylist must filter HERE — otherwise fs/search?mode=content
      // reads ~/.aws and friends even though fs/read refuses them.
      if (deniedName(name)) continue;
      if (++visited > WALK_ENTRY_MAX || Date.now() >= deadline) {
        truncated = true;
        stop = true;
        break;
      }
      if (visited % WALK_YIELD_EVERY === 0) await yieldToLoop();
      const abs = path.join(current.dir, name);
      const rel = entryPath(current.rel, name);
      let st;
      try { st = fs.lstatSync(abs); } catch { continue; }
      if (st.isSymbolicLink()) continue;
      if (st.isDirectory()) {
        if (current.depth < WALK_DEPTH_MAX) stack.push({ dir: abs, rel, depth: current.depth + 1 });
        else truncated = true;
        continue;
      }
      if (!st.isFile()) continue;

      if (mode === 'name') {
        if (rel.toLocaleLowerCase().includes(needle)) hits.push({ path: rel });
      } else {
        if (scannedFiles >= WALK_FILE_MAX) {
          truncated = true;
          stop = true;
          break;
        }
        scannedFiles += 1;
        if (st.size > WALK_FILE_BYTES) { truncated = true; continue; }
        let opened;
        try { opened = readOpenFile(abs, WALK_FILE_BYTES); } catch { continue; }
        if (opened.notFile || isBinary(opened.buf)) continue;
        let perFile = 0;
        const lines = opened.buf.toString('utf8').split('\n');
        for (let line = 0; line < lines.length && perFile < PER_FILE_HITS; line += 1) {
          if (!lines[line].toLocaleLowerCase().includes(needle)) continue;
          hits.push({ path: rel, line: line + 1, text: clipText(lines[line].replace(/\r$/, '')) });
          perFile += 1;
          if (hits.length >= SEARCH_HITS) {
            truncated = true;
            stop = true;
            break;
          }
        }
      }
      if (hits.length >= SEARCH_HITS) {
        truncated = true;
        stop = true;
        break;
      }
    }
  }
  return { hits: hits.slice(0, SEARCH_HITS), truncated };
}

export function createFiles(ctx) {
  // Each operation takes a resolver thunk so the SAME containment/caps logic
  // serves both a per-session root (resolveRoot) and the global browse root
  // (resolveBrowseRoot). The browser never supplies a root either way.
  async function listAt(resolve, relPath) {
    let abs;
    try { validateRelPath(relPath); } catch (err) { return failure(err); }
    const resolved = resolve();
    if (resolved.error) return resolved.error;
    const { root, git } = resolved;
    try {
      abs = safeJoin(root, relPath);
      const real = realpathInside(root, abs);
      const own = fs.lstatSync(abs);
      if (!own.isDirectory() || own.isSymbolicLink()) throw new PathError(404, 'not found');
      const names = fs.readdirSync(real).filter(name => name !== '.git' && !deniedName(name));
      const truncated = names.length > LIST_MAX;
      const entries = [];
      for (const name of names) {
        let st;
        try { st = fs.lstatSync(path.join(real, name)); } catch { continue; }
        entries.push({ name, type: fileType(st), size: st.size, mtime: st.mtimeMs, ignored: false });
      }
      entries.sort((a, b) => {
        const ad = a.type === 'dir' ? 0 : 1;
        const bd = b.type === 'dir' ? 0 : 1;
        return ad - bd || a.name.localeCompare(b.name);
      });
      entries.splice(LIST_MAX);
      if (git && !truncated) {
        const rels = entries.map(entry => entryPath(relPath, entry.name));
        const ignored = await ignoredPaths(root, rels, SEARCH_TIMEOUT_MS);
        for (let i = 0; i < entries.length; i += 1) entries[i].ignored = ignored.has(rels[i]);
      }
      return { status: 200, body: { ok: true, path: relPath, git, entries, truncated } };
    } catch (err) {
      return failure(err);
    }
  }

  async function readAt(resolve, relPath) {
    try { validateRelPath(relPath); } catch (err) { return failure(err); }
    const resolved = resolve();
    if (resolved.error) return resolved.error;
    const { root } = resolved;
    try {
      const abs = safeJoin(root, relPath);
      if (abs === root) return { status: 404, body: { ok: false, reason: 'is a directory' } };
      realpathInside(root, path.dirname(abs));
      let lst;
      try { lst = fs.lstatSync(abs); } catch { throw new PathError(404, 'not found'); }
      if (lst.isDirectory()) return { status: 404, body: { ok: false, reason: 'is a directory' } };
      if (!lst.isFile() || lst.isSymbolicLink()) throw new PathError(404, 'not found');
      const opened = readOpenFile(abs, READ_MAX);
      if (opened.notFile) throw new PathError(404, 'not found');
      const binary = isBinary(opened.buf);
      const truncated = opened.st.size > READ_MAX;
      const body = {
        ok: true, path: relPath, size: opened.st.size, mtime: opened.st.mtimeMs, binary, truncated,
      };
      if (!binary) {
        let content = opened.buf.subarray(0, Math.min(opened.buf.length, READ_MAX));
        if (truncated) {
          const newline = content.lastIndexOf(0x0a);
          content = newline < 0 ? Buffer.alloc(0) : content.subarray(0, newline + 1);
        }
        body.content = content.toString('utf8');
      }
      return { status: 200, body };
    } catch (err) {
      return failure(err);
    }
  }

  async function searchAt(resolve, q, { mode } = {}) {
    if (typeof q !== 'string' || q.length < 2 || q.length > 256) {
      return { status: 400, body: { ok: false, reason: 'query must be 2–256 characters' } };
    }
    if (mode !== 'content' && mode !== 'name') {
      return { status: 400, body: { ok: false, reason: 'invalid search mode' } };
    }
    const resolved = resolve();
    if (resolved.error) return resolved.error;
    if (searchesInFlight >= 2) {
      return { status: 429, body: { ok: false, reason: 'search busy — try again' } };
    }
    searchesInFlight += 1;
    const started = Date.now();
    try {
      const deadline = started + SEARCH_TIMEOUT_MS;
      const result = resolved.git
        ? await gitSearch(resolved.root, q, mode, deadline)
        : await walkSearch(resolved.root, q, mode, deadline);
      return {
        status: 200,
        body: {
          ok: true, mode, q, backend: resolved.git ? 'git' : 'walk',
          hits: result.hits, truncated: result.truncated, elapsed_ms: Date.now() - started,
        },
      };
    } finally {
      searchesInFlight -= 1;
    }
  }

  // Per-session entry points (root resolved from the session id) and the global
  // browse-root explorer share one implementation via the resolver thunk.
  const sessionRoot = sid => () => resolveRoot(ctx, sid);
  const homeRoot = () => resolveBrowseRoot(ctx);
  return {
    fsList: (sid, p) => listAt(sessionRoot(sid), p),
    fsRead: (sid, p) => readAt(sessionRoot(sid), p),
    fsSearch: (sid, q, opts) => searchAt(sessionRoot(sid), q, opts),
    fsListHome: p => listAt(homeRoot, p),
    fsReadHome: p => readAt(homeRoot, p),
    fsSearchHome: (q, opts) => searchAt(homeRoot, q, opts),
  };
}

export {
  validateRelPath, parseGitGrep, within,
  LIST_MAX, READ_MAX, SEARCH_HITS, SEARCH_TIMEOUT_MS,
};
