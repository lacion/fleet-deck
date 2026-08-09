// settings.ts — the daemon's durable, whitelisted settings surface. One place
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
// of the whitelist. One key breaks the replace-whole-value pattern on purpose:
// repo_setup_patch merges its entries into the stored repo_setup map (with
// "__delete" tombstones) at commit time, because the board's setup default is
// saved per repository and a blind whole-object replacement lets two
// concurrent boards each delete the other's save (BUG-147).
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
import { detectCoderWorkspaceRoot } from './config.ts';
import { resolveHoldMs } from './questions.ts';
import type { Statements } from './statements.ts';
import type { SqliteHandle } from './sqlite.ts';

// eslint-disable-next-line no-control-regex -- refusing NUL/C0/DEL in path & credential values is the entire purpose of this gate
const CONTROL_RE = /[\x00-\x1f\x7f]/;
// eslint-disable-next-line no-control-regex -- same gate for setup commands, but newline (\x0a) is allowed through on purpose
const SETUP_CONTROL_RE = /[\x00-\x09\x0b-\x1f\x7f]/;
const FAV_DIRS_MAX = 20;
const REPO_SETUP_MAX = 50;
const SETUP_CMD_MAX = 2000;
const ALLOWED_KEYS = [
  'repos_dir',
  'repo_transport',
  'repo_default_org',
  'browse_root',
  'fav_dirs',
  'repo_setup',
  'repo_setup_patch',
  'gateway_base_url',
  'gateway_auth_style',
  'gateway_token',
  'gateway_model_discovery',
  'gateway_default',
  'hold_ms',
];
// A gateway credential is long-lived and grants API spend, so the ceiling is
// generous but finite — an unbounded value would ride every /state frame's
// `token_set` computation and every spawn's tmux argv.
const GATEWAY_TOKEN_MAX = 4096;

// The resolved-choice shape the repos catalog hands us (repos.mjs) and the one
// this module's own resolvers return: a value plus where it came from.
interface SettingChoice {
  value: string | null;
  source: string;
}

// The dependency surface createSettings is handed by derive.mjs. The repos
// providers (resolve/set/validate) come from the repos catalog, which this
// settings layer rides on top of; `db` is the raw handle for the atomic commit
// path (absent in the derive/test autocommit contexts — see setSettings).
interface SettingsCtx {
  db: SqliteHandle | null;
  q: Statements['q'];
  onMutate: () => void;
  resolveReposDir: () => SettingChoice;
  setReposDir: (value: string | null) => void;
  resolveRepoDefaultOrg: () => SettingChoice;
  validateRepoDefaultOrg: (value: unknown) => string | null;
}

// A 400/5xx-tagged error: the status rides the thrown Error so setSettings can
// tell a validator's "your body is wrong" (400) from a storage failure (5xx).
class SettingError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

function namedError(status: number, message: string): SettingError {
  return new SettingError(status, message);
}

// useUnknownInCatchVariables makes a caught error `unknown`; these two read the
// status tag and message off it without assuming a shape (a thrown non-Error is
// still possible from deep in a driver).
function errStatus(err: unknown): number | undefined {
  if (typeof err === 'object' && err !== null && 'status' in err) {
    const status = (err as { status?: unknown }).status;
    if (typeof status === 'number') return status;
  }
  return undefined;
}

function errMessage(err: unknown): string {
  return err instanceof Error && err.message ? err.message : String(err);
}

function expandHome(value: string): string {
  if (value === '~') return os.homedir();
  if (value.startsWith('~/')) return path.join(os.homedir(), value.slice(2));
  return value;
}

