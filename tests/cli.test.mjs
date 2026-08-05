// tests/cli.test.mjs
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

import test, { after } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const TMP = fs.mkdtempSync(path.join(os.tmpdir(), 'fleetdeck-cli-'));
const HOME = path.join(TMP, 'home');
const XDG = path.join(TMP, 'config');
process.env.FLEETDECK_HOME = HOME;
process.env.XDG_CONFIG_HOME = XDG;
fs.mkdirSync(HOME, { recursive: true });

const ENV_FILE = path.join(HOME, 'service.env');
const SUPERVISE_SH = path.join(HOME, 'supervise.sh');
const SUPERVISOR_PID = path.join(HOME, 'supervisor.pid');
const UNIT_FILE = path.join(XDG, 'systemd', 'user', 'fleetdeck.service');

const {
<<<<<<< /tmp/mf-ours
<<<<<<< /tmp/mf-ours
  writeEnvFile, ENV_VALUE_BARE_SAFE, ENV_VALUE_UNQUOTABLE, supervisorAlive, supervisorLooksLikeOurs, argvIsOurSupervisor,
<<<<<<< /tmp/mf-ours
<<<<<<< /tmp/mf-ours
<<<<<<< /tmp/mf-ours
  serviceInstall, UNIT, SUPERVISE, MIN_NODE_RANGE, nodeVersionSupported,
=======
  writeEnvFile, ENV_VALUE_BARE_SAFE, ENV_VALUE_UNQUOTABLE, parseServiceEnvPort, serviceEnvPort,
  supervisorAlive, supervisorLooksLikeOurs, argvIsOurSupervisor,
=======
  writeEnvFile, ENV_VALUE_BARE_SAFE, ENV_VALUE_UNQUOTABLE, shQuote, supervisorAlive, supervisorLooksLikeOurs, argvIsOurSupervisor,
>>>>>>> /tmp/mf-theirs
  serviceInstall, UNIT, SUPERVISE,
>>>>>>> /tmp/mf-theirs
=======
  serviceInstall, UNIT, SUPERVISE, token,
>>>>>>> /tmp/mf-theirs
=======
  serviceInstall, UNIT, SUPERVISE, unitEscape, unitArg, unitEnvFilePath,
>>>>>>> /tmp/mf-theirs
=======
  serviceInstall, UNIT, SUPERVISE, quoteExecArg,
>>>>>>> /tmp/mf-theirs
} = await import(new URL('../bin/fleetdeck.mjs', import.meta.url));
const { parseTmuxVersion, tmuxVersionCapability, tmuxVersionSupported } = await import(new URL('../bin/tmux-version.mjs', import.meta.url));

after(() => { try { fs.rmSync(TMP, { recursive: true, force: true }); } catch { /* best effort */ } });

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
  assert.match(tmuxVersionCapability('unknown').reason, /version is unknown/);
});

// BUG-020: node:sqlite loads WITHOUT --experimental-sqlite only from 22.13.0
// (and 24.x), so the declared floor must exclude 22.5–22.12 — those versions
// satisfy the old >=22.5 engine range yet die with ERR_UNKNOWN_BUILTIN_MODULE
// before fleetd opens its listener.
test('node engine floor rejects 22.5–22.12 and Node 23, accepts 22.13+ and 24+', () => {
  assert.equal(nodeVersionSupported('22.5.1'), false, '22.5 was the old floor but cannot load node:sqlite unflagged');
  assert.equal(nodeVersionSupported('22.12.0'), false, 'last flagged 22.x is still too old');
  assert.equal(nodeVersionSupported('22.13.0'), true, 'first unflagged 22.x');
  assert.equal(nodeVersionSupported('22.18.0'), true);
  assert.equal(nodeVersionSupported('23.0.0'), false, 'the odd 23 line is unsupported');
  assert.equal(nodeVersionSupported('24.0.0'), true);
  assert.equal(nodeVersionSupported('25.1.0'), true);
  assert.equal(nodeVersionSupported('21.7.3'), false);
  assert.equal(nodeVersionSupported('not-a-version'), false);
});

