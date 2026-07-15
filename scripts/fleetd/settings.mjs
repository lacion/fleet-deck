// settings.mjs — the daemon's durable, whitelisted settings surface. One place
// that turns a POST /api/settings body into validated writes and serves the
// resolved settings object to GET /api/settings, the POST response, and the
// /state snapshot.
//
// Why a module of its own (it grew out of repos.mjs's repos_dir-only setter):
// the board now steers four durable preferences — the repos root (repos_dir),
// the shorthand clone transport (repo_transport), the global file-explorer root
// (browse_root), and pinned favourite folders (fav_dirs). Each is a row in the
// generic settings k/v table; each has its own validator; a POST VALIDATES
// every named key before it WRITES any of them (validate-all-then-apply-all),
// so a mixed body with one bad field leaves the store untouched. Unknown keys
// are refused BY NAME — a typo'd key must fail loud, never silently no-op, and
// old {repos_dir}-only clients keep working because repos_dir is still a member
// of the whitelist.
//
// The pure path gates (absolute after ~ expansion, no control chars, not the
// filesystem root, not an existing file) are the SAME ones repos.mjs's
// setReposDir enforces — repos_dir still delegates its WRITE to setReposDir so
// there is one writer for the repos root; browse_root mirrors those gates here.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { detectCoderWorkspaceRoot } from './config.mjs';

const CONTROL_RE = /[\x00-\x1f\x7f]/;
const FAV_DIRS_MAX = 20;
const ALLOWED_KEYS = ['repos_dir', 'repo_transport', 'browse_root', 'fav_dirs'];

function namedError(status, message) {
  const err = new Error(message);
  err.status = status;
  return err;
}

function expandHome(value) {
  if (value === '~') return os.homedir();
  if (value.startsWith('~/')) return path.join(os.homedir(), value.slice(2));
  return value;
}

// Mirror of setReposDir's path gates (repos.mjs:273-295), factored so
// browse_root and (defensively) repos_dir validate identically. Returns the
// resolved absolute path or throws a 400 naming `label`; null is the caller's
// to interpret (clear) and never reaches here.
function validatePathSetting(value, label) {
  if (typeof value !== 'string' || !value) throw namedError(400, `${label} must be an absolute path or null`);
  if (CONTROL_RE.test(value)) throw namedError(400, `${label} must not contain NUL or control characters`);
  // The value ITSELF (post ~ expansion) must be absolute, checked BEFORE
  // path.resolve — resolve() absolutizes ANY relative string against the
  // daemon's cwd, so isAbsolute(resolved) was a tautology and "." would have
  // validated and persisted as a cwd-dependent root.
  const expanded = expandHome(value);
  if (!path.isAbsolute(expanded)) throw namedError(400, `${label} must be an absolute path (or begin with ~/)`);
  const resolved = path.resolve(expanded);
  if (path.dirname(resolved) === resolved) throw namedError(400, `${label} must not be the filesystem root`);
  try {
    // Follow symlinks and refuse an existing non-directory up front, so a file
    // (or a symlink to one) fails here rather than as a confusing browse/clone
    // error later.
    if (fs.existsSync(resolved) && !fs.statSync(resolved).isDirectory()) {
      throw namedError(400, `${label} points to an existing file`);
    }
  } catch (err) {
    if (err?.status) throw err;
    throw namedError(400, `cannot inspect ${label}: ${err.message || err}`);
  }
  // The root ban above is lexical only — an alias like /proc/self/root passes
  // it and realpaths to /. Refuse when the CANONICAL path is the filesystem
  // root too. Best-effort: a not-yet-existing path has no realpath, and the
  // lexical ban already covered the literal spelling.
  try {
    const canonical = fs.realpathSync(resolved);
    if (path.dirname(canonical) === canonical) throw namedError(400, `${label} must not be the filesystem root`);
  } catch (err) {
    if (err?.status) throw err;
    /* ENOENT and friends — nothing further to prove about a missing path */
  }
  return resolved;
}

