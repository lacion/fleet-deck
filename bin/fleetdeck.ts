#!/usr/bin/env bun
// fleetdeck — the standalone CLI.
//
// Fleet Deck has always been a Claude Code plugin whose daemon is booted, lazily
// and detached, by a SessionStart hook. That works beautifully on a laptop and
// not at all on a remote dev box, where there may be no Claude Code session at
// all and the only way in is a browser. This CLI is the other way to run the
// SAME daemon: as a supervised, always-on service.
//
// It deliberately does NOT reimplement fleetd. `serve` imports the very bundle
// the plugin ships, so there is exactly one daemon implementation and CI's
// bundle drift gate keeps covering both entry points.
//
// Node builtins only. Nothing the published package SHIPS imports anything from
// node_modules: `files` carries the bundle, not the source, and esbuild has
// already inlined `ws` into it (SQLite is the `node:sqlite` builtin). That is
// what makes `npm i -g fleetdeck` a reasonable thing to put in a container image.
//
// NOTE: `ws` remains declared in package.json `dependencies` because the SOURCE
// daemon (npm start, and the test suite) imports it. A global install therefore
// pulls it in even though the bundle already inlines it. That is a deliberate,
// revisitable choice, not an oversight: reclassifying it to devDependencies would
// make the published package literally dependency-free, at the cost of breaking
// anyone who runs the daemon from source after `npm ci --omit=dev`.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFile, spawn } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { promisify } from 'node:util';
import { MIN_TMUX_VERSION, parseTmuxVersion, tmuxVersionSupported } from './tmux-version.ts';
import { livePidLooksLikeFleetd, pidRecord, verifyDaemonPid } from '../src/daemon/takeover.ts';

const execFileP = promisify(execFile);
const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..');

// --- types (local; no runtime shape) --------------------------------------
// The /health response, as consumed here. These fields are always present in
// our own daemon's response; health() casts the parsed JSON to this shape.
interface SpawnHealth {
  available: boolean;
  active: number;
  reason?: string;
}
interface Health {
  version: string;
  managed: boolean;
  pid: number;
  port?: number;
  fleet: number;
  spawn?: SpawnHealth;
}
// Node throws ErrnoException objects (a `.code` string) for fs/spawn failures;
// narrow an `unknown` catch value to that code without asserting the whole shape.
function errnoCode(e: unknown): string | undefined {
  return e instanceof Error && typeof (e as NodeJS.ErrnoException).code === 'string'
    ? (e as NodeJS.ErrnoException).code
    : undefined;
}

// Same resolution order as the SessionStart hook: the committed bundle is the
// production artifact; source is the dev-checkout fallback.
const BUNDLE = path.join(ROOT, 'src', 'daemon', 'fleetd.bundle.mjs');
const SOURCE = path.join(ROOT, 'src', 'daemon', 'fleetd.ts');
const FLEETD = fs.existsSync(BUNDLE) ? BUNDLE : SOURCE;

// eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing -- an empty FLEETDECK_HOME must fall back to the default, not be kept as ''
const HOME = process.env['FLEETDECK_HOME'] || path.join(os.homedir() || '/tmp', '.fleetdeck');
// Byte-identical to scripts/fleetd/config.mjs resolvePort (that module is not
// importable from the published CLI — see the header comment there): reject
// port 0 and every other non-1..65535 value so the CLI never targets a
// daemon identity nothing can actually reach. An explicit ambient
// FLEETDECK_PORT still wins; when it is unset we fall back to the port frozen
// into the installed service.env (BUG-075), and only then to the default.
// NOTE: resolveCliPort is only CALLED after serviceEnvPort/ENV_FILE below are
// initialized — moving the call above their const declarations would hit the
// temporal dead zone.
function resolveCliPort(): number {
  const raw = process.env['FLEETDECK_PORT'];
  if (raw === undefined || raw === '') return serviceEnvPort() ?? 4711;
  const port = Number(raw);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    process.stderr.write(
      `fleetdeck: invalid FLEETDECK_PORT ${JSON.stringify(raw)} — expected an integer port in 1..65535 (port 0 is not supported)\n`,
    );
    process.exit(2);
  }
  return port;
}
const SERVICE_NAME = 'fleetdeck';

const ENV_FILE = path.join(HOME, 'service.env');

// The installed service.env is the frozen config BOTH supervisors serve from (see
// the ENV-FILE SAFETY CONTRACT below). A `status` / `service start|stop` run from
// a shell where FLEETDECK_PORT is unset (or only partially re-exported) would
// otherwise health-check the WRONG port — reporting a healthy custom-port service
// down, and letting stop signal a different responder entirely. So when the
// ambient env does not name a port, honor the port captured at install time.
// An explicit ambient FLEETDECK_PORT still wins: the env file's own contract
// tells you to re-run `service install` after changing config, and you may
// legitimately point the CLI at a plugin-spawned (non-service) daemon.
//
// Both readers of this file (a POSIX `.`-source and systemd's EnvironmentFile)
// strip one level of matching outer quotes, so a single-quoted value parses to
// its inner text here too. Anything we cannot interpret safely (a junk port, a
// value still carrying quote characters) is IGNORED — health checks fall back to
// the default port rather than fetching an attacker-chosen one from a
// hand-edited file.
function parseServiceEnvPort(text: unknown): number | null {
  if (typeof text !== 'string') return null;
  for (const line of text.split('\n')) {
    const m = /^FLEETDECK_PORT=(.*)$/.exec(line);
    if (!m) continue;
    let v = (m[1] ?? '').trim();
    if (v.length >= 2 && v.startsWith("'") && v.endsWith("'")) v = v.slice(1, -1);
    const n = Number(v);
    if (v !== '' && Number.isInteger(n) && n > 0 && n <= 65535) return n;
  }
  return null;
}

function serviceEnvPort(): number | null {
  try {
    return parseServiceEnvPort(fs.readFileSync(ENV_FILE, 'utf8'));
  } catch {
    return null;
  } // not installed yet — the default port applies
}

const PORT = resolveCliPort();
const SUPERVISE_SH = path.join(HOME, 'supervise.sh');
const SUPERVISOR_PID = path.join(HOME, 'supervisor.pid');
const FLEETD_PID = path.join(HOME, 'fleetd.pid');
const LOG_FILE = path.join(HOME, 'fleetd.log');
const UNIT_FILE = path.join(
  // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing -- an empty XDG_CONFIG_HOME must fall back to the default, not be kept as ''
  process.env['XDG_CONFIG_HOME'] || path.join(os.homedir() || '/tmp', '.config'),
  'systemd',
  'user',
  `${SERVICE_NAME}.service`,
);

