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
//
// 0.15.0 adds a fifth preference that breaks the pattern above in one way worth
// stating up front: the LLM-gateway profile (gateway_*) holds a CREDENTIAL.
// Every other setting here is safe to broadcast, and resolveSettings() is
// broadcast — it rides the /state snapshot to every board client, phones on LAN
// mode included. So the gateway keys are asymmetric by design: they WRITE like
// the others, and they READ through two different doors — resolveGateway() for
// clients (masked; `token_set` is the whole truth it tells) and
// resolveGatewayEnv() for the spawn path (unmasked, one caller, never
// serialized). See the block comment above validateGatewayBaseUrl.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { detectCoderWorkspaceRoot } from './config.mjs';

const CONTROL_RE = /[\x00-\x1f\x7f]/;
const FAV_DIRS_MAX = 20;
const ALLOWED_KEYS = [
  'repos_dir', 'repo_transport', 'browse_root', 'fav_dirs',
  'gateway_base_url', 'gateway_auth_style', 'gateway_token',
  'gateway_model_discovery', 'gateway_default',
];
// A gateway credential is long-lived and grants API spend, so the ceiling is
// generous but finite — an unbounded value would ride every /state frame's
// `token_set` computation and every spawn's tmux argv.
const GATEWAY_TOKEN_MAX = 4096;

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

  // ----------------------------------------------------------------- gateway
  // The LLM-gateway profile (0.15.0): where a `gateway:true` spawn sends its
  // API traffic instead of Anthropic — a local CLIProxyAPI, a corporate
  // gateway, anything that speaks the Anthropic wire format.
  //
  // SECURITY, and the reason this key group is not shaped like the four above:
  // `gateway_token` is a live credential, and resolveSettings() is not a private
  // reply. It rides the /state snapshot (snapshot.mjs:167) to EVERY connected
  // board — including a phone over LAN mode, and including whatever a reverse
  // proxy fronts. So the token has exactly one reader, resolveGatewayEnv(), on
  // the spawn path; the settings view serves `token_set: true` and nothing more.
  // A masked-tail preview was considered and rejected: it leaks credential
  // length and prefix to every board client for no operational gain — "is one
  // configured" is the only question the UI actually asks.

  // http/https only. A gateway is a URL the daemon hands to a child process, so
  // the scheme wall is what stops `file:`, `data:` and friends from ever
  // reaching it. Cleartext http is ALLOWED and deliberately not warned about
  // here: the overwhelmingly common case is 127.0.0.1, where TLS buys nothing,
  // and refusing it would lock out every local proxy this feature exists for.
  //
  // SECURITY — why userinfo and query strings are REFUSED rather than accepted
  // and masked: unlike gateway_token, this value is deliberately NOT secret. It
  // is served raw by resolveGateway(), rides the broadcast /state snapshot, and
  // is rendered into the spawn form so a human can see where their session is
  // going. `https://user:pass@gw` and `https://gw/?api_key=…` are both ordinary
  // proxy spellings that would smuggle a credential down that public path — and
  // `new URL().href` preserves both, so normalization does not save us. There is
  // a field for credentials and this is not it; refusing at the door keeps the
  // one masked value (gateway_token) the only secret in the profile, which is
  // the property the whole settings split rests on.
  function validateGatewayBaseUrl(value) {
    if (typeof value !== 'string' || !value) throw namedError(400, 'gateway_base_url must be a URL or null');
    if (CONTROL_RE.test(value)) throw namedError(400, 'gateway_base_url must not contain NUL or control characters');
    let url;
    // Deliberately does NOT echo `value`: a human who pastes a credential into
    // the wrong field must not have it reflected back in an error string that
    // may be logged or rendered.
    try { url = new URL(value); } catch { throw namedError(400, 'gateway_base_url is not a valid URL'); }
    if (url.protocol !== 'http:' && url.protocol !== 'https:') {
      throw namedError(400, `gateway_base_url must be http:// or https:// — got ${url.protocol}//`);
    }
    if (url.username || url.password) {
      throw namedError(400, 'gateway_base_url must not embed credentials (user:password@) — put the credential in gateway_token, which is never served back to a client');
    }
    if (url.search) {
      throw namedError(400, 'gateway_base_url must not carry a query string — it would be broadcast to every board; put a credential in gateway_token instead');
    }
    if (url.hash) throw namedError(400, 'gateway_base_url must not carry a fragment');
    // Trailing slashes are cosmetic to the client but make the /state view and
    // the injected env disagree on spelling; normalize once, at the door.
    return url.href.replace(/\/+$/, '');
  }

  function validateGatewayToken(value) {
    if (typeof value !== 'string' || !value) throw namedError(400, 'gateway_token must be a non-empty string or null');
    // Control characters would be smuggled into an HTTP header value; a
    // credential never legitimately contains them.
    if (CONTROL_RE.test(value)) throw namedError(400, 'gateway_token must not contain NUL or control characters');
    if (value.length > GATEWAY_TOKEN_MAX) {
      throw namedError(400, `gateway_token must be ${GATEWAY_TOKEN_MAX} characters or fewer — got ${value.length}`);
    }
    return value;
  }

  // Booleans persist as '1'/'0' so a cleared row and an explicit false stay
  // distinguishable from a never-set one at the SQL layer.
  function validateGatewayBool(value, label) {
    if (typeof value !== 'boolean') throw namedError(400, `${label} must be a boolean or null`);
    return value ? '1' : '0';
  }

  function readGatewayBool(key, fallback) {
    const raw = readSetting(key);
    if (raw == null) return fallback;
    return raw === '1';
  }

  // The MASKED view — the only gateway shape that leaves this module for a
  // client. `ready` is what the board gates its toggle on: a base_url with no
  // token (or the reverse) is a half-configured profile that would fail at the
  // pane with a 401, so it is not offered as spawnable.
  function resolveGateway() {
    const base_url = readSetting('gateway_base_url');
    const auth_style = readSetting('gateway_auth_style') === 'api-key' ? 'api-key' : 'bearer';
    const token_set = readSetting('gateway_token') != null;
    return {
      base_url,
      auth_style,
      token_set,
      // Gateways commonly serve model names Claude Code doesn't ship in its
      // built-in list, and without discovery those are simply absent from the
      // /model picker — so this defaults ON.
      model_discovery: readGatewayBool('gateway_model_discovery', true),
      // When true, a spawn that says nothing about `gateway` routes through it
      // anyway. Off by default: silently rerouting billing is the failure mode
      // this whole feature exists to make explicit.
      default: readGatewayBool('gateway_default', false),
      ready: !!base_url && token_set,
    };
  }

  // The UNMASKED env map — spawn.mjs's tmux `-e` payload. Returns null when the
  // profile is not fully configured, so a caller can never half-apply one (a
  // base_url with no credential routes traffic somewhere that will 401, which
  // looks like a Claude Code bug rather than a settings mistake).
  //
  // The credential variable is chosen by auth_style because Claude Code sends
  // the two in DIFFERENT headers — ANTHROPIC_AUTH_TOKEN as `Authorization:
  // Bearer …`, ANTHROPIC_API_KEY as `x-api-key`. A credential in the wrong one
  // reaches the gateway in a header it does not read and fails 401; 'bearer' is
  // the default because it is what Anthropic's own gateway guidance recommends
  // when the operator didn't say, and what CLIProxyAPI's `api-keys` list wants.
  function resolveGatewayEnv() {
    const base_url = readSetting('gateway_base_url');
    const token = readSetting('gateway_token');
    if (!base_url || !token) return null;
    const auth_style = readSetting('gateway_auth_style') === 'api-key' ? 'api-key' : 'bearer';
    const env = { ANTHROPIC_BASE_URL: base_url };
    env[auth_style === 'api-key' ? 'ANTHROPIC_API_KEY' : 'ANTHROPIC_AUTH_TOKEN'] = token;
    if (readGatewayBool('gateway_model_discovery', true)) {
      env.CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY = '1';
    }
    return env;
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
    gateway_base_url: {
      prepare: v => (v == null ? null : validateGatewayBaseUrl(v)),
      commit: v => q.setSetting.run('gateway_base_url', v ?? null, Date.now()),
    },
    gateway_auth_style: {
      prepare: v => {
        if (v != null && v !== 'bearer' && v !== 'api-key') {
          throw namedError(400, `gateway_auth_style must be bearer or api-key — got ${JSON.stringify(v)}`);
        }
        return v;
      },
      commit: v => q.setSetting.run('gateway_auth_style', v ?? null, Date.now()),
    },
    gateway_token: {
      prepare: v => (v == null ? null : validateGatewayToken(v)),
      commit: v => q.setSetting.run('gateway_token', v ?? null, Date.now()),
    },
    gateway_model_discovery: {
      prepare: v => (v == null ? null : validateGatewayBool(v, 'gateway_model_discovery')),
      commit: v => q.setSetting.run('gateway_model_discovery', v ?? null, Date.now()),
    },
    gateway_default: {
      prepare: v => (v == null ? null : validateGatewayBool(v, 'gateway_default')),
      commit: v => q.setSetting.run('gateway_default', v ?? null, Date.now()),
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
      // MASKED by construction — see resolveGateway. Never inline the raw
      // gateway_token row here: this object is broadcast, not returned.
      gateway: resolveGateway(),
    };
  }

  // resolveGatewayEnv is deliberately NOT part of resolveSettings and has
  // exactly one caller (spawns.mjs, on the launch path). Keeping it a separate
  // export is the structural reason a future edit to the settings view cannot
  // accidentally start serializing the credential.
  return {
    setSettings, resolveSettings, browseRootChoice, persistRepoTransport,
    resolveGateway, resolveGatewayEnv,
  };
}
