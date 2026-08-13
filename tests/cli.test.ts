// tests/cli.test.ts
//
// First test coverage for the standalone CLI (bin/fleetdeck.mjs). These cover
// only the parts that do NOT need a live daemon or tmux: the pure/file-writing
// helpers, run against a throwaway FLEETDECK_HOME. `serve`, `service start/stop`
// and health-check paths are deliberately NOT exercised here — they background a
// long-lived daemon.
//
// The module derives HOME / ENV_FILE / SUPERVISE_SH / SUPERVISOR_PID / UNIT_FILE
// from FLEETDECK_HOME and XDG_CONFIG_HOME at import time, so those are set to a
// temp tree BEFORE the dynamic import below. Importing is side-effect-free: the
// CLI dispatch is guarded by IS_ENTRYPOINT, which is false when the module is
// merely imported (the entry point is the node:test runner, not fleetdeck.mjs).

import test, { after } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';

const TMP = fs.mkdtempSync(path.join(os.tmpdir(), 'fleetdeck-cli-'));
const HOME = path.join(TMP, 'home');
const XDG = path.join(TMP, 'config');
// These module-level env mutations are captured by the CLI module at import time
// (below). Under bun's single shared test process they would otherwise outlive
// this file and pollute every later test — the restore in the after() hook below
// undoes them. No-op under node, which isolates each file in its own child.
const SAVED_ENV: Record<string, string | undefined> = {
  FLEETDECK_HOME: process.env['FLEETDECK_HOME'],
  XDG_CONFIG_HOME: process.env['XDG_CONFIG_HOME'],
  FLEETDECK_PORT: process.env['FLEETDECK_PORT'],
};
process.env['FLEETDECK_HOME'] = HOME;
process.env['XDG_CONFIG_HOME'] = XDG;
fs.mkdirSync(HOME, { recursive: true });

// PORT must be a port where NOTHING answers: a dev box can have a real managed
// fleetd on the default 4711, and the serviceStart test below needs the health
// probe to see a dead port. Grab a free port, release it, and pin the CLI to it
// (the module captures PORT at import time, just like HOME).
const DEAD_PORT = await new Promise<number>((resolve) => {
  const srv = net.createServer().listen(0, '127.0.0.1', () => {
    const p = (srv.address() as net.AddressInfo).port;
    srv.close(() => {
      resolve(p);
    });
  });
});
process.env['FLEETDECK_PORT'] = String(DEAD_PORT);

// Undo the three module-level env mutations once this file's tests finish, so
// they don't leak into later files under bun's shared process (see SAVED_ENV).
after(() => {
  for (const [k, v] of Object.entries(SAVED_ENV)) {
    if (v === undefined) delete process.env[k];
    else process.env[k] = v;
  }
});

const ENV_FILE = path.join(HOME, 'service.env');
const SUPERVISE_SH = path.join(HOME, 'supervise.sh');
const SUPERVISOR_PID = path.join(HOME, 'supervisor.pid');
const FLEETD_PID = path.join(HOME, 'fleetd.pid');
const UNIT_FILE = path.join(XDG, 'systemd', 'user', 'fleetdeck.service');

const {
  writeEnvFile,
  ENV_VALUE_BARE_SAFE,
  ENV_VALUE_UNQUOTABLE,
  parseServiceEnvPort,
  serviceEnvPort,
  shQuote,
  token,
  supervisorAlive,
  supervisorLooksLikeOurs,
  argvIsOurSupervisor,
  serviceInstall,
  serviceStart,
  UNIT,
  SUPERVISE,
  MIN_BUN_VERSION,
  bunVersionSupported,
  unitEscape,
  unitArg,
  unitEnvFilePath,
  quoteExecArg,
  healthIsOurManagedDaemon,
  healthPidIsOurDaemon,
} = await import('../bin/fleetdeck.ts');
const { parseTmuxVersion, tmuxVersionCapability, tmuxVersionSupported } =
  await import('../bin/tmux-version.ts');

// `Health` is a file-local interface in bin/fleetdeck.ts (not exported), and the
// health guards below are deliberately fed malformed/partial responder bodies to
// prove they reject them. Recover the exact parameter type from the function and
// funnel each untrusted literal through it at the boundary — `unknown -> HealthArg`
// keeps the junk values verbatim without an `any` anywhere.
type HealthArg = Parameters<typeof healthPidIsOurDaemon>[0];
const asHealth = (h: unknown): HealthArg => h as HealthArg;

// tmuxVersionCapability returns a discriminated union; `.reason` lives only on the
// `available: false` arm. Narrow to that arm for the unknown-version assertion.
type TmuxUnavailCap = Extract<ReturnType<typeof tmuxVersionCapability>, { available: false }>;