function version(): string {
  try {
    const v = (
      JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8')) as { version?: unknown }
    ).version;
    return typeof v === 'string' && v ? v : '0.0.0';
  } catch {
    return '0.0.0';
  }
}

const out = (s: string) => process.stdout.write(`${s}\n`);
const err = (s: string) => process.stderr.write(`${s}\n`);

// ------------------------------------------------------------------- serve

// The dedicated exit code for "launched under the wrong runtime" — distinct from
// 1 (generic failure), 2 (usage), and 3 (lost the port election). EX_CONFIG from
// sysexits(3): the service is MIS-CONFIGURED (pointed at the wrong interpreter),
// not crashed. Supervisors GENERATED FROM 0.23.1+ (UNIT/SUPERVISE below) treat it
// as fail-stopped; a pre-0.23.1 unit predates that and keeps restarting, but every
// start now logs the one-line fix and exits BEFORE binding a port or importing the
// daemon — a readable journal line instead of a hot-looping ReferenceError stack.
const EXIT_WRONG_RUNTIME = 78;

// The installed package is INCOMPLETE — the daemon entrypoint (the committed
// bundle in a published install, or the source in a dev checkout) is not on
// disk, so `serve` has nothing to import. This is what a partial or interrupted
// `npm i -g` leaves behind: bin/ landed but src/daemon/fleetd.bundle.mjs did not
// (an in-place upgrade over a daemon still running out of the global dir is one
// way to get there — the exact state a Coder workspace hit after the Bun
// cutover). Before this guard, serve() fell through to importing the unshippable
// source and emitted a baffling `Cannot find module '.../src/daemon/takeover.ts'
// from '.../bin/fleetdeck.mjs'`. Reinstalling is the only fix, so — like
// EXIT_WRONG_RUNTIME — the generated supervisors must NOT restart on it.
// EX_NOINPUT from sysexits(3): an input file was missing.
const EXIT_INCOMPLETE_INSTALL = 66;

// Run the daemon in the FOREGROUND. This is what a supervisor execs, so it must
// not fork, must not detach, and must let the daemon's own SIGTERM handler run —
// fleetd already has a tested graceful shutdown and we must not shadow it.
async function serve(): Promise<void> {
  // RUNTIME PREFLIGHT. From v0.23.0 the daemon bundle calls `Bun.serve` (+ native
  // WebSocket) unconditionally, so under any non-Bun runtime the daemon throws a
  // raw `ReferenceError: Bun is not defined` on its first server line. The way
  // that actually reaches a user is an in-place upgrade: `npm i -g fleetdeck@latest`
  // refreshes the package but does NOT rewrite an already-installed systemd unit /
  // supervise.sh, so a pre-0.23.0 `ExecStart=node …/fleetdeck.mjs serve` now points
  // Node at a Bun-only daemon. With `Restart=always` that ReferenceError becomes a
  // hot crash-loop no operator can read. Fail fast and legibly with the one-line
  // fix, and exit with a code the generated supervisors do NOT restart. (A fresh
  // install can't hit this — `service install` runs under Bun, so it writes a bun
  // ExecStart; the plugin hooks likewise invoke `bun …`. This guards the upgrade.)
  const bun = process.versions.bun;
  if (!bun || !bunVersionSupported(bun)) {
    const under = bun ? `Bun ${bun} (older than ${MIN_BUN_VERSION})` : `Node ${process.version}`;
    err(
      `✗ fleetd requires Bun ${MIN_BUN_VERSION}+ but was launched under ${under}.\n` +
        '  An older service unit is still starting fleetd under the wrong runtime.\n' +
        '  Re-point it (with bun on PATH):  fleetdeck service install\n' +
        `  or launch directly:              bun ${path.join(HERE, 'fleetdeck.mjs')} serve`,
    );
    process.exit(EXIT_WRONG_RUNTIME);
  }
  // INSTALL-INTEGRITY PREFLIGHT. The entrypoint resolved above (FLEETD) is the
  // committed bundle in a published install; a partial `npm i -g` can leave it
  // absent while bin/ is present. Importing a missing path throws a cryptic
  // `Cannot find module '.../fleetd.ts'` attributed to THIS file (the importer),
  // not to the missing daemon — so name what is actually wrong, and exit a code
  // the generated supervisors do NOT restart (reinstalling is the only fix).
  if (!fs.existsSync(FLEETD)) {
    err(
      `✗ fleetd entrypoint is missing: ${FLEETD}\n` +
        '  This fleetdeck install is incomplete — the daemon bundle did not land\n' +
        '  (a partial or interrupted `npm install -g`). Reinstall it:\n' +
        '    npm install -g fleetdeck',
    );
    process.exit(EXIT_INCOMPLETE_INSTALL);
  }
  process.env['FLEETDECK_MANAGED'] = '1';
  // pathToFileURL, not string concat: a legal install path containing `#`, a raw
  // `%`, or spaces still resolves under Bun — `file://${path}` truncates at the
  // fragment delimiter and throws URIError on a raw percent sequence. A `?` does
  // NOT resolve, even here: Bun decodes the href and re-splits the path at the `?`
  // at both the entrypoint resolver and this in-process `import()`, so a `?` prefix
  // is an accepted, documented limitation of the Bun-only install (see
  // tests/cli-serve-paths.test.ts).
  await import(pathToFileURL(FLEETD).href);
}

// ------------------------------------------------------------------ health

async function health({ timeout = 1000 }: { timeout?: number } = {}): Promise<Health | null> {
  try {
    const res = await fetch(`http://127.0.0.1:${PORT}/health`, {
      signal: AbortSignal.timeout(timeout),
    });
    if (!res.ok) return null;
    return (await res.json()) as Health;
  } catch {
    return null;
  }
}