test('node engine floor matches the declared package.json engines range', () => {
  const pkg = JSON.parse(fs.readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
  assert.equal(pkg.engines.node, MIN_NODE_RANGE, 'doctor text and engines must not drift apart');
  assert.equal(MIN_NODE_RANGE, '^22.13.0 || >=24.0.0');
});

// Save/clear every FLEETDECK_* var (so a stray one in the ambient environment
// cannot skew a writeEnvFile test), then restore. FLEETDECK_HOME is cleared too,
// but the module already captured its HOME constant at import, so file locations
// are unaffected.
function withCleanFleetEnv(vars, fn) {
  const saved = {};
  for (const k of Object.keys(process.env)) {
    if (k.startsWith('FLEETDECK_')) { saved[k] = process.env[k]; delete process.env[k]; }
  }
  try {
    for (const [k, v] of Object.entries(vars)) process.env[k] = v;
    return fn();
  } finally {
    for (const k of Object.keys(process.env)) if (k.startsWith('FLEETDECK_')) delete process.env[k];
    for (const [k, v] of Object.entries(saved)) process.env[k] = v;
  }
}

// ------------------------------------------------------------- writeEnvFile

test('writeEnvFile: writes valid FLEETDECK_* values, 0600, KEY=value per line', () => {
  const n = withCleanFleetEnv({
    FLEETDECK_PORT: '4711',
    FLEETDECK_PROXY_AUTH: 'trust',
    FLEETDECK_TRUSTED_ORIGINS: 'https://*.coder.example.com,https://fleetdeck--luis--dev--main.example.com',
    FLEETDECK_TOKEN: 'AbC123+/def456ghi789==', // base64-shaped token: + / = are allowed
    FLEETDECK_MANAGED: '1', // must be skipped — owned by `serve`, never config
  }, () => writeEnvFile());

  assert.equal(n, 4, 'MANAGED is excluded, the other four are written');
  const body = fs.readFileSync(ENV_FILE, 'utf8');
  assert.match(body, /^FLEETDECK_PORT=4711$/m);
  assert.match(body, /^FLEETDECK_PROXY_AUTH=trust$/m);
  assert.match(body, /^FLEETDECK_TRUSTED_ORIGINS=https:\/\/\*\.coder\.example\.com,https:\/\/fleetdeck--luis--dev--main\.example\.com$/m);
  assert.match(body, /^FLEETDECK_TOKEN=AbC123\+\/def456ghi789==$/m);
  assert.doesNotMatch(body, /FLEETDECK_MANAGED/, 'MANAGED must never be persisted');
  assert.equal(fs.statSync(ENV_FILE).mode & 0o777, 0o600, 'a token may live here — owner-only');
});

// A legitimately-spaced command knob (FLEETDECK_AGENTS_CMD, documented in the
// README + read by agents-poll.mjs) must SURVIVE install, single-quoted so the
// shell `.`-source keeps it one literal token instead of word-splitting it.
test('writeEnvFile: single-quotes the documented spaced command knob (FLEETDECK_AGENTS_CMD)', () => {
  const n = withCleanFleetEnv({ FLEETDECK_AGENTS_CMD: 'claude agents --json' }, () => writeEnvFile());
  assert.equal(n, 1);
  const body = fs.readFileSync(ENV_FILE, 'utf8');
  assert.match(body, /^FLEETDECK_AGENTS_CMD='claude agents --json'$/m, 'spaced command single-quoted, not split or refused');
});

// Shell metacharacters are ACCEPTED and single-quoted — literal to BOTH the
// `.`-source and systemd EnvironmentFile (a single-quoted RHS gets no expansion
// in either), so the divergence the bare path guards against cannot arise.
const escapeRe = s => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
for (const [label, val] of [
  ['a $() command substitution', '$(id)'],
  ['a ; command separator', 'a; rm -rf ~'],
  ['a backtick', 'a`id`b'],
  ['a $VAR expansion', 'https://$HOST/x'],
  ['a double quote', 'a"b'],
  ['a pipe/amp', 'a|b&c'],
]) {
  test(`writeEnvFile: single-quotes ${label} literally`, () => {
    withCleanFleetEnv({ FLEETDECK_AGENTS_CMD: val }, () => writeEnvFile());
    const body = fs.readFileSync(ENV_FILE, 'utf8');
    assert.match(body, new RegExp(`^FLEETDECK_AGENTS_CMD='${escapeRe(val)}'$`, 'm'),
      `${JSON.stringify(val)} must be written single-quoted, verbatim`);
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
]) {
  test(`writeEnvFile: REJECTS ${label}`, () => {
    assert.throws(
      () => withCleanFleetEnv({ FLEETDECK_TRUSTED_ORIGINS: bad }, () => writeEnvFile()),
      (e) => e instanceof Error && /FLEETDECK_TRUSTED_ORIGINS/.test(e.message) && /unsafe/.test(e.message),
      `value ${JSON.stringify(bad)} must be refused, naming the key`,
    );
  });
}

// BUG-076: writeFileSync's mode option is IGNORED on an existing file, so a
// pre-created permissive service.env must be chmod-repaired by a rewrite —
// otherwise a reinstall leaves FLEETDECK_TOKEN group/world-readable.
test('writeEnvFile: tightens a pre-existing 0644 service.env to 0600 (BUG-076)', (t) => {
  if (process.platform === 'win32') return t.skip('POSIX modes');
  fs.writeFileSync(ENV_FILE, 'FLEETDECK_STALE=1\n', { mode: 0o644 });
  fs.chmodSync(ENV_FILE, 0o644); // writeFileSync mode is best-effort; pin it
  assert.equal(fs.statSync(ENV_FILE).mode & 0o777, 0o644, 'precondition: permissive file on disk');
  withCleanFleetEnv({ FLEETDECK_PORT: '4711' }, () => writeEnvFile());
  assert.equal(fs.statSync(ENV_FILE).mode & 0o777, 0o600, 'rewrite must repair an existing inode to owner-only');
  fs.unlinkSync(ENV_FILE); // leave no permissive seed for later ENV_FILE tests
});

test('ENV_VALUE_BARE_SAFE stays tight; ENV_VALUE_UNQUOTABLE is minimal (metachars are quotable, not refused)', () => {
  for (const ok of ['', '4711', 'trust', 'https://*.example.com', 'a,b,c', 'AbC+/=', '/home/dev/.fleetdeck', 'x@y%z']) {
    assert.ok(ENV_VALUE_BARE_SAFE.test(ok), `${JSON.stringify(ok)} should be written bare`);
  }
  // metacharacters: not bare-safe (→ single-quoted) but NOT refused (backslash is
  // NOT here — systemd resolves it inside single quotes, so it is unquotable below)
  for (const meta of ['a b', '$x', '`x`', 'a;b', 'a|b', 'a&b', 'a(b)', 'a<b', 'a"b', 'a{b}', 'a#b', 'a~b', 'a!b']) {
    assert.ok(!ENV_VALUE_BARE_SAFE.test(meta), `${JSON.stringify(meta)} should not be bare`);
    assert.ok(!ENV_VALUE_UNQUOTABLE.test(meta), `${JSON.stringify(meta)} should be quotable, not refused`);
  }
  // unquotable: control chars, the single quote, and backslash
  for (const no of ['a\nb', 'a\tb', ('a' + String.fromCharCode(0) + 'b'), "a'b", 'a\\b']) {
    assert.ok(ENV_VALUE_UNQUOTABLE.test(no), `${JSON.stringify(no)} should be refused`);
  }
});

<<<<<<< /tmp/mf-ours
// ------------------------------------------------- service.env port reader

// BUG-075: status/start/stop health checks must honor the FLEETDECK_PORT frozen
// into the installed service.env, not just the CLI process's ambient env —
// otherwise a healthy custom-port service is reported down and stop can signal a
// different responder. parseServiceEnvPort is the strict reader; serviceEnvPort
// reads the file it was pointed at.
test('parseServiceEnvPort: reads bare and single-quoted ports, rejects junk', () => {
  assert.equal(parseServiceEnvPort('FLEETDECK_PORT=4733\n'), 4733);
  assert.equal(parseServiceEnvPort('FLEETDECK_PORT=4733'), 4733, 'no trailing newline still parses');
  assert.equal(parseServiceEnvPort("FLEETDECK_PORT='4733'\n"), 4733, 'one level of single quotes strips (POSIX/.-source parity)');
  assert.equal(parseServiceEnvPort('# comment\nFLEETDECK_TOKEN=abc\nFLEETDECK_PORT=4733\n'), 4733, 'finds it among other keys');
  assert.equal(parseServiceEnvPort('FLEETDECK_PORT=4733  \n'), 4733, 'trailing whitespace is tolerated');
  for (const junk of [
    'FLEETDECK_PORT=abc',
    'FLEETDECK_PORT=',
    'FLEETDECK_PORT=0',
    'FLEETDECK_PORT=65536',
    'FLEETDECK_PORT=1.5',
    'FLEETDECK_PORT=  12x',
    'FLEETDECK_PORT="4733"',      // double quotes: written values never use them; refuse to guess
    "FLEETDECK_PORT='47'33'",      // quote chars still in the value — a hand-edited file, ignored
    'FLEETDECK_PORT=$(id)',        // injection-shaped value
    'export FLEETDECK_PORT=4733',  // we never write `export`; a foreign line is not ours to trust
    'OTHER_PORT=4733',
    '',
  ]) {
    assert.equal(parseServiceEnvPort(junk), null, `${JSON.stringify(junk)} must be ignored`);
  }
  assert.equal(parseServiceEnvPort(null), null);
  assert.equal(parseServiceEnvPort(42), null);
});

test('serviceEnvPort: returns the port from the installed file, null when absent/unreadable', () => {
  try { fs.unlinkSync(ENV_FILE); } catch { /* absent */ }
  assert.equal(serviceEnvPort(), null, 'no service.env (not installed) — the default port applies');
  fs.writeFileSync(ENV_FILE, "FLEETDECK_PORT=4733\nFLEETDECK_TOKEN='tok'\n");
  assert.equal(serviceEnvPort(), 4733);
  fs.writeFileSync(ENV_FILE, 'FLEETDECK_PORT=$(id)\n');
  assert.equal(serviceEnvPort(), null, 'an unparseable file is ignored, never fetched from');
  // Leave no trace: later tests in this file write and assert ENV_FILE contents.
  try { fs.unlinkSync(ENV_FILE); } catch { /* absent */ }
=======
// -------------------------------------------------------- token --rotate

// BUG-076: the same mode-option blind spot in `fleetdeck token --rotate` — a
// pre-existing permissive token file must be chmod-repaired by rotation, or the
// fresh secret stays group/world-readable.
test('token --rotate: tightens a pre-existing 0644 token to 0600 (BUG-076)', async (t) => {
  if (process.platform === 'win32') return t.skip('POSIX modes');
  const TOKEN_FILE = path.join(HOME, 'token');
  fs.writeFileSync(TOKEN_FILE, 'oldstaletoken0123456789abcdef', { mode: 0o644 });
  fs.chmodSync(TOKEN_FILE, 0o644);
  assert.equal(fs.statSync(TOKEN_FILE).mode & 0o777, 0o644, 'precondition: permissive file on disk');

  const outChunks = [];
  const write = process.stdout.write;
  process.stdout.write = (s) => { outChunks.push(String(s)); return true; };
  let rc;
  try { rc = await token(['--rotate']); }
  finally { process.stdout.write = write; }

  assert.equal(rc, 0);
  assert.equal(fs.statSync(TOKEN_FILE).mode & 0o777, 0o600, 'rotation must repair an existing inode to owner-only');
  const rotated = fs.readFileSync(TOKEN_FILE, 'utf8').trim();
  assert.match(rotated, /^[0-9a-f]{64}$/, 'a fresh 32-byte hex token was written');
  assert.notEqual(rotated, 'oldstaletoken0123456789abcdef');
  assert.ok(outChunks.some(c => c.includes('token rotated')), 'success was reported');
>>>>>>> /tmp/mf-theirs
});

// ------------------------------------------------------- supervisorAlive

test('supervisorAlive: no pidfile → 0', () => {
  try { fs.unlinkSync(SUPERVISOR_PID); } catch { /* absent */ }
  assert.equal(supervisorAlive(), 0);
});

test('supervisorAlive: a dead/unused pid → 0', () => {
  // 0x3fffffff is above any real pid on Linux; kill(0) throws ESRCH.
  fs.writeFileSync(SUPERVISOR_PID, '1073741823');
  assert.equal(supervisorAlive(), 0);
});

test('supervisorAlive: a LIVE pid that is not our supervisor → 0 (no false "running")', (t) => {
  if (process.platform !== 'linux') return t.skip('identity check is /proc-based; skip off-Linux');
  // process.pid is alive but its /proc cmdline is the test runner, not SUPERVISE_SH.
  fs.writeFileSync(SUPERVISOR_PID, String(process.pid));
  assert.equal(supervisorAlive(), 0, 'a stale pidfile pointing at an unrelated live process must not claim running');
});

test('supervisorLooksLikeOurs: false for this (non-supervisor) live process on Linux', (t) => {
  if (process.platform !== 'linux') return t.skip('/proc only');
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

// -------------------------------------------------------- service install

test('serviceInstall (no-systemd path): writes 0700 supervise.sh + 0600 env, no unit', async (t) => {
  // Force the no-systemd branch deterministically: with PATH pointing at an empty
  // dir, hasSystemd()'s `systemctl` lookup fails ENOENT. This writes files only —
  // it does NOT start the supervisor, so no daemon and no tmux are involved.
  const savedPath = process.env.PATH;
  const emptyDir = path.join(TMP, 'nopath');
  fs.mkdirSync(emptyDir, { recursive: true });
  // Fresh inode: a permissive ENV_FILE left by an earlier test would survive the
  // write on UNFIXED code and fail the 0600 assertion for the wrong reason.
  try { fs.unlinkSync(ENV_FILE); } catch { /* absent */ }
  try { fs.unlinkSync(SUPERVISE_SH); } catch { /* absent */ }
  try { fs.rmSync(path.dirname(UNIT_FILE), { recursive: true, force: true }); } catch { /* absent */ }

  const rc = await withCleanFleetEnv({
    FLEETDECK_HOME: HOME,
    FLEETDECK_PORT: '4711',
    FLEETDECK_TRUSTED_ORIGINS: 'https://*.example.com',
  }, async () => {
    process.env.PATH = emptyDir;
    try { return await serviceInstall(); }
    finally { process.env.PATH = savedPath; }
  });

  assert.equal(rc, 0);
  assert.ok(fs.existsSync(SUPERVISE_SH), 'the supervised wrapper is written');
  assert.equal(fs.statSync(SUPERVISE_SH).mode & 0o777, 0o700, 'SUPERVISE_SH must be 0700');
  assert.ok(!fs.existsSync(UNIT_FILE), 'the systemd unit is NOT written on the no-systemd path');

  const sh = fs.readFileSync(SUPERVISE_SH, 'utf8');
  assert.match(sh, /^#!\/bin\/sh$/m, 'shebang');
  assert.ok(sh.includes(ENV_FILE), 'sources the frozen env file');
  assert.ok(sh.includes('serve'), 'execs `fleetdeck serve`');
  assert.ok(sh.includes('-eq 3 ] && exit 3'), 'declines to respawn on exit 3 (port lost — hot loop)');

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
  assert.throws(() => quoteExecArg('/tmp/node\nspace/node'), /cannot be represented/, 'newline refused');
  assert.throws(() => quoteExecArg('/tmp/node"quote/node'), /cannot be represented/, 'double quote refused');

  // The generated unit quotes BOTH executable and script path. (The real
  // process.execPath / HERE on this machine may already contain spaces, so
  // assert the quoting invariant rather than exact paths.)
  const execLine = UNIT().split('\n').find((l) => l.startsWith('ExecStart='));
  assert.match(execLine, /^ExecStart="[^"]*" "[^"]*" serve$/, 'both paths double-quoted, serve bare');
});

test('SUPERVISE(): sources the env file safely and backs off, never respawning a clean exit', () => {
  const s = SUPERVISE();
  assert.ok(s.includes(`. ${shQuote(ENV_FILE)}`), 'dot-sources the env file inside set -a/set +a');
  assert.match(s, /set -a; \. '/, 'exports while sourcing so children inherit config');
  assert.ok(s.includes('-eq 0 ] && exit 0'), 'a clean SIGTERM shutdown is not respawned');
  assert.ok(s.includes('-eq 3 ] && exit 3'), 'a lost-port exit is not hot-looped');
  assert.ok(s.includes('serve'), 'execs `fleetdeck serve`');
});

<<<<<<< /tmp/mf-ours
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
  assert.equal(unitArg('C:\\Program Files\\node\\node.exe'), '"C:\\\\Program Files\\\\node\\\\node.exe"');
});

test('unitEnvFilePath: literal paths pass through (%-escaped); whitespace/quotes/backslash are refused', () => {
  assert.equal(unitEnvFilePath('/home/dev/.fleetdeck/service.env'), '/home/dev/.fleetdeck/service.env');
  assert.equal(unitEnvFilePath('/home/100%/.fleetdeck/service.env'), '/home/100%%/.fleetdeck/service.env');
  for (const bad of ['/home/fleet deck/service.env', '/home/we"ird/service.env', "/home/we'ird/service.env", '/home/we\\ird/service.env']) {
    assert.throws(
      () => unitEnvFilePath(bad),
      (e) => e instanceof Error && /EnvironmentFile/.test(e.message) && /FLEETDECK_HOME/.test(e.message),
      `${JSON.stringify(bad)} must be refused with an install-time error naming the knob`,
    );
  }
});

test('UNIT(): normal paths stay byte-identical to the pre-fix unit (bare, no % present)', () => {
  const u = UNIT();
  assert.ok(u.includes(`EnvironmentFile=-${ENV_FILE}`), 'env file directive unchanged for a normal path');
  assert.ok(u.includes(`ExecStart=${process.execPath} `), 'node binary stays bare when the path is safe');
  assert.doesNotMatch(u, /%%/, 'no escaping noise when no path needs it');
=======
// BUG-079: the wrapper embeds ENV_FILE, the Node executable, and the CLI path.
// Inside DOUBLE quotes, $(), backticks, and $VAR in those paths stay live shell
// syntax — a literal path like `$(printf injected)` was EXECUTED when the
// wrapper resolved it. Every embedded path must be single-quoted instead, so
// nothing in it expands.
test('shQuote: single-quotes a path so no shell metacharacter expands', () => {
  assert.equal(shQuote('/plain/path'), `'/plain/path'`);
  assert.equal(shQuote('$(printf injected)'), `'$(printf injected)'`);
  assert.equal(shQuote('/home/o`id`brien/x'), "'/home/o`id`brien/x'");
  assert.equal(shQuote("/it's/a path"), `'/it'\\''s/a path'`, 'embedded single quote uses the \'"\'"\' idiom');
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
  const CLI = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..', 'bin', 'fleetdeck.mjs');
  assert.match(s, new RegExp(`^  ${escapeRe(shQuote(process.execPath))} ${escapeRe(shQuote(CLI))} serve &`, 'm'),
    'exec line single-quotes both paths');
});

// The exact BUG-079 trigger, end to end: an ENV_FILE whose path contains
// `$(printf injected)` must reach the shell as ONE literal word — sourcing it
// must not execute the substitution. This runs real /bin/sh on a stubbed
// wrapper line.
test('SUPERVISE() quoting: a $(...) path is sourced literally, never executed', (t) => {
  if (process.platform === 'win32') return t.skip('POSIX shell test');
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
  assert.ok(!fs.existsSync(path.join(TMP, 'INJECTION_MARK')), 'the $(printf injected) in the path was NOT executed');
>>>>>>> /tmp/mf-theirs
});