export function createSettings(ctx) {
  const { q, onMutate, resolveReposDir, setReposDir } = ctx;

  function readSetting(key) {
    return q.getSetting.get(key)?.value ?? null;
  }

  // -------------------------------------------------------------- repo_transport
  // ssh is the RESOLVED default — the SETTING owns it, so parseRepoInput's own
  // third param stays https and the pure function is byte-stable. resolveTarget
  // holds the single read that STEERS a spawn; this pair serves the settings
  // view (it needs the source too), defaulting identically.
  function resolveRepoTransport() {
    const value = readSetting('repo_transport');
    const known = value === 'ssh' || value === 'https';
    return { value: known ? value : 'ssh', source: known ? 'override' : 'default' };
  }

  // -------------------------------------------------------------- browse_root
  // Precedence (D4): the browse_root setting → FLEETDECK_BROWSE_ROOT env →
  // Coder /workspace detection → the daemon user's home. Whichever wins names
  // the `source`; the fs layer (files.mjs) then containment-checks `resolved`
  // and fails LOUD (410 naming this source) if a CONFIGURED root has vanished —
  // it must never silently fall through to home. Pure: resolves a path, never
  // stats it here (files.mjs owns the realpath + existence wall).
  function browseRootChoice() {
    const setting = readSetting('browse_root');
    if (setting != null) {
      return { value: setting, source: 'override', resolved: path.resolve(expandHome(setting)) };
    }
    const env = process.env.FLEETDECK_BROWSE_ROOT;
    if (env) {
      return { value: env, source: 'env', resolved: path.resolve(expandHome(env)) };
    }
    const detected = detectCoderWorkspaceRoot();
    if (detected) {
      return { value: detected, source: 'detected', resolved: detected };
    }
    let home = null;
    try { home = os.homedir(); } catch { home = null; }
    return { value: home, source: 'default', resolved: home };
  }

  // ---------------------------------------------------------------- fav_dirs
  // Pinned folders for the DirPicker + global FileViewer: ≤20 absolute,
  // existing directories, deduped. Existence is validated at SET time because a
  // favourite that isn't a directory is a broken chip; the READ path only
  // guards the JSON parse, so a corrupt row degrades to [] and never 500s the
  // snapshot (the guarded-parse precedent from snapshot.mjs's conflicts).
  function resolveFavDirs() {
    const raw = readSetting('fav_dirs');
    if (raw == null) return [];
    let parsed;
    try { parsed = JSON.parse(raw); } catch {
      console.error('fleetd settings: fav_dirs is corrupt JSON — serving []');
      return [];
    }
    return Array.isArray(parsed) ? parsed.filter(v => typeof v === 'string') : [];
  }

  function validateFavDirs(value) {
    if (value == null) return null;                 // clear
    if (!Array.isArray(value)) throw namedError(400, 'fav_dirs must be an array of absolute directory paths or null');
    if (value.length === 0) return null;            // [] clears
    const seen = new Set();
    const out = [];
    for (const entry of value) {
      if (typeof entry !== 'string' || !entry) throw namedError(400, 'each fav_dir must be a non-empty string');
      if (CONTROL_RE.test(entry)) throw namedError(400, 'a fav_dir must not contain NUL or control characters');
      // Absolute BEFORE resolve, like validatePathSetting: resolve() would
      // absolutize "." against the daemon's cwd and let it through.
      const expanded = expandHome(entry);
      if (!path.isAbsolute(expanded)) throw namedError(400, 'a fav_dir must be an absolute path (or begin with ~/)');
      const resolved = path.resolve(expanded);
      let isDir = false;
      try { isDir = fs.statSync(resolved).isDirectory(); } catch { isDir = false; }
      if (!isDir) throw namedError(400, `a fav_dir is not an existing directory — ${resolved}`);
      if (seen.has(resolved)) continue;             // dedupe on the resolved path
      seen.add(resolved);
      out.push(resolved);
    }
    if (out.length > FAV_DIRS_MAX) throw namedError(400, `fav_dirs must list ${FAV_DIRS_MAX} directories or fewer — got ${out.length}`);
    return out;
  }

  // Each key VALIDATES in prepare() (pure — may throw a 400, never writes) and
  // WRITES in commit(prepared). setSettings runs every prepare BEFORE any
  // commit, so a mixed body with one bad field writes nothing. `null` clears a
  // key everywhere (a null-valued row reads back as the default, exactly like
  // setReposDir's clear).
  const HANDLERS = {
    repos_dir: {
      // repos.mjs stays the SINGLE writer for the repos root; we pre-validate
      // with the shared gates so a bad repos_dir cannot slip past a valid
      // sibling key and half-apply a body.
      prepare: v => { if (v != null) validatePathSetting(v, 'repos_dir'); return v; },
      commit: v => setReposDir(v),
    },
    repo_transport: {
      prepare: v => {
        if (v != null && v !== 'ssh' && v !== 'https') {
          throw namedError(400, `repo_transport must be ssh or https — got ${JSON.stringify(v)}`);
        }
        return v;
      },
      commit: v => q.setSetting.run('repo_transport', v ?? null, Date.now()),
    },
    browse_root: {
      prepare: v => { if (v != null) validatePathSetting(v, 'browse_root'); return v; },
      commit: v => q.setSetting.run('browse_root', v ?? null, Date.now()),
    },
    fav_dirs: {
      prepare: v => validateFavDirs(v),             // → normalized array | null
      commit: prepared => q.setSetting.run('fav_dirs', prepared == null ? null : JSON.stringify(prepared), Date.now()),
    },
  };

  function setSettings(body) {
    if (!body || typeof body !== 'object' || Array.isArray(body)) {
      return { status: 400, body: { ok: false, reason: 'settings body must be a JSON object' } };
    }
    const keys = Object.keys(body);
    const unknown = keys.find(k => !ALLOWED_KEYS.includes(k));
    if (unknown) {
      return {
        status: 400,
        body: { ok: false, reason: `unknown setting "${unknown}" — allowed: ${ALLOWED_KEYS.join(', ')}` },
      };
    }
    try {
      // Validate every named key first…
      const prepared = keys.map(k => ({ k, value: HANDLERS[k].prepare(body[k]) }));
      // …then apply them all — nothing above wrote, so a throw here is impossible
      // to reach with a half-validated body.
      for (const { k, value } of prepared) HANDLERS[k].commit(value);
      onMutate();
      return { status: 200, body: { ok: true, settings: resolveSettings() } };
    } catch (err) {
      return { status: err.status || 400, body: { ok: false, reason: err.message || String(err) } };
    }
  }

  // D2: remember the last EXPLICIT transport choice, written daemon-side on an
  // accepted shorthand spawn (spawns.mjs) — covering curl users too, and NOT
  // pill clicks (those are exploratory). The value is pre-validated by spawns'
  // 400 gate; re-checked here as defence in depth so a bad value can never
  // reach the row. No onMutate — the caller batches its own broadcast.
  function persistRepoTransport(value) {
    if (value !== 'ssh' && value !== 'https') return;
    q.setSetting.run('repo_transport', value, Date.now());
  }

  // The whole settings object — GET /api/settings, the POST response, and the
  // /state snapshot all serve THIS shape (the shared board contract).
  function resolveSettings() {
    return {
      repos_dir: resolveReposDir(),
      repo_transport: resolveRepoTransport(),
      browse_root: browseRootChoice(),
      fav_dirs: resolveFavDirs(),
    };
  }

  return { setSettings, resolveSettings, browseRootChoice, persistRepoTransport };
}