async function status(args: string[] = []): Promise<number> {
  const showToken = args.includes('--show-token');
  const h = await health();
  if (!h) {
    out(`fleetdeck: no daemon answering on 127.0.0.1:${PORT}`);
    return 1;
  }
  out(`fleetdeck v${h.version}${h.managed ? ' (managed service)' : ' (plugin-spawned)'}`);
  out(`  pid      ${h.pid}`);
  out(`  port     ${PORT}`);
  // The board key lives in the ?t= of the credentialed URL. status output routinely
  // ends up in logs, terminal scrollback, support pastes and screen-shares, so the
  // key is REDACTED by default even though the caller could read it from HOME/token:
  // a secret that only leaks when you explicitly ask for it is a far smaller footgun.
  // `--show-token` reprints the full link for the moment you actually mean to copy it.
  let key = '';
  try {
    key = fs.readFileSync(path.join(HOME, 'token'), 'utf8').trim();
  } catch {
    /* no token file yet — the daemon mints one on next boot */
  }
  if (key && showToken) {
    out(`  board    http://127.0.0.1:${PORT}/?t=${encodeURIComponent(key)}`);
  } else if (key) {
    out(
      `  board    http://127.0.0.1:${PORT}/  (board key hidden — run \`fleetdeck token\`, or \`fleetdeck status --show-token\` for the full link)`,
    );
  } else {
    out(`  board    http://127.0.0.1:${PORT}/`);
  }
  out(`  sessions ${h.fleet}`);
  out(
    // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing -- an empty spawn.reason must fall back to 'unknown', not be kept as ''
    `  spawn    ${h.spawn?.available ? `available (${h.spawn.active} active)` : `unavailable — ${h.spawn?.reason || 'unknown'}`}`,
  );
  const own = version();
  if (h.managed && h.version !== own && own !== '0.0.0') {
    out(`  ⚠ this CLI is v${own}; restart the service to pick it up`);
  }
  return 0;
}

// ------------------------------------------------------------------ doctor

// The supported Bun floor, kept in lockstep with package.json `engines`. 1.3.14
// is the minimum because the daemon requires `Bun.serve` + native WebSocket +
// `bun:sqlite` as they behave in 1.3.14 — the validated single-runtime baseline.
// `process.versions.bun` reports a full `x.y.z`, so an incomplete version string
// (a missing minor or patch) is conservatively rejected rather than assumed.
const MIN_BUN_VERSION = '1.3.14';
function bunVersionSupported(version: unknown): boolean {
  const parts = String(version)
    .split('.')
    .map((n) => Number.parseInt(n, 10));
  const major = parts[0];
  const minor = parts[1];
  const patch = parts[2];
  if (major === undefined || Number.isNaN(major)) return false;
  if (minor === undefined || Number.isNaN(minor)) return false;
  if (patch === undefined || Number.isNaN(patch)) return false;
  // >= 1.3.14
  if (major !== 1) return major > 1;
  if (minor !== 3) return minor > 3;
  return patch >= 14;
}

async function onPath(cmd: string): Promise<boolean> {
  try {
    await execFileP('sh', ['-c', `command -v ${cmd}`], { timeout: 5000 });
    return true;
  } catch {
    return false;
  }
}

// Preflight for a standalone box. Split into HARD failures (the fleet cannot
// work) and WARNINGS (it will come up, but something you care about is missing).
// The Coder template runs `doctor` before `service start`, so a warning must
// never block boot — you need the board up to even read the warning.
async function doctor(): Promise<number> {
  const problems: string[] = [];
  const warnings: string[] = [];

  const bunVersion = process.versions.bun;
  if (!bunVersion) {
    problems.push(
      `this process is not running under Bun — the fleetdeck CLI and daemon require Bun ${MIN_BUN_VERSION}+ (install from https://bun.sh)`,
    );
  } else if (!bunVersionSupported(bunVersion)) {
    problems.push(`Bun ${bunVersion} is too old — fleetd needs Bun ${MIN_BUN_VERSION}+`);
  }
  if (!(await onPath('bun'))) {
    problems.push(
      'bun is not on PATH — the CLI launches via `#!/usr/bin/env bun` and the systemd unit re-execs it, so the OS cannot start fleetd without bun on PATH',
    );
  }

  if (!(await onPath('tmux'))) {
    problems.push(
      `tmux ${MIN_TMUX_VERSION}+ is not on PATH — every agent runs in a tmux pane, so nothing can spawn without it`,
    );
  } else {
    try {
      const { stdout } = await execFileP('tmux', ['-V'], { timeout: 5_000 });
      const parsed = parseTmuxVersion(stdout);
      if (!parsed) {
        problems.push(
          `tmux version could not be determined — fleetdeck requires tmux ${MIN_TMUX_VERSION}+ for safe server probing`,
        );
      } else if (!tmuxVersionSupported(stdout)) {
        problems.push(
          `tmux ${parsed.version} is too old — fleetdeck requires tmux ${MIN_TMUX_VERSION}+ for safe server probing`,
        );
      }
    } catch {
      problems.push(
        `tmux version could not be determined — fleetdeck requires tmux ${MIN_TMUX_VERSION}+ for safe server probing`,
      );
    }
  }
  if (!(await onPath('claude'))) {
    problems.push('the `claude` CLI is not on PATH — the board would have nothing to launch');
  }

  try {
    fs.mkdirSync(HOME, { recursive: true });
    fs.accessSync(HOME, fs.constants.W_OK);
  } catch (e) {
    problems.push(
      `FLEETDECK_HOME (${HOME}) is not writable: ${errnoCode(e) ?? (e instanceof Error ? e.message : String(e))}`,
    );
  }

  // The plugin is what makes a spawned pane REPORT. Without it the board can
  // still launch `claude`, but no hook ever fires, so the card is created and
  // then never moves again — the single most confusing way for this to fail.
  let pluginSeen = false;
  try {
    const { stdout } = await execFileP('claude', ['plugin', 'list'], { timeout: 10_000 });
    pluginSeen = /fleetdeck/i.test(stdout);
  } catch {
    /* old CLI, or claude missing — already covered above */
  }
  if (!pluginSeen) {
    warnings.push(
      'the fleetdeck plugin does not appear to be installed for the `claude` CLI. ' +
        'The board can still spawn agents, but their hooks will never report, so every card ' +
        'will sit at its initial state forever. Install it with:\n' +
        '    claude plugin marketplace add lacion/fleet-deck && claude plugin install fleetdeck@fleetdeck',
    );
  }

  const running = await health({ timeout: 500 });
  if (running)
    out(`ℹ a daemon is already up on :${PORT} (v${running.version}, pid ${running.pid})`);

  if (
    process.env['FLEETDECK_PROXY_AUTH'] === 'trust' &&
    !process.env['FLEETDECK_TRUSTED_ORIGINS']
  ) {
    problems.push(
      'FLEETDECK_PROXY_AUTH=trust with no FLEETDECK_TRUSTED_ORIGINS — the daemon will refuse to start',
    );
  }

  for (const w of warnings) err(`⚠ ${w}`);
  for (const p of problems) err(`✗ ${p}`);
  if (!problems.length)
    out(`✓ fleetdeck v${version()} preflight passed${warnings.length ? ' (with warnings)' : ''}`);
  return problems.length ? 1 : 0;
}