// Mirror of setReposDir's path gates (repos.mjs:273-295), factored so
// browse_root and (defensively) repos_dir validate identically. Returns the
// resolved absolute path or throws a 400 naming `label`; null is the caller's
// to interpret (clear) and never reaches here.
function validatePathSetting(value: unknown, label: string): string {
  if (typeof value !== 'string' || !value)
    throw namedError(400, `${label} must be an absolute path or null`);
  if (CONTROL_RE.test(value))
    throw namedError(400, `${label} must not contain NUL or control characters`);
  // The value ITSELF (post ~ expansion) must be absolute, checked BEFORE
  // path.resolve — resolve() absolutizes ANY relative string against the
  // daemon's cwd, so isAbsolute(resolved) was a tautology and "." would have
  // validated and persisted as a cwd-dependent root.
  const expanded = expandHome(value);
  if (!path.isAbsolute(expanded))
    throw namedError(400, `${label} must be an absolute path (or begin with ~/)`);
  const resolved = path.resolve(expanded);
  if (path.dirname(resolved) === resolved)
    throw namedError(400, `${label} must not be the filesystem root`);
  try {
    // Follow symlinks and refuse an existing non-directory up front, so a file
    // (or a symlink to one) fails here rather than as a confusing browse/clone
    // error later.
    if (fs.existsSync(resolved) && !fs.statSync(resolved).isDirectory()) {
      throw namedError(400, `${label} points to an existing file`);
    }
  } catch (err) {
    if (errStatus(err)) throw err;
    throw namedError(400, `cannot inspect ${label}: ${errMessage(err)}`);
  }
  // The root ban above is lexical only — an alias like /proc/self/root passes
  // it and realpaths to /. Refuse when the CANONICAL path is the filesystem
  // root too. Best-effort: a not-yet-existing path has no realpath, and the
  // lexical ban already covered the literal spelling.
  try {
    const canonical = fs.realpathSync(resolved);
    if (path.dirname(canonical) === canonical)
      throw namedError(400, `${label} must not be the filesystem root`);
  } catch (err) {
    if (errStatus(err)) throw err;
    /* ENOENT and friends — nothing further to prove about a missing path */
  }
  return resolved;
}

// prepare() VALIDATES a key's incoming value (pure — may throw a 400, never
// writes) and returns the shape commit() will persist; commit(prepared) does
// the WRITE. T threads prepare's output into commit's input so the two stay in
// lockstep per key. The method-syntax declaration is deliberate: it lets a
// concrete SettingHandler<string[]> widen to SettingHandler<unknown> at the
// dynamic dispatch site (setSettings) via method-parameter bivariance — the
// per-key bodies below keep their precise types.
interface SettingHandler<T> {
  prepare(value: unknown): T;
  commit(prepared: T): void;
}

// Identity helper: fixes each handler's T from its literal so the bodies see a
// concrete prepared type, without an outer annotation forcing them to `unknown`.
const defineHandler = <T>(handler: SettingHandler<T>): SettingHandler<T> => handler;