after(() => {
  try {
    fs.rmSync(TMP, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
});

test('tmux version parser enforces 3.4+ and rejects unknown output', () => {
  assert.deepEqual(parseTmuxVersion('tmux 3.4\n'), { major: 3, minor: 4, version: '3.4' });
  assert.deepEqual(parseTmuxVersion('tmux 3.7b'), { major: 3, minor: 7, version: '3.7b' });
  assert.deepEqual(parseTmuxVersion('tmux 4.0'), { major: 4, minor: 0, version: '4.0' });
  assert.equal(parseTmuxVersion('tmux next-3.5'), null);
  assert.equal(parseTmuxVersion('3.4'), null);
  assert.equal(tmuxVersionSupported('tmux 3.3a'), false);
  assert.equal(tmuxVersionSupported('tmux 3.4'), true);
  assert.equal(tmuxVersionSupported('tmux 3.10'), true, 'minor versions compare numerically');
  assert.equal(tmuxVersionSupported('unknown'), false);
  assert.deepEqual(tmuxVersionCapability('tmux 3.3a'), {
    available: false,
    version: '3.3a',
    reason: 'tmux 3.3a is too old; tmux 3.4+ required',
  });
  assert.deepEqual(tmuxVersionCapability('tmux 3.4'), { available: true, version: '3.4' });
  assert.match((tmuxVersionCapability('unknown') as TmuxUnavailCap).reason, /version is unknown/);
});

// Single-runtime floor: the daemon requires `Bun.serve` + native WebSocket +
// `bun:sqlite` as they behave in Bun 1.3.14 — the validated baseline for the
// Bun-only swap. So the floor rejects anything below 1.3.14 and accepts 1.3.14+
// and every newer minor/major; source and shipped bundle share one Bun floor.
test('bun engine floor rejects <1.3.14, accepts 1.3.14+ and newer majors/minors', () => {
  assert.equal(bunVersionSupported('1.3.13'), false, 'last release before the floor');
  assert.equal(bunVersionSupported('1.3.14'), true, 'the validated single-runtime floor');
  assert.equal(bunVersionSupported('1.3.20'), true);
  assert.equal(bunVersionSupported('1.4.0'), true, 'newer minor');
  assert.equal(bunVersionSupported('2.0.0'), true, 'newer major');
  assert.equal(bunVersionSupported('1.2.99'), false, 'older minor');
  assert.equal(bunVersionSupported('0.8.1'), false, 'ancient bun');
  assert.equal(bunVersionSupported('1.3'), false, 'incomplete version is rejected conservatively');
  assert.equal(bunVersionSupported('not-a-version'), false);
});

test('bun engine floor matches the declared package.json engines range', () => {
  const pkg = JSON.parse(fs.readFileSync(new URL('../package.json', import.meta.url), 'utf8')) as {
    engines: { bun: string };
  };
  assert.equal(pkg.engines.bun, `>=${MIN_BUN_VERSION}`, 'doctor text and engines must not drift apart');
  assert.equal(MIN_BUN_VERSION, '1.3.14');
});

// Save/clear every FLEETDECK_* var (so a stray one in the ambient environment
// cannot skew a writeEnvFile test), then restore. FLEETDECK_HOME is cleared too,
// but the module already captured its HOME constant at import, so file locations
// are unaffected.
function withCleanFleetEnv<T>(vars: Record<string, string>, fn: () => T): T {
  const saved: Record<string, string | undefined> = {};
  for (const k of Object.keys(process.env)) {
    if (k.startsWith('FLEETDECK_')) {
      saved[k] = process.env[k];
      Reflect.deleteProperty(process.env, k);
    }
  }
  try {
    for (const [k, v] of Object.entries(vars)) process.env[k] = v;
    return fn();
  } finally {
    for (const k of Object.keys(process.env))
      if (k.startsWith('FLEETDECK_')) Reflect.deleteProperty(process.env, k);
    for (const [k, v] of Object.entries(saved)) process.env[k] = v;
  }
}

// ------------------------------------------------------------- writeEnvFile

test('writeEnvFile: writes valid FLEETDECK_* values, 0600, KEY=value per line', () => {
  const n = withCleanFleetEnv(
    {
      FLEETDECK_PORT: '4711',
      FLEETDECK_PROXY_AUTH: 'trust',
      FLEETDECK_TRUSTED_ORIGINS:
        'https://*.coder.example.com,https://fleetdeck--luis--dev--main.example.com',
      FLEETDECK_TOKEN: 'AbC123+/def456ghi789==', // base64-shaped token: + / = are allowed
      FLEETDECK_MANAGED: '1', // must be skipped — owned by `serve`, never config
    },
    () => writeEnvFile(),
  );

  assert.equal(n, 4, 'MANAGED is excluded, the other four are written');
  const body = fs.readFileSync(ENV_FILE, 'utf8');
  assert.match(body, /^FLEETDECK_PORT=4711$/m);
  assert.match(body, /^FLEETDECK_PROXY_AUTH=trust$/m);
  assert.match(
    body,
    /^FLEETDECK_TRUSTED_ORIGINS=https:\/\/\*\.coder\.example\.com,https:\/\/fleetdeck--luis--dev--main\.example\.com$/m,
  );
  assert.match(body, /^FLEETDECK_TOKEN=AbC123\+\/def456ghi789==$/m);
  assert.doesNotMatch(body, /FLEETDECK_MANAGED/, 'MANAGED must never be persisted');
  assert.equal(fs.statSync(ENV_FILE).mode & 0o777, 0o600, 'a token may live here — owner-only');
});

// A legitimately-spaced command knob (FLEETDECK_AGENTS_CMD, documented in the
// README + read by agents-poll.mjs) must SURVIVE install, single-quoted so the
// shell `.`-source keeps it one literal token instead of word-splitting it.
test('writeEnvFile: single-quotes the documented spaced command knob (FLEETDECK_AGENTS_CMD)', () => {
  const n = withCleanFleetEnv({ FLEETDECK_AGENTS_CMD: 'claude agents --json' }, () =>
    writeEnvFile(),
  );
  assert.equal(n, 1);
  const body = fs.readFileSync(ENV_FILE, 'utf8');
  assert.match(
    body,
    /^FLEETDECK_AGENTS_CMD='claude agents --json'$/m,
    'spaced command single-quoted, not split or refused',
  );
});

// Shell metacharacters are ACCEPTED and single-quoted — literal to BOTH the
// `.`-source and systemd EnvironmentFile (a single-quoted RHS gets no expansion
// in either), so the divergence the bare path guards against cannot arise.
const escapeRe = (s: string): string => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
for (const [label, val] of [
  ['a $() command substitution', '$(id)'],
  ['a ; command separator', 'a; rm -rf ~'],
  ['a backtick', 'a`id`b'],
  ['a $VAR expansion', 'https://$HOST/x'],
  ['a double quote', 'a"b'],
  ['a pipe/amp', 'a|b&c'],
] as [string, string][]) {
  test(`writeEnvFile: single-quotes ${label} literally`, () => {
    withCleanFleetEnv({ FLEETDECK_AGENTS_CMD: val }, () => writeEnvFile());
    const body = fs.readFileSync(ENV_FILE, 'utf8');
    assert.match(
      body,
      new RegExp(`^FLEETDECK_AGENTS_CMD='${escapeRe(val)}'$`, 'm'),
      `${JSON.stringify(val)} must be written single-quoted, verbatim`,
    );
  });
}

// Only what NO quoting reconciles between the two readers is refused. (A NUL
// can't round-trip through process.env — Node truncates env values at NUL — so
// the NUL branch of ENV_VALUE_UNQUOTABLE is asserted directly in the regex test
// below rather than through writeEnvFile here.)
for (const [label, bad] of [
  ['a newline', 'line1\nline2'],
  ['a tab', 'a\tb'],
  ['an embedded single quote', "a'b"],
  ['a backslash (systemd resolves it even inside single quotes)', 'a\\b'],
  ['a trailing backslash (unterminates the systemd quote)', 'a\\'],
] as [string, string][]) {
  test(`writeEnvFile: REJECTS ${label}`, () => {
    assert.throws(
      () => withCleanFleetEnv({ FLEETDECK_TRUSTED_ORIGINS: bad }, () => writeEnvFile()),
      (e: unknown) =>
        e instanceof Error &&
        e.message.includes('FLEETDECK_TRUSTED_ORIGINS') &&
        e.message.includes('unsafe'),
      `value ${JSON.stringify(bad)} must be refused, naming the key`,
    );
  });
}

// BUG-076: writeFileSync's mode option is IGNORED on an existing file, so a
// pre-created permissive service.env must be chmod-repaired by a rewrite —
// otherwise a reinstall leaves FLEETDECK_TOKEN group/world-readable.
test('writeEnvFile: tightens a pre-existing 0644 service.env to 0600 (BUG-076)', (t) => {
  if (process.platform === 'win32') {
    t.skip('POSIX modes');
    return;
  }
  fs.writeFileSync(ENV_FILE, 'FLEETDECK_STALE=1\n', { mode: 0o644 });
  fs.chmodSync(ENV_FILE, 0o644); // writeFileSync mode is best-effort; pin it
  assert.equal(fs.statSync(ENV_FILE).mode & 0o777, 0o644, 'precondition: permissive file on disk');
  withCleanFleetEnv({ FLEETDECK_PORT: '4711' }, () => writeEnvFile());
  assert.equal(
    fs.statSync(ENV_FILE).mode & 0o777,
    0o600,
    'rewrite must repair an existing inode to owner-only',
  );
  fs.unlinkSync(ENV_FILE); // leave no permissive seed for later ENV_FILE tests
});

test('ENV_VALUE_BARE_SAFE stays tight; ENV_VALUE_UNQUOTABLE is minimal (metachars are quotable, not refused)', () => {
  for (const ok of [
    '',
    '4711',
    'trust',
    'https://*.example.com',
    'a,b,c',
    'AbC+/=',
    '/home/dev/.fleetdeck',
    'x@y%z',
  ]) {
    assert.ok(ENV_VALUE_BARE_SAFE.test(ok), `${JSON.stringify(ok)} should be written bare`);
  }
  // metacharacters: not bare-safe (→ single-quoted) but NOT refused (backslash is
  // NOT here — systemd resolves it inside single quotes, so it is unquotable below)
  for (const meta of [
    'a b',
    '$x',
    '`x`',
    'a;b',
    'a|b',
    'a&b',
    'a(b)',
    'a<b',
    'a"b',
    'a{b}',
    'a#b',
    'a~b',
    'a!b',
  ]) {
    assert.ok(!ENV_VALUE_BARE_SAFE.test(meta), `${JSON.stringify(meta)} should not be bare`);
    assert.ok(
      !ENV_VALUE_UNQUOTABLE.test(meta),
      `${JSON.stringify(meta)} should be quotable, not refused`,
    );
  }
  // unquotable: control chars, the single quote, and backslash
  for (const no of ['a\nb', 'a\tb', 'a' + String.fromCharCode(0) + 'b', "a'b", 'a\\b']) {
    assert.ok(ENV_VALUE_UNQUOTABLE.test(no), `${JSON.stringify(no)} should be refused`);
  }
});

// ------------------------------------------------- service.env port reader

// BUG-075: status/start/stop health checks must honor the FLEETDECK_PORT frozen
// into the installed service.env, not just the CLI process's ambient env —
// otherwise a healthy custom-port service is reported down and stop can signal a
// different responder. parseServiceEnvPort is the strict reader; serviceEnvPort
// reads the file it was pointed at.
test('parseServiceEnvPort: reads bare and single-quoted ports, rejects junk', () => {
  assert.equal(parseServiceEnvPort('FLEETDECK_PORT=4733\n'), 4733);
  assert.equal(
    parseServiceEnvPort('FLEETDECK_PORT=4733'),
    4733,
    'no trailing newline still parses',
  );
  assert.equal(
    parseServiceEnvPort("FLEETDECK_PORT='4733'\n"),
    4733,
    'one level of single quotes strips (POSIX/.-source parity)',
  );
  assert.equal(
    parseServiceEnvPort('# comment\nFLEETDECK_TOKEN=abc\nFLEETDECK_PORT=4733\n'),
    4733,
    'finds it among other keys',
  );
  assert.equal(
    parseServiceEnvPort('FLEETDECK_PORT=4733  \n'),
    4733,
    'trailing whitespace is tolerated',
  );
  for (const junk of [
    'FLEETDECK_PORT=abc',
    'FLEETDECK_PORT=',
    'FLEETDECK_PORT=0',
    'FLEETDECK_PORT=65536',
    'FLEETDECK_PORT=1.5',
    'FLEETDECK_PORT=  12x',
    'FLEETDECK_PORT="4733"', // double quotes: written values never use them; refuse to guess
    "FLEETDECK_PORT='47'33'", // quote chars still in the value — a hand-edited file, ignored
    'FLEETDECK_PORT=$(id)', // injection-shaped value
    'export FLEETDECK_PORT=4733', // we never write `export`; a foreign line is not ours to trust
    'OTHER_PORT=4733',
    '',
  ]) {
    assert.equal(parseServiceEnvPort(junk), null, `${JSON.stringify(junk)} must be ignored`);
  }
  assert.equal(parseServiceEnvPort(null), null);
  assert.equal(parseServiceEnvPort(42), null);
});

test('serviceEnvPort: returns the port from the installed file, null when absent/unreadable', () => {
  try {
    fs.unlinkSync(ENV_FILE);
  } catch {
    /* absent */
  }
  assert.equal(serviceEnvPort(), null, 'no service.env (not installed) — the default port applies');
  fs.writeFileSync(ENV_FILE, "FLEETDECK_PORT=4733\nFLEETDECK_TOKEN='tok'\n");
  assert.equal(serviceEnvPort(), 4733);
  fs.writeFileSync(ENV_FILE, 'FLEETDECK_PORT=$(id)\n');
  assert.equal(serviceEnvPort(), null, 'an unparseable file is ignored, never fetched from');
  // Leave no trace: later tests in this file write and assert ENV_FILE contents.
  try {
    fs.unlinkSync(ENV_FILE);
  } catch {
    /* absent */
  }
});

// -------------------------------------------------------- token --rotate

// BUG-076: the same mode-option blind spot in `fleetdeck token --rotate` — a
// pre-existing permissive token file must be chmod-repaired by rotation, or the
// fresh secret stays group/world-readable.
test('token --rotate: tightens a pre-existing 0644 token to 0600 (BUG-076)', async (t) => {
  if (process.platform === 'win32') {
    t.skip('POSIX modes');
    return;
  }
  const TOKEN_FILE = path.join(HOME, 'token');
  fs.writeFileSync(TOKEN_FILE, 'oldstaletoken0123456789abcdef', { mode: 0o644 });
  fs.chmodSync(TOKEN_FILE, 0o644);
  assert.equal(
    fs.statSync(TOKEN_FILE).mode & 0o777,
    0o644,
    'precondition: permissive file on disk',
  );

  const outChunks: string[] = [];
  const write = process.stdout.write.bind(process.stdout);
  process.stdout.write = (s: string | Uint8Array): boolean => {
    outChunks.push(String(s));
    return true;
  };
  let rc: number | undefined;
  try {
    rc = await token(['--rotate']);
  } finally {
    process.stdout.write = write;
  }

  assert.equal(rc, 0);
  assert.equal(
    fs.statSync(TOKEN_FILE).mode & 0o777,
    0o600,
    'rotation must repair an existing inode to owner-only',
  );
  const rotated = fs.readFileSync(TOKEN_FILE, 'utf8').trim();
  assert.match(rotated, /^[0-9a-f]{64}$/, 'a fresh 32-byte hex token was written');
  assert.notEqual(rotated, 'oldstaletoken0123456789abcdef');
  assert.ok(
    outChunks.some((c) => c.includes('token rotated')),
    'success was reported',
  );
});

// ------------------------------------------------------- supervisorAlive

test('supervisorAlive: no pidfile → 0', () => {
  try {
    fs.unlinkSync(SUPERVISOR_PID);
  } catch {
    /* absent */
  }
  assert.equal(supervisorAlive(), 0);
});

test('supervisorAlive: a dead/unused pid → 0', () => {
  // 0x3fffffff is above any real pid on Linux; kill(0) throws ESRCH.
  fs.writeFileSync(SUPERVISOR_PID, '1073741823');
  assert.equal(supervisorAlive(), 0);
});

test('supervisorAlive: a LIVE pid that is not our supervisor → 0 (no false "running")', (t) => {
  if (process.platform !== 'linux') {
    t.skip('identity check is /proc-based; skip off-Linux');
    return;
  }
  // process.pid is alive but its /proc cmdline is the test runner, not SUPERVISE_SH.
  fs.writeFileSync(SUPERVISOR_PID, String(process.pid));
  assert.equal(
    supervisorAlive(),
    0,
    'a stale pidfile pointing at an unrelated live process must not claim running',
  );
});

test('supervisorLooksLikeOurs: false for this (non-supervisor) live process on Linux', (t) => {
  if (process.platform !== 'linux') {
    t.skip('/proc only');
    return;
  }
  assert.equal(supervisorLooksLikeOurs(process.pid), false);
});

// Positive side of the identity check, kept spawn-free: the negative cases above
// already run the real /proc read end-to-end (a live-but-unrelated pid resolves to
// 0). Spawning a real `sh SUPERVISE_SH` here is deliberately avoided — a child that
// outlives the test corrupts node:test's stdout report channel — so the "recognized
// as ours" branch is covered through its pure match rule instead.
test('argvIsOurSupervisor: matches only when SUPERVISE_SH is in the argv', () => {
  assert.equal(argvIsOurSupervisor(['sh', SUPERVISE_SH]), true);
  assert.equal(argvIsOurSupervisor(['/bin/sh', SUPERVISE_SH]), true);
  assert.equal(argvIsOurSupervisor(['node', '/somewhere/else/fleetd.mjs', 'serve']), false);
  assert.equal(argvIsOurSupervisor(['sleep', '30']), false);
  assert.equal(argvIsOurSupervisor([]), false);
  assert.equal(argvIsOurSupervisor(null), false);
});

// -------------------------------------------------------- service start

// BUG-080: a live supervisor wrapper is not a live BOARD. During a fleetd
// crash-loop the wrapper is alive but sleeping in exponential backoff, so the
// old kill(0)-only branch reported "already running" (exit 0) while nothing
// answered on the port — indefinitely. "Started" must mean "answering": the
// existing-supervisor branch must require a managed health response, and
// report a degraded supervisor (nonzero, with the log path) otherwise.
test('serviceStart: live supervisor + dead daemon → nonzero degraded report, not "already running"', async (t) => {
  if (process.platform !== 'linux') {
    t.skip('supervisor identity check is /proc-based; skip off-Linux');
    return;
  }
  // Force the no-systemd branch the same way the serviceInstall test does.
  const savedPath = process.env['PATH'];
  const emptyDir = path.join(TMP, 'nopath');
  fs.mkdirSync(emptyDir, { recursive: true });
  fs.writeFileSync(SUPERVISE_SH, '#!/bin/sh\n', { mode: 0o700 });
  try {
    fs.rmSync(path.dirname(UNIT_FILE), { recursive: true, force: true });
  } catch {
    /* absent */
  }
  // Stand in for the sleeping-in-backoff wrapper: a live process whose argv
  // contains SUPERVISE_SH as its own element, so supervisorAlive() accepts it
  // as ours (argvIsOurSupervisor is an exact-element match). Passing it as the
  // $0 of `sh -c` keeps it in argv WITHOUT executing the script. The child
  // must not outlive the test (an orphan would corrupt node:test's report
  // channel), so it exits on its own after a few seconds even if killed late.
  const { spawn } = await import('node:child_process');
  const fake = spawn('sh', ['-c', 'sleep 5', SUPERVISE_SH], { detached: true, stdio: 'ignore' });
  fake.unref();
  t.after(() => {
    try {
      if (fake.pid !== undefined) process.kill(fake.pid, 'SIGKILL');
    } catch {
      /* already gone */
    }
  });

  process.env['PATH'] = emptyDir;
  let rc: number | undefined;
  try {
    // Written INSIDE the PATH scope: serviceStart must see our live fake
    // wrapper, and the supervisorAlive tests above leave the pidfile deleted.
    fs.writeFileSync(SUPERVISOR_PID, String(fake.pid));
    rc = await serviceStart();
  } finally {
    process.env['PATH'] = savedPath;
  }

  assert.equal(rc, 1, 'a wrapper with no daemon answering is degraded, not success');
  // The pidfile must NOT have been rewritten by a second spawn — the pid of a
  // re-spawned wrapper would differ from our fake one.
  assert.equal(
    fs.readFileSync(SUPERVISOR_PID, 'utf8').trim(),
    String(fake.pid),
    'no second supervisor was spawned over the live one',
  );
});

// ---------------------------------------------------- healthPidIsOurDaemon
// BUG-082: the no-systemd `service stop` used to SIGTERM whatever pid ANY
// health-compatible responder on the port returned. The gate must only accept
// a pid that a managed responder reports AND that matches OUR home's
// fleetd.pid (pid + recorded port) AND that still looks like a live fleetd.

// A live, fleetd-shaped same-user process: `node .../scripts/fleetd/fleetd.ts`.
// Detached + unref'd with piped stdio so it cannot corrupt node:test's report
// channel; every test that spawns one kills it in a finally.
function spawnFakeFleetd() {
  const fleetdPath = path.resolve(import.meta.dirname, '..', 'src', 'daemon', 'fleetd.ts');
  const child = spawn(process.execPath, [fleetdPath], { detached: true, stdio: 'ignore' });
  child.unref();
  return child;
}

test('healthPidIsOurDaemon: accepts only a managed responder whose pid matches fleetd.pid (pid + port) and /proc identity', (t) => {
  if (process.platform !== 'linux') {
    t.skip('identity check is /proc-based; skip off-Linux');
    return;
  }
  const child = spawnFakeFleetd();
  try {
    fs.writeFileSync(FLEETD_PID, JSON.stringify({ pid: child.pid, port: DEAD_PORT }));
    assert.equal(
      healthPidIsOurDaemon(asHealth({ pid: child.pid, managed: true })),
      true,
      'our own managed daemon must pass',
    );
    assert.equal(
      healthPidIsOurDaemon(asHealth({ pid: child.pid, managed: false })),
      false,
      'a plugin-spawned (unmanaged) daemon is not ours to stop',
    );
    assert.equal(
      healthPidIsOurDaemon(asHealth({ pid: child.pid })),
      false,
      'a health body without `managed` is not a managed fleetd',
    );
  } finally {
    try {
      if (child.pid !== undefined) process.kill(child.pid, 'SIGKILL');
    } catch {
      /* already gone */
    }
    try {
      fs.unlinkSync(FLEETD_PID);
    } catch {
      /* absent */
    }
  }
});

test('healthPidIsOurDaemon: rejects a foreign responder whose pid is not in fleetd.pid', (t) => {
  if (process.platform !== 'linux') {
    t.skip('identity check is /proc-based; skip off-Linux');
    return;
  }
  const child = spawnFakeFleetd();
  try {
    // Another installation's daemon recorded in ITS home — our pidfile names a
    // different (dead) pid, so the responder's live pid must NOT be signalled.
    fs.writeFileSync(FLEETD_PID, JSON.stringify({ pid: 1073741823, port: DEAD_PORT }));
    assert.equal(
      healthPidIsOurDaemon(asHealth({ pid: child.pid, managed: true })),
      false,
      'pid not recorded in OUR fleetd.pid',
    );
    // A fake local server claiming the recorded pid of a NON-fleetd live process
    // (this test runner) must also fail the /proc identity leg.
    fs.writeFileSync(FLEETD_PID, JSON.stringify({ pid: process.pid, port: DEAD_PORT }));
    assert.equal(
      healthPidIsOurDaemon(asHealth({ pid: process.pid, managed: true })),
      false,
      'pidfile match but the live process is not a fleetd',
    );
  } finally {
    try {
      if (child.pid !== undefined) process.kill(child.pid, 'SIGKILL');
    } catch {
      /* already gone */
    }
    try {
      fs.unlinkSync(FLEETD_PID);
    } catch {
      /* absent */
    }
  }
});

test('healthPidIsOurDaemon: rejects a recorded port that is not the selected PORT', (t) => {
  if (process.platform !== 'linux') {
    t.skip('identity check is /proc-based; skip off-Linux');
    return;
  }
  const child = spawnFakeFleetd();
  try {
    fs.writeFileSync(FLEETD_PID, JSON.stringify({ pid: child.pid, port: 9999 }));
    assert.equal(
      healthPidIsOurDaemon(asHealth({ pid: child.pid, managed: true })),
      false,
      'fleetd.pid records a different port than the CLI selected',
    );
  } finally {
    try {
      if (child.pid !== undefined) process.kill(child.pid, 'SIGKILL');
    } catch {
      /* already gone */
    }
    try {
      fs.unlinkSync(FLEETD_PID);
    } catch {
      /* absent */
    }
  }
});

test('healthPidIsOurDaemon: no pidfile → false (cannot prove ownership, never kill)', () => {
  try {
    fs.unlinkSync(FLEETD_PID);
  } catch {
    /* absent */
  }
  assert.equal(healthPidIsOurDaemon(asHealth({ pid: process.pid, managed: true })), false);
  assert.equal(healthPidIsOurDaemon(null), false);
  assert.equal(healthPidIsOurDaemon(asHealth({})), false);
  assert.equal(healthPidIsOurDaemon(asHealth({ pid: 'not-a-number', managed: true })), false);
});

// -------------------------------------------------------- service install

test('serviceInstall (no-systemd path): writes 0700 supervise.sh + 0600 env, no unit', async () => {
  // Force the no-systemd branch deterministically: with PATH pointing at an empty
  // dir, hasSystemd()'s `systemctl` lookup fails ENOENT. This writes files only —
  // it does NOT start the supervisor, so no daemon and no tmux are involved.
  const savedPath = process.env['PATH'];
  const emptyDir = path.join(TMP, 'nopath');
  fs.mkdirSync(emptyDir, { recursive: true });
  // Fresh inode: a permissive ENV_FILE left by an earlier test would survive the
  // write on UNFIXED code and fail the 0600 assertion for the wrong reason.
  try {
    fs.unlinkSync(ENV_FILE);
  } catch {
    /* absent */
  }
  try {
    fs.unlinkSync(SUPERVISE_SH);
  } catch {
    /* absent */
  }
  try {
    fs.rmSync(path.dirname(UNIT_FILE), { recursive: true, force: true });
  } catch {
    /* absent */
  }

  const rc = await withCleanFleetEnv(
    {
      FLEETDECK_HOME: HOME,
      FLEETDECK_PORT: '4711',
      FLEETDECK_TRUSTED_ORIGINS: 'https://*.example.com',
    },
    async () => {
      process.env['PATH'] = emptyDir;
      try {
        return await serviceInstall();
      } finally {
        process.env['PATH'] = savedPath;
      }
    },
  );

  assert.equal(rc, 0);
  assert.ok(fs.existsSync(SUPERVISE_SH), 'the supervised wrapper is written');
  assert.equal(fs.statSync(SUPERVISE_SH).mode & 0o777, 0o700, 'SUPERVISE_SH must be 0700');
  assert.ok(!fs.existsSync(UNIT_FILE), 'the systemd unit is NOT written on the no-systemd path');

  const sh = fs.readFileSync(SUPERVISE_SH, 'utf8');
  assert.match(sh, /^#!\/bin\/sh$/m, 'shebang');
  assert.ok(sh.includes(ENV_FILE), 'sources the frozen env file');
  assert.ok(sh.includes('serve'), 'execs `fleetdeck serve`');
  assert.ok(
    sh.includes('-eq 3 ] && exit 3'),
    'declines to respawn on exit 3 (port lost — hot loop)',
  );

  const env = fs.readFileSync(ENV_FILE, 'utf8');
  assert.equal(fs.statSync(ENV_FILE).mode & 0o777, 0o600);
  assert.match(env, /^FLEETDECK_PORT=4711$/m);
});

// ------------------------------------------------- UNIT / SUPERVISE generators

test('UNIT(): a well-formed systemd user unit that execs `serve` and guards exit 3', () => {
  const u = UNIT();
  assert.match(u, /^\[Unit\]$/m);
  assert.match(u, /^\[Service\]$/m);
  assert.ok(u.includes(`EnvironmentFile=-${ENV_FILE}`), 'optional env file (leading -)');
  assert.ok(u.includes('serve'), 'ExecStart runs `serve`');
  assert.match(u, /^Restart=always$/m);
  assert.match(u, /^RestartPreventExitStatus=3$/m, 'does not hot-loop on the port-lost exit');
  assert.match(u, /^WantedBy=default\.target$/m);
});

test('quoteExecArg(): systemd-quotes ExecStart args so spaced/percent paths survive (BUG-078)', () => {
  // systemd splits a bare ExecStart token on whitespace, so a legal Node path
  // with a space was truncated at the first space and the service could not
  // start. Quoting must also neutralize `%` (specifier expansion applies to
  // every unit-file setting, quoted or not) and an embedded single quote
  // (which would otherwise start systemd's single-quote mode and swallow the
  // closing double quote).
  assert.equal(quoteExecArg('/usr/bin/node'), '"/usr/bin/node"', 'plain path: quoted, unchanged');
  assert.equal(
    quoteExecArg('/tmp/node space/node'),
    '"/tmp/node space/node"',
    'a spaced path stays ONE argument inside double quotes',
  );
  assert.equal(
    quoteExecArg('/opt/100%/node'),
    '"/opt/100%%/node"',
    'a literal percent is escaped as %% (specifier expansion runs even inside quotes)',
  );
  assert.equal(
    quoteExecArg("/opt/it's here/node"),
    String.raw`"/opt/it\'s here/node"`,
    'an embedded single quote is escaped so it cannot swallow the closing double quote',
  );
  assert.throws(
    () => quoteExecArg('/tmp/node\nspace/node'),
    /cannot be represented/,
    'newline refused',
  );
  assert.throws(
    () => quoteExecArg('/tmp/node"quote/node'),
    /cannot be represented/,
    'double quote refused',
  );

  // The generated unit quotes BOTH executable and script path. (The real
  // process.execPath / HERE on this machine may already contain spaces, so
  // assert the quoting invariant rather than exact paths.)
  const execLine = UNIT()
    .split('\n')
    .find((l) => l.startsWith('ExecStart='));
  assert.ok(execLine !== undefined, 'the generated unit has an ExecStart line');
  assert.match(
    execLine,
    /^ExecStart="[^"]*" "[^"]*" serve$/,
    'both paths double-quoted, serve bare',
  );
});

test('SUPERVISE(): sources the env file safely and backs off, never respawning a clean exit', () => {
  const s = SUPERVISE();
  assert.ok(s.includes(`. ${shQuote(ENV_FILE)}`), 'dot-sources the env file inside set -a/set +a');
  assert.match(s, /set -a; \. '/, 'exports while sourcing so children inherit config');
  assert.ok(s.includes('-eq 0 ] && exit 0'), 'a clean SIGTERM shutdown is not respawned');
  assert.ok(s.includes('-eq 3 ] && exit 3'), 'a lost-port exit is not hot-looped');
  assert.ok(s.includes('serve'), 'execs `fleetdeck serve`');
});

// -------------------------------------------- systemd unit path escaping (BUG-077)
//
// The unit's two path-bearing directives interpolate REAL paths (node binary,
// this script, FLEETDECK_HOME/service.env). Written bare, systemd word-splits a
// spaced path, treats quotes as syntax, and expands `%` as a specifier — a
// valid install then fails to start or reads the wrong env file.

test('unitEscape: doubles every % so systemd specifier expansion is a no-op', () => {
  assert.equal(unitEscape('/opt/100%/fleet deck'), '/opt/100%%/fleet deck');
  assert.equal(unitEscape('%i%h%%'), '%%i%%h%%%%');
  assert.equal(unitEscape('/plain/path'), '/plain/path', 'no % → unchanged');
});

test('unitArg: one literal argv word per token, whatever the path contains', () => {
  // bare-safe paths stay byte-identical to older installs
  assert.equal(unitArg('/usr/bin/node'), '/usr/bin/node');
  assert.equal(unitArg('/home/dev/.fleetdeck'), '/home/dev/.fleetdeck');
  // a spaced path must be quoted so systemd keeps it ONE argv element
  assert.equal(unitArg('/opt/fleet deck/bin/node'), '"/opt/fleet deck/bin/node"');
  // % is escaped BEFORE the bare/quoted decision (specifier expansion precedes
  // tokenization); a %-only path is still one bare word, just with %% doubling
  assert.equal(unitArg('/opt/100%/node'), '/opt/100%%/node');
  assert.equal(unitArg('/opt/100% dir/node'), '"/opt/100%% dir/node"');
  // embedded double quotes and backslashes are escaped inside the quotes
  assert.equal(unitArg('/opt/we"ird/node'), '"/opt/we\\"ird/node"');
  assert.equal(
    unitArg('C:\\Program Files\\node\\node.exe'),
    '"C:\\\\Program Files\\\\node\\\\node.exe"',
  );
});

test('unitEnvFilePath: literal paths pass through (%-escaped); whitespace/quotes/backslash are refused', () => {
  assert.equal(
    unitEnvFilePath('/home/dev/.fleetdeck/service.env'),
    '/home/dev/.fleetdeck/service.env',
  );
  assert.equal(
    unitEnvFilePath('/home/100%/.fleetdeck/service.env'),
    '/home/100%%/.fleetdeck/service.env',
  );
  for (const bad of [
    '/home/fleet deck/service.env',
    '/home/we"ird/service.env',
    "/home/we'ird/service.env",
    '/home/we\\ird/service.env',
  ]) {
    assert.throws(
      () => unitEnvFilePath(bad),
      (e: unknown) =>
        e instanceof Error &&
        e.message.includes('EnvironmentFile') &&
        e.message.includes('FLEETDECK_HOME'),
      `${JSON.stringify(bad)} must be refused with an install-time error naming the knob`,
    );
  }
});

test('UNIT(): normal paths need no %-escaping; ExecStart is quoted (BUG-078 supersedes the old bare form)', () => {
  const u = UNIT();
  assert.ok(
    u.includes(`EnvironmentFile=-${ENV_FILE}`),
    'env file directive unchanged for a normal path',
  );
  // BUG-078 always double-quotes the ExecStart tokens (hostile-path safety), so
  // even a safe node path is quoted — this supersedes BUG-077's "stays bare".
  assert.ok(u.includes(`ExecStart="${process.execPath}" `), 'node binary is double-quoted');
  assert.doesNotMatch(u, /%%/, 'no %-escaping noise when no path needs it');
});

// BUG-079: the wrapper embeds ENV_FILE, the Node executable, and the CLI path.
// Inside DOUBLE quotes, $(), backticks, and $VAR in those paths stay live shell
// syntax — a literal path like `$(printf injected)` was EXECUTED when the
// wrapper resolved it. Every embedded path must be single-quoted instead, so
// nothing in it expands.
test('shQuote: single-quotes a path so no shell metacharacter expands', () => {
  assert.equal(shQuote('/plain/path'), `'/plain/path'`);
  assert.equal(shQuote('$(printf injected)'), `'$(printf injected)'`);
  assert.equal(shQuote('/home/o`id`brien/x'), "'/home/o`id`brien/x'");
  assert.equal(
    shQuote("/it's/a path"),
    `'/it'\\''s/a path'`,
    "embedded single quote uses the '\"'\"' idiom",
  );
});

test('SUPERVISE(): embeds paths single-quoted — $(), backticks, and quotes in a path stay literal', () => {
  const s = SUPERVISE();
  // The generated script must not hand any embedded path to the shell inside
  // double quotes, where $(...)/`...`/$VAR would still expand.
  assert.doesNotMatch(s, /"\$\{/, 'no "${...}" double-quoted interpolation of embedded paths');
  for (const embedded of [ENV_FILE, process.execPath]) {
    assert.ok(s.includes(shQuote(embedded)), `${embedded} appears single-quoted`);
  }
  // The two reader lines for the env file: existence test and dot-source.
  assert.ok(s.includes(`[ -f ${shQuote(ENV_FILE)} ]`), 'existence test is single-quoted');
  assert.ok(s.includes(`. ${shQuote(ENV_FILE)};`), 'dot-source is single-quoted');
  // The exec line: node and the CLI path are both single-quoted, serve stays bare.
  const CLI = path.resolve(
    path.dirname(new URL(import.meta.url).pathname),
    '..',
    'bin',
    'fleetdeck.mjs',
  );
  assert.match(
    s,
    new RegExp(`^  ${escapeRe(shQuote(process.execPath))} ${escapeRe(shQuote(CLI))} serve &`, 'm'),
    'exec line single-quotes both paths',
  );
});

// The exact BUG-079 trigger, end to end: an ENV_FILE whose path contains
// `$(printf injected)` must reach the shell as ONE literal word — sourcing it
// must not execute the substitution. This runs real /bin/sh on a stubbed
// wrapper line.
test('SUPERVISE() quoting: a $(...) path is sourced literally, never executed', (t) => {
  if (process.platform === 'win32') {
    t.skip('POSIX shell test');
    return;
  }
  const evil = path.join(TMP, 'home $(printf injected > INJECTION_MARK)');
  fs.mkdirSync(evil, { recursive: true });
  fs.writeFileSync(path.join(evil, 'service.env'), 'FLEETDECK_PORT=4711\n');
  const envFile = path.join(evil, 'service.env');
  // Mirror the wrapper's source line with the hostile path quoted by shQuote.
  const script = `[ -f ${shQuote(envFile)} ] && { set -a; . ${shQuote(envFile)}; set +a; }\nprintf '%s' "$FLEETDECK_PORT"\n`;
  const scriptFile = path.join(TMP, 'probe.sh');
  fs.writeFileSync(scriptFile, script, { mode: 0o700 });
  const r = spawnSync('sh', [scriptFile], { encoding: 'utf8', cwd: TMP });
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.stdout, '4711', 'the env file at the metachar path is sourced literally');
  assert.ok(
    !fs.existsSync(path.join(TMP, 'INJECTION_MARK')),
    'the $(printf injected) in the path was NOT executed',
  );
});

test('healthIsOurManagedDaemon: an unmanaged health answer is refused (BUG-081)', async () => {
  // The core of BUG-081: a responder without the managed marker must never be
  // accepted as the managed service, regardless of pid/home proof.
  assert.equal(await healthIsOurManagedDaemon(null), false);
  assert.equal(await healthIsOurManagedDaemon(asHealth({})), false);
  assert.equal(await healthIsOurManagedDaemon(asHealth({ pid: process.pid })), false);
  assert.equal(await healthIsOurManagedDaemon(asHealth({ managed: 0, pid: process.pid })), false);
  assert.equal(
    await healthIsOurManagedDaemon(asHealth({ managed: false, pid: process.pid })),
    false,
  );
});