// ----------------------------------------------------------------- service

// systemd is NOT a given. A Coder workspace container makes PID 1 the agent's
// init script — there is no init system at all — so the supervised-wrapper path
// below is the common case, not an exotic fallback.
async function hasSystemd(): Promise<boolean> {
  try {
    await execFileP('systemctl', ['--user', 'show-environment'], { timeout: 5000 });
    return true;
  } catch {
    return false;
  }
}

// Freeze the FLEETDECK_* env at install time into one file that BOTH supervisors
// read. systemd user units do not inherit an interactive shell's environment, and
// a `service start` run from a different shell than `service install` would
// otherwise silently get different config. One file, one source of truth.
//
// ENV-FILE SAFETY CONTRACT. service.env is consumed TWO ways that do NOT agree on
// quoting: the no-systemd supervisor `.`-sources it in a POSIX shell (SUPERVISE:
// `set -a; . "$ENV_FILE"; set +a`), where each `KEY=value` is an assignment whose
// RHS is subject to $-expansion, command substitution, and — via an embedded
// space — command splitting; systemd's `EnvironmentFile=` takes the value
// literally to end-of-line and does NO expansion. So a value like `$(id)`,
// `a; rm -rf ~`, or `a b` would be EXECUTED on the shell path and stored verbatim
// on the systemd path — divergent behavior, and arbitrary shell execution on the
// no-systemd path (the common case). Rather than try to quote for two
// incompatible parsers, we VALIDATE: every FLEETDECK_* value must be drawn from a
// charset that is literal in BOTH readers. That set (alnum plus _ - . : / , @ % +
// = *) is the intersection of "safe, unquoted, on a `.`-source assignment RHS"
// and "literal in a systemd EnvironmentFile", and it comfortably covers every real
// config value: ports, hosts, comma-separated trusted origins including a leading
// `*` wildcard label, token/trust, hex OR base64 (`+/=`) tokens, and slash paths.
// A value drawn from it is written BARE (byte-identical to older installs).
//
// But some knobs are legitimately a spaced shell command — FLEETDECK_AGENTS_CMD
// (e.g. `claude agents --json`, agents-poll.mjs) is documented. Bare-writing a
// spaced value would word-split on the `.`-source path, so such values are
// SINGLE-QUOTED: a single-quoted RHS is taken LITERALLY — no $-expansion, no
// command substitution, no splitting — by BOTH a POSIX `.`-source and systemd's
// EnvironmentFile (which strips matching outer quotes), so the two readers agree
// and the divergence the bare path guards against cannot arise. We still refuse
// what single-quoting canNOT reconcile: control chars / newlines (break the line
// format for both), an embedded single quote (ends the quote), AND a backslash —
// systemd's EnvironmentFile resolves backslash escapes UNCONDITIONALLY, even
// inside single quotes (systemd#10659), so `'a\b'` becomes `ab` under systemd but
// `a\b` under the POSIX `.`-source, and a trailing `\` even unterminates the
// systemd quote. All three get a clear install-time refusal naming the key.
const ENV_VALUE_BARE_SAFE = /^[A-Za-z0-9_.:/,@%+=*-]*$/;
// eslint-disable-next-line no-control-regex -- refusing NUL and C0 controls (plus quote and backslash) in env values is the entire purpose of this gate
const ENV_VALUE_UNQUOTABLE = /[\u0000-\u001f\u0027\u005c]/;

function writeEnvFile(): number {
  const lines: string[] = [];
  for (const [k, v] of Object.entries(process.env)) {
    if (!k.startsWith('FLEETDECK_')) continue;
    if (k === 'FLEETDECK_MANAGED') continue; // owned by `serve`, never configuration
    if (v === undefined) continue; // process.env values are string | undefined; a set var is always a string
    if (ENV_VALUE_UNQUOTABLE.test(v)) {
      throw new Error(
        `${k} has a value unsafe for ${ENV_FILE}, which is BOTH shell-sourced (no-systemd ` +
          `supervisor) AND parsed by systemd EnvironmentFile. A newline, control character, ` +
          `embedded single quote, or backslash cannot be represented identically to both readers ` +
          `and is refused. Fix or unset ${k}.`,
      );
    }
    // Bare when safe (unchanged); single-quoted otherwise (spaces / $ ; & | etc.
    // stay literal for both readers). No embedded single quote can reach here.
    lines.push(ENV_VALUE_BARE_SAFE.test(v) ? `${k}=${v}` : `${k}='${v}'`);
  }
  fs.mkdirSync(HOME, { recursive: true });
  // 0600: FLEETDECK_TOKEN may legitimately live here. The mode option protects
  // only a NEWLY CREATED file — Node ignores it when the file already exists,
  // so a pre-existing permissive service.env would survive a reinstall. chmod
  // after every write to repair an existing inode, and fail the install when
  // the owner-only contract cannot be established.
  fs.writeFileSync(ENV_FILE, `${lines.join('\n')}\n`, { encoding: 'utf8', mode: 0o600 });
  fs.chmodSync(ENV_FILE, 0o600);
  return lines.length;
}