export function createSettings(ctx: SettingsCtx) {
  const {
    db,
    q,
    onMutate,
    resolveReposDir,
    setReposDir,
    resolveRepoDefaultOrg,
    validateRepoDefaultOrg,
  } = ctx;

  function readSetting(key: string): string | null {
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
  function browseRootChoice(): {
    value: string | null;
    source: 'override' | 'env' | 'detected' | 'default';
    resolved: string | null;
  } {
    const setting = readSetting('browse_root');
    if (setting != null) {
      return { value: setting, source: 'override', resolved: path.resolve(expandHome(setting)) };
    }
    const env = process.env['FLEETDECK_BROWSE_ROOT'];
    if (env) {
      return { value: env, source: 'env', resolved: path.resolve(expandHome(env)) };
    }
    const detected = detectCoderWorkspaceRoot();
    if (detected) {
      return { value: detected, source: 'detected', resolved: detected };
    }
    let home: string | null = null;
    try {
      home = os.homedir();
    } catch {
      /* home stays null — no resolvable default root */
    }
    return { value: home, source: 'default', resolved: home };
  }

  // ---------------------------------------------------------------- fav_dirs
  // Pinned folders for the DirPicker + global FileViewer: ≤20 absolute,
  // existing directories, deduped. Existence is validated at SET time because a
  // favourite that isn't a directory is a broken chip; the READ path only
  // guards the JSON parse, so a corrupt row degrades to [] and never 500s the
  // snapshot (the guarded-parse precedent from snapshot.mjs's conflicts).
  function resolveFavDirs(): string[] {
    const raw = readSetting('fav_dirs');
    if (raw == null) return [];
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      console.error('fleetd settings: fav_dirs is corrupt JSON — serving []');
      return [];
    }
    return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === 'string') : [];
  }

  function validateFavDirs(value: unknown): string[] | null {
    if (value == null) return null; // clear
    if (!Array.isArray(value))
      throw namedError(400, 'fav_dirs must be an array of absolute directory paths or null');
    if (value.length === 0) return null; // [] clears
    const seen = new Set<string>();
    const out: string[] = [];
    for (const entry of value) {
      if (typeof entry !== 'string' || !entry)
        throw namedError(400, 'each fav_dir must be a non-empty string');
      if (CONTROL_RE.test(entry))
        throw namedError(400, 'a fav_dir must not contain NUL or control characters');
      // Absolute BEFORE resolve, like validatePathSetting: resolve() would
      // absolutize "." against the daemon's cwd and let it through.
      const expanded = expandHome(entry);
      if (!path.isAbsolute(expanded))
        throw namedError(400, 'a fav_dir must be an absolute path (or begin with ~/)');
      const resolved = path.resolve(expanded);
      let isDir = false;
      try {
        isDir = fs.statSync(resolved).isDirectory();
      } catch {
        /* isDir stays false — missing path or stat failure is "not a dir" */
      }
      if (!isDir) throw namedError(400, `a fav_dir is not an existing directory — ${resolved}`);
      if (seen.has(resolved)) continue; // dedupe on the resolved path
      seen.add(resolved);
      out.push(resolved);
    }
    if (out.length > FAV_DIRS_MAX)
      throw namedError(
        400,
        `fav_dirs must list ${FAV_DIRS_MAX} directories or fewer — got ${out.length}`,
      );
    return out;
  }

  // Repo-name → visible POSIX-sh setup command. This is only a board prefill;
  // the daemon never applies it implicitly to a spawn.
  function resolveRepoSetup(): Record<string, string> {
    const raw = readSetting('repo_setup');
    if (raw == null) return {};
    try {
      const parsed: unknown = JSON.parse(raw);
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {};
      return Object.fromEntries(
        Object.entries(parsed as Record<string, unknown>).filter(
          (e): e is [string, string] => typeof e[0] === 'string' && typeof e[1] === 'string',
        ),
      );
    } catch {
      console.error('fleetd settings: repo_setup is corrupt JSON — serving {}');
      return {};
    }
  }

  function validateRepoSetup(value: unknown): Record<string, string> | null {
    if (value == null) return null;
    if (typeof value !== 'object' || Array.isArray(value)) {
      throw namedError(400, 'repo_setup must be an object mapping repo names to commands or null');
    }
    return validateRepoSetupEntries(Object.entries(value as Record<string, unknown>));
  }

  function validateRepoSetupEntries(entries: [string, unknown][]): Record<string, string> {
    if (entries.length > REPO_SETUP_MAX) {
      throw namedError(
        400,
        `repo_setup must contain ${REPO_SETUP_MAX} entries or fewer — got ${entries.length}`,
      );
    }
    // Accumulate into entries, not `out[name] = cmd` on a plain object — a
    // legitimate repo named "__proto__" would otherwise hit the inherited
    // prototype setter, mutate the accumulator's prototype, and serialize
    // back to {} while the API still reports 200.
    const out: [string, string][] = [];
    for (const [name, cmd] of entries) {
      if (!name || CONTROL_RE.test(name)) {
        throw namedError(
          400,
          'repo_setup keys must be non-empty repo names without control characters',
        );
      }
      if (typeof cmd !== 'string') {
        throw namedError(400, `repo_setup command for "${name}" must be a string`);
      }
      if (cmd.length > SETUP_CMD_MAX) {
        throw namedError(
          400,
          `repo_setup command for "${name}" must be ${SETUP_CMD_MAX} characters or fewer — got ${cmd.length}`,
        );
      }
      if (SETUP_CONTROL_RE.test(cmd)) {
        throw namedError(
          400,
          `repo_setup command for "${name}" must not contain NUL or control characters other than newline`,
        );
      }
      out.push([name, cmd]);
    }
    return Object.fromEntries(out);
  }

  // A PATCH is the save one board can make without clobbering another's: it
  // merges into the stored map at commit time instead of replacing it, so two
  // concurrent per-repository saves both survive (BUG-147). Validation mirrors
  // the whole-object handler: `null` (or {}, which validates to nothing)
  // clears, a string entry upserts, and `__delete` is the patch-only tombstone —
  // a value validateRepoSetupEntries never emits, so no whole-object client can
  // accidentally delete, and a NAME whose string value is the sentinel deletes
  // by construction (the last writer leaves a tombstone, not a setup command).
  function validateRepoSetupPatch(value: unknown): Record<string, string> | null {
    if (value == null) return null;
    if (typeof value !== 'object' || Array.isArray(value)) {
      throw namedError(
        400,
        'repo_setup_patch must be an object mapping repo names to commands, "__delete" entries, or null',
      );
    }
    const out: Record<string, string> = {};
    for (const [name, cmd] of Object.entries(value as Record<string, unknown>)) {
      if (cmd === '__delete') {
        if (!name || CONTROL_RE.test(name)) {
          throw namedError(
            400,
            'repo_setup_patch keys must be non-empty repo names without control characters',
          );
        }
        out[name] = cmd;
        continue;
      }
      Object.assign(out, validateRepoSetupEntries([[name, cmd]]));
    }
    return out;
  }

  // ---------------------------------------------------------------- hold_ms
  // UX 2.1: the question hold window as a first-class setting, tunable without
  // an env var. Two readers, two shapes: the SETTINGS VIEW resolves it
  // (resolveHoldMsSetting → resolveSettings, the /state broadcast); the
  // QUESTIONS RELAY re-reads the raw row per hold creation (resolveHoldMsRaw,
  // handed to resolveHoldMs as its fallback) so a changed value steers NEW
  // holds immediately — live holds keep the window they parked with, same as
  // FLEETDECK_HOLD_MS always behaved. The env var stays the override, and
  // resolveHoldMs (questions.mjs) owns the clamp ([250, 650_000], under the
  // shim watchdog; the lockstep invariant lives at the definition).
  function validateHoldMs(value: unknown): string | null {
    if (value == null) return null;
    if (typeof value !== 'number' || !Number.isFinite(value)) {
      throw namedError(400, 'hold_ms must be a number of milliseconds or null');
    }
    return String(Math.trunc(value)); // clamping happens at resolve time
  }

  function resolveHoldMsRaw(): string | null {
    return readSetting('hold_ms');
  }

  function resolveHoldMsSetting() {
    const env = process.env['FLEETDECK_HOLD_MS'];
    if (Number.isFinite(Number(env)) && Number(env) > 0) {
      return { value: resolveHoldMs(), source: 'env' };
    }
    const stored = readSetting('hold_ms');
    if (stored != null) return { value: resolveHoldMs({}, resolveHoldMsRaw), source: 'override' };
    return { value: resolveHoldMs({}), source: 'default' };
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
  function validateGatewayBaseUrl(value: unknown): string {
    if (typeof value !== 'string' || !value)
      throw namedError(400, 'gateway_base_url must be a URL or null');
    if (CONTROL_RE.test(value))
      throw namedError(400, 'gateway_base_url must not contain NUL or control characters');
    let url: URL;
    // Deliberately does NOT echo `value`: a human who pastes a credential into
    // the wrong field must not have it reflected back in an error string that
    // may be logged or rendered.
    try {
      url = new URL(value);
    } catch {
      throw namedError(400, 'gateway_base_url is not a valid URL');
    }
    if (url.protocol !== 'http:' && url.protocol !== 'https:') {
      throw namedError(400, `gateway_base_url must be http:// or https:// — got ${url.protocol}//`);
    }
    if (url.username || url.password) {
      throw namedError(
        400,
        'gateway_base_url must not embed credentials (user:password@) — put the credential in gateway_token, which is never served back to a client',
      );
    }
    if (url.search) {
      throw namedError(
        400,
        'gateway_base_url must not carry a query string — it would be broadcast to every board; put a credential in gateway_token instead',
      );
    }
    if (url.hash) throw namedError(400, 'gateway_base_url must not carry a fragment');
    // Trailing slashes are cosmetic to the client but make the /state view and
    // the injected env disagree on spelling; normalize once, at the door.
    return url.href.replace(/\/+$/, '');
  }

  function validateGatewayToken(value: unknown): string {
    if (typeof value !== 'string' || !value)
      throw namedError(400, 'gateway_token must be a non-empty string or null');
    // Control characters would be smuggled into an HTTP header value; a
    // credential never legitimately contains them.
    if (CONTROL_RE.test(value))
      throw namedError(400, 'gateway_token must not contain NUL or control characters');
    if (value.length > GATEWAY_TOKEN_MAX) {
      throw namedError(
        400,
        `gateway_token must be ${GATEWAY_TOKEN_MAX} characters or fewer — got ${value.length}`,
      );
    }
    return value;
  }

  // Booleans persist as '1'/'0' so a cleared row and an explicit false stay
  // distinguishable from a never-set one at the SQL layer.
  function validateGatewayBool(value: unknown, label: string): string {
    if (typeof value !== 'boolean') throw namedError(400, `${label} must be a boolean or null`);
    return value ? '1' : '0';
  }

  function readGatewayBool(key: string, fallback: boolean): boolean {
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
  function resolveGatewayEnv(): Record<string, string> | null {
    const base_url = readSetting('gateway_base_url');
    const token = readSetting('gateway_token');
    if (!base_url || !token) return null;
    const auth_style = readSetting('gateway_auth_style') === 'api-key' ? 'api-key' : 'bearer';
    const env: Record<string, string> = { ANTHROPIC_BASE_URL: base_url };
    env[auth_style === 'api-key' ? 'ANTHROPIC_API_KEY' : 'ANTHROPIC_AUTH_TOKEN'] = token;
    if (readGatewayBool('gateway_model_discovery', true)) {
      env['CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY'] = '1';
    }
    return env;
  }

  // Each key VALIDATES in prepare() (pure — may throw a 400, never writes) and
  // WRITES in commit(prepared). setSettings runs every prepare BEFORE any
  // commit, so a mixed body with one bad field writes nothing. `null` clears a
  // key everywhere (a null-valued row reads back as the default, exactly like
  // setReposDir's clear).
  const HANDLERS = {
    repos_dir: defineHandler<string | null>({
      // repos.mjs stays the SINGLE writer for the repos root; we pre-validate
      // with the shared gates so a bad repos_dir cannot slip past a valid
      // sibling key and half-apply a body.
      prepare: (v) => {
        if (v != null) validatePathSetting(v, 'repos_dir');
        return v as string | null; // validated (or null) above; persist the raw value, not the resolved one
      },
      commit: (v) => {
        setReposDir(v);
      },
    }),
    repo_transport: defineHandler<string | null>({
      prepare: (v) => {
        if (v != null && v !== 'ssh' && v !== 'https') {
          throw namedError(400, `repo_transport must be ssh or https — got ${JSON.stringify(v)}`);
        }
        return v as string | null;
      },
      commit: (v) => q.setSetting.run('repo_transport', v ?? null, Date.now()),
    }),
    repo_default_org: defineHandler<string | null>({
      prepare: (v) => validateRepoDefaultOrg(v),
      commit: (v) => q.setSetting.run('repo_default_org', v ?? null, Date.now()),
    }),
    browse_root: defineHandler<string | null>({
      prepare: (v) => {
        if (v != null) validatePathSetting(v, 'browse_root');
        return v as string | null;
      },
      commit: (v) => q.setSetting.run('browse_root', v ?? null, Date.now()),
    }),
    fav_dirs: defineHandler<string[] | null>({
      prepare: (v) => validateFavDirs(v), // → normalized array | null
      commit: (prepared) =>
        q.setSetting.run(
          'fav_dirs',
          prepared == null ? null : JSON.stringify(prepared),
          Date.now(),
        ),
    }),
    repo_setup: defineHandler<Record<string, string> | null>({
      prepare: (v) => validateRepoSetup(v),
      commit: (prepared) =>
        q.setSetting.run(
          'repo_setup',
          prepared == null ? null : JSON.stringify(prepared),
          Date.now(),
        ),
    }),
    repo_setup_patch: defineHandler<Record<string, string> | null>({
      prepare: (v) => validateRepoSetupPatch(v),
      commit: (prepared) => {
        if (prepared == null || Object.keys(prepared).length === 0) {
          q.setSetting.run('repo_setup', null, Date.now());
          return;
        }
        const merged = resolveRepoSetup();
        for (const [name, cmd] of Object.entries(prepared)) {
          // eslint-disable-next-line @typescript-eslint/no-dynamic-delete -- a "__delete" tombstone removes exactly the keyed repo from the merged map; that dynamic key is the whole point of the patch
          if (cmd === '__delete') delete merged[name];
          else merged[name] = cmd;
        }
        q.setSetting.run(
          'repo_setup',
          Object.keys(merged).length === 0 ? null : JSON.stringify(merged),
          Date.now(),
        );
      },
    }),
    hold_ms: defineHandler<string | null>({
      prepare: (v) => validateHoldMs(v),
      commit: (v) => q.setSetting.run('hold_ms', v ?? null, Date.now()),
    }),
    gateway_base_url: defineHandler<string | null>({
      prepare: (v) => (v == null ? null : validateGatewayBaseUrl(v)),
      commit: (v) => q.setSetting.run('gateway_base_url', v ?? null, Date.now()),
    }),
    gateway_auth_style: defineHandler<string | null>({
      prepare: (v) => {
        if (v != null && v !== 'bearer' && v !== 'api-key') {
          throw namedError(
            400,
            `gateway_auth_style must be bearer or api-key — got ${JSON.stringify(v)}`,
          );
        }
        return v as string | null;
      },
      commit: (v) => q.setSetting.run('gateway_auth_style', v ?? null, Date.now()),
    }),
    gateway_token: defineHandler<string | null>({
      prepare: (v) => (v == null ? null : validateGatewayToken(v)),
      commit: (v) => q.setSetting.run('gateway_token', v ?? null, Date.now()),
    }),
    gateway_model_discovery: defineHandler<string | null>({
      prepare: (v) => (v == null ? null : validateGatewayBool(v, 'gateway_model_discovery')),
      commit: (v) => q.setSetting.run('gateway_model_discovery', v ?? null, Date.now()),
    }),
    gateway_default: defineHandler<string | null>({
      prepare: (v) => (v == null ? null : validateGatewayBool(v, 'gateway_default')),
      commit: (v) => q.setSetting.run('gateway_default', v ?? null, Date.now()),
    }),
  };

  // Dispatch view of HANDLERS: indexing by a runtime string needs the widened
  // shape (method-parameter bivariance makes each SettingHandler<T> a
  // SettingHandler<unknown> here). noUncheckedIndexedAccess adds `| undefined`,
  // so callers guard — though setSettings' whitelist check already proved the
  // key is present.
  const handlerFor = (key: string): SettingHandler<unknown> | undefined =>
    (HANDLERS as Record<string, SettingHandler<unknown>>)[key];

  function setSettings(body: unknown): { status: number; body: Record<string, unknown> } {
    if (!body || typeof body !== 'object' || Array.isArray(body)) {
      return { status: 400, body: { ok: false, reason: 'settings body must be a JSON object' } };
    }
    const record = body as Record<string, unknown>;
    const keys = Object.keys(record);
    const unknown = keys.find((k) => !ALLOWED_KEYS.includes(k));
    if (unknown) {
      return {
        status: 400,
        body: {
          ok: false,
          reason: `unknown setting "${unknown}" — allowed: ${ALLOWED_KEYS.join(', ')}`,
        },
      };
    }
    try {
      // Validate every named key first…
      const prepared = keys.map((k) => {
        const handler = handlerFor(k);
        if (!handler) throw namedError(400, `unknown setting "${k}"`); // unreachable: whitelisted above
        return { handler, value: handler.prepare(record[k]) };
      });
      // …then apply them all — nothing above wrote, so a validation throw here is
      // impossible to reach with a half-validated body. (Derive/test contexts
      // without a raw `db` handle keep the old autocommit path — the daemon's
      // ctx always carries one.)
      if (!db) {
        for (const { handler, value } of prepared) handler.commit(value);
        onMutate();
        return { status: 200, body: { ok: true, settings: resolveSettings() } };
      }
      // Validation alone is not atomicity, though: each commit was an
      // independent autocommit, so a LATER write failing (SQLITE_BUSY/FULL on a
      // later key, an onMutate() throw) used to return the error below with the
      // EARLIER keys already durable — the caller got an error AND a
      // half-applied body that still changed repository/browser/gateway
      // behavior, worst case a new gateway_base_url combined with the previous
      // gateway_token, pointing a live credential at the wrong host. A throw
      // below is therefore a STORAGE or callback failure, not a caller mistake
      // (5xx, not 400). One IMMEDIATE transaction around the commits + the
      // callback closes that: any failure rolls back every write, so the
      // returned error is the truth.
      db.exec('BEGIN IMMEDIATE');
      try {
        for (const { handler, value } of prepared) handler.commit(value);
        onMutate();
        db.exec('COMMIT');
      } catch (err) {
        try {
          db.exec('ROLLBACK');
        } catch {
          /* preserve the original error */
        }
        throw err;
      }
      return { status: 200, body: { ok: true, settings: resolveSettings() } };
    } catch (err) {
      // A thrown 400 is a validator rejecting the caller's body; anything
      // untagged is a storage failure the caller cannot fix by editing fields.
      const status = errStatus(err) ?? 500;
      return { status, body: { ok: false, reason: errMessage(err) } };
    }
  }

  // D2: remember the last EXPLICIT transport choice, written daemon-side on an
  // accepted shorthand spawn (spawns.mjs) — covering curl users too, and NOT
  // pill clicks (those are exploratory). The value is pre-validated by spawns'
  // 400 gate; re-checked here as defence in depth so a bad value can never
  // reach the row. No onMutate — the caller batches its own broadcast.
  function persistRepoTransport(value: unknown): void {
    if (value !== 'ssh' && value !== 'https') return;
    q.setSetting.run('repo_transport', value, Date.now());
  }

  // Same accepted-spawn persistence as transport: a board/curl caller may send
  // repo_org explicitly with a bare repo so that spawn is deterministic even if
  // an onBlur settings save is still in flight. Once accepted, that explicit
  // choice becomes the durable default for the next bare name.
  function persistRepoDefaultOrg(value: unknown): void {
    if (value == null) return;
    let org: string | null;
    try {
      org = validateRepoDefaultOrg(value);
    } catch {
      return;
    }
    q.setSetting.run('repo_default_org', org, Date.now());
  }

  // Daemon-side single-repo variant of repo_setup_patch — same read-merge-write,
  // no broadcast (the caller batches its own), for the spawn path to remember a
  // setup default without clobbering a concurrent board save (BUG-147).
  function setRepoSetupEntry(name: string, cmd: string | null | undefined): void {
    let prepared: Record<string, string> | null;
    try {
      prepared = validateRepoSetupPatch({ [name]: cmd ?? '__delete' });
    } catch {
      return;
    }
    HANDLERS.repo_setup_patch.commit(prepared);
  }

  // The whole settings object — GET /api/settings, the POST response, and the
  // /state snapshot all serve THIS shape (the shared board contract).
  function resolveSettings() {
    return {
      repos_dir: resolveReposDir(),
      repo_transport: resolveRepoTransport(),
      repo_default_org: resolveRepoDefaultOrg(),
      browse_root: browseRootChoice(),
      fav_dirs: resolveFavDirs(),
      repo_setup: resolveRepoSetup(),
      hold_ms: resolveHoldMsSetting(),
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
    setSettings,
    resolveSettings,
    browseRootChoice,
    persistRepoTransport,
    persistRepoDefaultOrg,
    resolveGateway,
    resolveGatewayEnv,
    resolveHoldMsRaw,
    setRepoSetupEntry,
  };
}