// systemd-UNIT PATH SAFETY. The two path-bearing directives below interpolate
// REAL paths (the node binary, this script, FLEETDECK_HOME/service.env) that the
// user does not control the shape of — Node under `nvm/v24 linux/node`, a
// FLEETDECK_HOME with a space, a project dir named `100%`. Written bare, systemd
// would word-split a spaced path into extra argv, treat `"`/`'` as quoting
// syntax, and expand `%` as a specifier (`%i`, `%h`, ...) — so a valid install
// produced a unit that fails to start or reads the wrong env file.
//
// Escaping follows systemd.syntax(7):
//  - `%` → `%%` FIRST, in every directive. Specifier expansion runs before
//    tokenization, so it must be neutralized before any quoting.
//  - ExecStart: EVERY argv token is double-quoted (quoteExecArg, BUG-078;
//    unitArg retains BUG-077's conditional bare form for its exported
//    contract). Inside the quotes `%` is already doubled and `'` is
//    backslash-escaped — the two escapes systemd resolves there. What quoting
//    cannot carry is REFUSED with a clear install-time error: control
//    characters (they break the line format) and `"`.
//  - EnvironmentFile: takes ONE path (no tokenization), so instead of quoting
//    we REFUSE paths the unquoted grammar cannot carry: whitespace, quotes, or
//    a backslash get a clear install-time error naming the path. Anything else
//    (spaces excluded — refused) is literal to end-of-line.
const UNIT_VALUE_UNSAFE = /[\s"'\\]/;
// eslint-disable-next-line no-control-regex -- refusing NUL and C0 controls (and the double-quote) in ExecStart args is the entire purpose of this gate
const EXEC_ARG_UNQUOTABLE = /[\u0000-\u001f"]/;

function unitEscape(s: string): string {
  return s.replace(/%/g, '%%');
}

// BUG-078's ExecStart quoting: EVERY token is double-quoted (BUG-077 kept a
// bare fast path; it was dropped because systemd's ExecStart grammar makes a
// bare token with an embedded quote unsafe, while a quoted token stays exactly
// one argv element whatever it contains). Inside the quotes `%` is doubled and
// `'` backslash-escaped — the escapes systemd resolves there. What quoting
// cannot carry — control characters (they break the line format) and `"` —
// is refused with a clear install-time error.
function quoteExecArg(p: string): string {
  if (EXEC_ARG_UNQUOTABLE.test(p)) {
    throw new Error(
      `cannot write ${UNIT_FILE}: the path ${JSON.stringify(p)} contains a control character ` +
        'or double quote, which cannot be represented in a systemd ExecStart line. ' +
        'Install Node and fleetdeck at a path without those characters.',
    );
  }
  return `"${p.replaceAll('%', '%%').replaceAll("'", "\\'")}"`;
}

// BUG-077's ExecStart helper, kept as the exported contract: a bare-safe token
// (after %-escaping, which runs before the bare/quoted decision because
// specifier expansion precedes tokenization) passes through byte-identical to
// older installs; anything else is quoted, with `"` and `\` escaped inside.
// NOTE: the generated unit itself uses quoteExecArg (always-quote, refuse `"`)
// — the adversarially verified BUG-078 emission; unitArg remains for callers
// that need the older conditional form.
function unitArg(s: string): string {
  const escaped = unitEscape(s).replace(/\\/g, '\\\\').replace(/"/g, '\\"');
  return ENV_VALUE_BARE_SAFE.test(s) ? escaped : `"${escaped}"`;
}

function unitEnvFilePath(p: string): string {
  if (UNIT_VALUE_UNSAFE.test(p)) {
    throw new Error(
      `FLEETDECK_HOME resolves to ${p}, which the systemd EnvironmentFile directive cannot ` +
        `carry (whitespace, quotes, and backslashes are not quotable there). Point FLEETDECK_HOME ` +
        `at a path without those characters and re-run \`fleetdeck service install\`.`,
    );
  }
  return unitEscape(p);
}

const UNIT = (): string => `[Unit]
Description=Fleet Deck — the always-on board for your Claude Code fleet
After=network.target

[Service]
Type=simple
EnvironmentFile=-${unitEnvFilePath(ENV_FILE)}
ExecStart=${quoteExecArg(process.execPath)} ${quoteExecArg(path.join(HERE, 'fleetdeck.mjs'))} serve
Restart=always
RestartSec=2
# exit 3 is "another daemon already owns the port" — restarting is a hot loop.
# exit ${EXIT_WRONG_RUNTIME} is "launched under the wrong runtime" (Node vs Bun) — reinstall, don't restart.
# exit ${EXIT_INCOMPLETE_INSTALL} is "install incomplete, daemon bundle missing" — reinstall, don't restart.
RestartPreventExitStatus=3 ${EXIT_WRONG_RUNTIME} ${EXIT_INCOMPLETE_INSTALL}

[Install]
WantedBy=default.target
`;

// POSIX single-argument quoting for every path the generated shell wrapper
// embeds. Inside single quotes NOTHING expands — no $VAR, no $(), no backticks
// — so a literal FLEETDECK_HOME or install path containing shell metacharacters
// stays literal when supervise.sh runs. Double quotes would keep $(), backticks,
// and $VAR live (BUG-079: a path like `$(printf injected)` was EXECUTED while
// the wrapper resolved it). An embedded single quote uses the standard
// '...'\''...' idiom: end quote, escaped quote, reopen.
const shQuote = (s: string): string => `'${s.replace(/'/g, `'\\''`)}'`;

// The no-systemd supervisor. Deliberately a shell script rather than a Node
// parent: one less long-lived process, and it survives this CLI exiting, which
// a coder_script REQUIRES (a script that does not exit leaves the workspace
// stuck "starting" forever).
const SUPERVISE = (): string => `#!/bin/sh
# GENERATED by \`fleetdeck service install\` — do not edit; re-run install instead.
set -u
[ -f ${shQuote(ENV_FILE)} ] && { set -a; . ${shQuote(ENV_FILE)}; set +a; }

child=''
term() { [ -n "$child" ] && kill -TERM "$child" 2>/dev/null; exit 0; }
trap term TERM INT

delay=1
while :; do
  ${shQuote(process.execPath)} ${shQuote(path.join(HERE, 'fleetdeck.mjs'))} serve &
  child=$!
  wait "$child"
  code=$?
  child=''
  # 0 — a deliberate SIGTERM shutdown. Respawning would fight whoever stopped us.
  [ "$code" -eq 0 ] && exit 0
  # 3 — lost the port election; another daemon owns :${PORT}. Respawning is a hot loop.
  [ "$code" -eq 3 ] && exit 3
  # ${EXIT_WRONG_RUNTIME} — launched under the wrong runtime (Node vs Bun). Respawning cannot help; reinstall.
  [ "$code" -eq ${EXIT_WRONG_RUNTIME} ] && exit ${EXIT_WRONG_RUNTIME}
  # ${EXIT_INCOMPLETE_INSTALL} — install incomplete, daemon bundle missing. Respawning cannot help; reinstall.
  [ "$code" -eq ${EXIT_INCOMPLETE_INSTALL} ] && exit ${EXIT_INCOMPLETE_INSTALL}
  sleep "$delay"
  delay=$(( delay < 30 ? delay * 2 : 30 ))
done
`;

async function serviceInstall(): Promise<number> {
  const n = writeEnvFile();
  if (await hasSystemd()) {
    fs.mkdirSync(path.dirname(UNIT_FILE), { recursive: true });
    fs.writeFileSync(UNIT_FILE, UNIT(), 'utf8');
    await execFileP('systemctl', ['--user', 'daemon-reload'], { timeout: 10_000 });
    await execFileP('systemctl', ['--user', 'enable', SERVICE_NAME], { timeout: 10_000 });
    out(`✓ installed systemd user unit ${UNIT_FILE}`);
    out(`  captured ${n} FLEETDECK_* var(s) into ${ENV_FILE}`);
    out('  note: in a container a user unit needs `loginctl enable-linger` to outlive your login.');
  } else {
    fs.writeFileSync(SUPERVISE_SH, SUPERVISE(), { encoding: 'utf8', mode: 0o700 });
    out(`✓ no systemd — installed supervised wrapper ${SUPERVISE_SH}`);
    out(`  captured ${n} FLEETDECK_* var(s) into ${ENV_FILE}`);
  }
  out('  re-run `fleetdeck service install` after changing any FLEETDECK_* variable.');
  return 0;
}

// SUPERVISOR IDENTITY CONTRACT. supervisor.pid records the pid of the detached
// `sh SUPERVISE_SH` that serviceStart backgrounds. A bare kill(pid, 0) only proves
// SOMETHING with that pid is alive — after a reboot + PID reuse a stale
// supervisor.pid can point at an unrelated live process, which would make
// `service start` falsely report "already running" (the board never comes up) or
// make `service stop` SIGTERM an innocent process. So, exactly like the daemon's
// own HOME-ownership check (takeover.mjs `livePidLooksLikeFleetd`), we verify the
// live process is actually OUR supervisor before trusting the pidfile: on Linux
// its /proc/<pid>/cmdline must reference SUPERVISE_SH. /proc is Linux-only; on
// macOS/other there is no cheap identity probe, so we fall back to kill(0)
// liveness alone (documented limitation: a recycled pid there can still be
// misread, the same fallback the daemon accepts on non-Linux).
// The match rule, kept pure (no /proc read) so it is testable without spawning a
// real supervisor: we backgrounded `sh SUPERVISE_SH`, so the absolute SUPERVISE_SH
// path is present in the live process's argv exactly when the pid is still ours.
function argvIsOurSupervisor(argv: unknown): boolean {
  return Array.isArray(argv) && argv.includes(SUPERVISE_SH);
}

function supervisorLooksLikeOurs(pid: number): boolean {
  if (process.platform !== 'linux') return true; // no /proc — best-effort fallback
  try {
    const argv = fs
      .readFileSync(`/proc/${pid}/cmdline`)
      .toString('utf8')
      .split('\0')
      .filter(Boolean);
    return argvIsOurSupervisor(argv);
  } catch (err) {
    // ENOENT is decisive: the pid died between kill(0) and here, so it is not our
    // supervisor. Permission/transient I/O errors are NOT decisive — treating a
    // still-live process as ours avoids falsely dropping a running supervisor.
    return errnoCode(err) !== 'ENOENT';
  }
}

function supervisorAlive(): number {
  try {
    const pid = Number(fs.readFileSync(SUPERVISOR_PID, 'utf8').trim());
    if (!Number.isInteger(pid) || pid <= 0) return 0;
    process.kill(pid, 0);
    if (!supervisorLooksLikeOurs(pid)) return 0; // stale pidfile after PID reuse
    return pid;
  } catch {
    return 0;
  }
}

// STOP-TARGET IDENTITY CONTRACT. The no-systemd stop path used to SIGTERM
// whatever pid ANY health-compatible responder on our port returned — a fake
// local server, or another installation's daemon answering on a recycled port,
// could aim our SIGTERM at an arbitrary same-user process. So, exactly like the
// hook's takeover gate (takeover.mjs `verifyDaemonPid`), a health pid is only
// signalled when EVERY one of these holds:
//   - the responder claims to be a MANAGED daemon (h.managed) — `service stop`
//     owns the supervised install only; a plugin-spawned daemon is not ours to kill;
//   - the pid matches the one recorded in OUR HOME's fleetd.pid (the HOME
//     ownership lock), and any port recorded there matches our selected PORT;
//   - the live process still carries a fleetd /proc shape (livePidLooksLikeFleetd).
// Any disagreement → false, and the caller reports the foreign responder and
// leaves it untouched rather than signalling a process it cannot identify.
//
// pidRecord / livePidLooksLikeFleetd (and verifyDaemonPid below) come from
// takeover.ts via a STATIC top-level import, so `bun run bundle:bin` esbuild-
// inlines them into bin/fleetdeck.mjs and the shipped CLI carries this logic
// itself. That is mandatory, not stylistic: a published (bundle-only) install
// ships only bin/ + the daemon bundle — no src/ tree — so the former computed-
// path require()/import() of src/daemon/takeover.ts raised ERR_MODULE_NOT_FOUND
// there and crashed every command that reached these gates (`service start`/
// `stop`). takeover.ts stays dependency-free (node builtins + pure helpers), so
// esbuild inlines it cleanly — the same way the SessionStart hook bundle does.
function healthPidIsOurDaemon(h: Health | null | undefined): boolean {
  if (!h?.managed) return false;
  // eslint-disable-next-line @typescript-eslint/no-unnecessary-type-conversion -- h is an unchecked cast of wire JSON; Number() coerces a stringy pid before the integer guard
  const pid = Number(h.pid);
  if (!Number.isInteger(pid) || pid <= 0) return false;
  let record: { pid: number; port: number | null } | null;
  try {
    record = pidRecord(fs.readFileSync(FLEETD_PID, 'utf8'));
  } catch {
    // No/unreadable pidfile: cannot prove ownership → do not kill.
    return false;
  }
  if (record?.pid !== pid) return false;
  // The pidfile predates port recording (or another daemon in the chain wrote
  // it before the port was frozen in): a missing recorded port cannot
  // DISPROVE ownership, so only a recorded port that disagrees with the
  // selected PORT vetoes the kill.
  if (record.port !== null && record.port !== PORT) return false;
  return livePidLooksLikeFleetd(pid);
}

// "Started" must mean "answering", not "spawned". systemctl returns as soon as
// the process exists, which is a good ~100ms before fleetd has opened SQLite and
// bound the port — long enough that a template's next step (or an impatient
// human running `fleetdeck status`) sees a dead board and concludes it failed.
async function waitForHealth({
  tries = 20,
  everyMs = 250,
  expect,
}: {
  tries?: number;
  everyMs?: number;
  expect?: (h: Health) => boolean | Promise<boolean>;
} = {}): Promise<Health | null> {
  for (let i = 0; i < tries; i += 1) {
    await new Promise((r) => setTimeout(r, everyMs));
    const h = await health({ timeout: everyMs });
    // AWAIT the predicate. healthIsOurManagedDaemon is async (its Promise wraps
    // takeover's verifyDaemonPid), and an un-awaited Promise is ALWAYS truthy —
    // which silently turned the managed-identity gate into a no-op on the
    // supervised `service start` path, so a foreign/unmanaged responder already
    // owning the port was accepted as "up". Awaiting restores the gate.
    if (h && (!expect || (await expect(h)))) return h;
  }
  return null;
}

// A health answer proves SOMETHING is listening on :PORT, not that it is the
// managed service we just spawned. `service start` must accept only OUR daemon:
// managed (started via `serve`, which sets FLEETDECK_MANAGED=1) AND positively
// identified as the fleetd that claimed OUR FLEETDECK_HOME — its /health pid
// must match the HOME/fleetd.pid ownership record and still carry a fleetd
// /proc shape (the same verifyDaemonPid gate the hook's takeover path uses).
// Without this, an unmanaged/foreign responder that already owns the port makes
// `service start` print success for a service that does not exist: our wrapper
// exits 3 after losing the port election while waitForHealth saw the squatter.
async function healthIsOurManagedDaemon(h: Health | null | undefined): Promise<boolean> {
  if (!h?.managed) return false;
  return verifyDaemonPid(h.pid, HOME);
}

async function serviceStart(): Promise<number> {
  if ((await hasSystemd()) && fs.existsSync(UNIT_FILE)) {
    await execFileP('systemctl', ['--user', 'start', SERVICE_NAME], { timeout: 15_000 });
    if (!(await waitForHealth())) {
      err(`✗ ${SERVICE_NAME}.service started but no daemon answered on :${PORT} within 5s`);
      err('  systemctl --user status fleetdeck  /  journalctl --user -u fleetdeck');
      return 1;
    }
    out(`✓ fleetdeck up on http://127.0.0.1:${PORT} (${SERVICE_NAME}.service)`);
    return 0;
  }
  if (!fs.existsSync(SUPERVISE_SH)) {
    err('✗ not installed — run `fleetdeck service install` first');
    return 1;
  }
  const sup = supervisorAlive();
  if (sup) {
    // A live wrapper is not a live BOARD: the supervisor sleeps in exponential
    // backoff between respawns, so during a fleetd crash-loop kill(0) alone
    // would report "already running" while nothing answers on the port —
    // indefinitely. "Started" means "answering", same contract as the branch
    // below: require a MANAGED health response (an unmanaged daemon on the
    // port is a plugin-spawned squatter the wrapper is not supervising).
    const h = await waitForHealth();
    if (h?.managed) {
      out('✓ already running');
      return 0;
    }
    err(
      `✗ supervisor alive (pid ${sup}) but no managed daemon answering on :${PORT} — see ${LOG_FILE}`,
    );
    err(
      '  the wrapper may be backing off between respawns; check the log, or `fleetdeck service stop` then start',
    );
    return 1;
  }
  // MUST return immediately: a coder_script that does not exit leaves the
  // workspace stuck "starting".
  fs.mkdirSync(HOME, { recursive: true });
  const log = fs.openSync(LOG_FILE, 'a', 0o600);
  try {
    fs.chmodSync(LOG_FILE, 0o600);
  } catch {
    /* pre-existing perms */
  }
  const child = spawn('sh', [SUPERVISE_SH], { detached: true, stdio: ['ignore', log, log] });
  child.once('error', () => {
    /* detached supervisor; failures surface via the health probe below */
  });
  child.unref();
  fs.closeSync(log);
  fs.writeFileSync(SUPERVISOR_PID, String(child.pid), { encoding: 'utf8', mode: 0o600 });

  const h = await waitForHealth({ expect: healthIsOurManagedDaemon });
  if (!h) {
    err(
      `✗ supervisor started (pid ${String(child.pid)}) but no MANAGED daemon for this FLEETDECK_HOME answered on :${PORT} within 5s — see ${LOG_FILE}`,
    );
    if (await health({ timeout: 500 })) {
      err('  something else already owns the port — `fleetdeck status` shows what is answering');
    }
    return 1;
  }
  // The daemon answering is ours, but the SUPERVISOR may already be gone (it
  // exits 3 right after a lost port election, or could crash immediately).
  // Reporting "up (supervisor pid N)" for a dead N would promise an always-on
  // service nothing is supervising — fail instead of printing a corpse.
  if (!supervisorAlive()) {
    err(
      `✗ daemon up on :${PORT} but supervisor pid ${String(child.pid)} already exited — no always-on service — see ${LOG_FILE}`,
    );
    return 1;
  }
  out(`✓ fleetdeck up on http://127.0.0.1:${PORT} (supervisor pid ${String(child.pid)})`);
  return 0;
}

async function serviceStop(): Promise<number> {
  if ((await hasSystemd()) && fs.existsSync(UNIT_FILE)) {
    await execFileP('systemctl', ['--user', 'stop', SERVICE_NAME], { timeout: 15_000 });
    out(`✓ stopped ${SERVICE_NAME}.service`);
    return 0;
  }
  // ORDER MATTERS: kill the supervisor FIRST. Signal the daemon while its
  // supervisor is alive and the supervisor dutifully restarts it.
  const sup = supervisorAlive();
  if (sup) {
    try {
      process.kill(sup, 'SIGTERM');
    } catch {
      /* already gone */
    }
  }
  try {
    fs.unlinkSync(SUPERVISOR_PID);
  } catch {
    /* best effort */
  }

  const h = await health({ timeout: 500 });
  const ours = h?.pid ? healthPidIsOurDaemon(h) : false;
  if (ours && h) {
    try {
      process.kill(h.pid, 'SIGTERM');
    } catch {
      /* already gone */
    }
    for (let i = 0; i < 12; i += 1) {
      await new Promise((r) => setTimeout(r, 250));
      if (!(await health({ timeout: 250 }))) break;
    }
  } else if (h?.pid) {
    // A responder answered /health but failed the identity gate — a foreign
    // daemon or a fake server squatting on our port. Do NOT signal it.
    err(
      `⚠ a daemon is answering on :${PORT} (pid ${h.pid}) but it is not this home's managed fleetd — leaving it untouched`,
    );
  }
  out(sup || ours ? '✓ stopped' : 'ℹ nothing was running');
  return 0;
}

async function serviceUninstall(): Promise<number> {
  await serviceStop();
  if ((await hasSystemd()) && fs.existsSync(UNIT_FILE)) {
    try {
      await execFileP('systemctl', ['--user', 'disable', SERVICE_NAME], { timeout: 10_000 });
    } catch {
      /* not enabled */
    }
    try {
      fs.unlinkSync(UNIT_FILE);
    } catch {
      /* already gone */
    }
    try {
      await execFileP('systemctl', ['--user', 'daemon-reload'], { timeout: 10_000 });
    } catch {
      /* best effort */
    }
  }
  try {
    fs.unlinkSync(SUPERVISE_SH);
  } catch {
    /* already gone */
  }
  out('✓ uninstalled (state in FLEETDECK_HOME was left alone)');
  return 0;
}

async function service(sub: string | undefined): Promise<number> {
  switch (sub) {
    case 'install':
      return serviceInstall();
    case 'uninstall':
      return serviceUninstall();
    case 'start':
      return serviceStart();
    case 'stop':
      return serviceStop();
    case 'restart':
      await serviceStop();
      return serviceStart();
    default:
      err('usage: fleetdeck service <install|uninstall|start|stop|restart>');
      return 2;
  }
}

// ------------------------------------------------------------------- token

async function token(args: string[]): Promise<number> {
  const file = path.join(HOME, 'token');
  if (args.includes('--rotate')) {
    const { randomBytes } = await import('node:crypto');
    fs.mkdirSync(HOME, { recursive: true });
    fs.writeFileSync(file, randomBytes(32).toString('hex'), { encoding: 'utf8', mode: 0o600 });
    // The mode option above applies only when the write CREATES the file — an
    // existing token keeps its inode's permissions, so a pre-existing 0644
    // token would stay world-readable through rotation. chmod on every rotate
    // and refuse to report success when the owner-only contract cannot be
    // established — the new secret must not stay exposed.
    try {
      fs.chmodSync(file, 0o600);
    } catch (e) {
      err(
        `✗ token rotated at ${file} but could not be locked to 0600 (${e instanceof Error ? e.message : String(e)}) — fix its permissions before it is used`,
      );
      return 1;
    }
    out('✓ token rotated — restart the daemon for it to take effect');
  }
  try {
    out(fs.readFileSync(file, 'utf8').trim());
    return 0;
  } catch {
    err(
      `no token at ${file} — one is generated on first start when the board is reachable from off-box`,
    );
    return 1;
  }
}

// -------------------------------------------------------------------- main

const HELP = `fleetdeck v${version()} — the board for your Claude Code fleet

  fleetdeck serve                  run the daemon in the foreground (what a supervisor execs)
  fleetdeck status [--show-token]  ask the running daemon how it is doing (board key hidden unless --show-token)
  fleetdeck doctor                 preflight this machine before running a fleet on it
  fleetdeck service install        install a supervisor (systemd user unit, or a wrapper)
  fleetdeck service start|stop     ...and drive it
  fleetdeck service restart
  fleetdeck service uninstall
  fleetdeck token [--rotate]       print (or replace) the bearer token

Configuration is entirely FLEETDECK_* environment variables — see the README.
For an always-on board on a remote dev box, see docs/CODER.md.
`;

async function main(argv: string[]): Promise<void> {
  const [cmd, ...rest] = argv;
  let code = 0;
  switch (cmd) {
    case 'serve':
      await serve();
      return; // never returns until SIGTERM; serve owns the process
    case 'status':
      code = await status(rest);
      break;
    case 'doctor':
      code = await doctor();
      break;
    case 'service':
      code = await service(rest[0]);
      break;
    case 'token':
      code = await token(rest);
      break;
    case '--version':
    case '-v':
      out(version());
      break;
    case 'help':
    case '--help':
    case '-h':
    case undefined:
      out(HELP);
      break;
    default:
      err(`unknown command: ${cmd}\n`);
      err(HELP);
      code = 2;
  }
  // `serve` returned above and owns the process; every other command is done.
  process.exit(code);
}

// Only dispatch when this file is the process entry point. Importing the module
// (the test suite does) must never run a command or exit the process — so the
// helpers above stay testable in isolation. Compare real paths because the global
// `fleetdeck` bin is a symlink into node_modules while import.meta.url is resolved.
const IS_ENTRYPOINT = (() => {
  try {
    return !!process.argv[1] && fs.realpathSync(process.argv[1]) === fileURLToPath(import.meta.url);
  } catch {
    return false;
  }
})();

if (IS_ENTRYPOINT) await main(process.argv.slice(2));

// exported for tests only — the env-file validation, supervisor identity check,
// and file generators are contracts. Nothing here runs on import (see
// IS_ENTRYPOINT above), so importing is side-effect-free.
export {
  writeEnvFile,
  ENV_VALUE_BARE_SAFE,
  ENV_VALUE_UNQUOTABLE,
  parseServiceEnvPort,
  serviceEnvPort,
  shQuote,
  supervisorAlive,
  supervisorLooksLikeOurs,
  argvIsOurSupervisor,
  healthPidIsOurDaemon,
  healthIsOurManagedDaemon,
  waitForHealth,
  serviceInstall,
  serviceStart,
  UNIT,
  SUPERVISE,
  unitEscape,
  unitArg,
  quoteExecArg,
  unitEnvFilePath,
  doctor,
  MIN_BUN_VERSION,
  bunVersionSupported,
  token,
};
