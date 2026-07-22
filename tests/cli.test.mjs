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
  writeEnvFile, ENV_VALUE_BARE_SAFE, ENV_VALUE_UNQUOTABLE, supervisorAlive, supervisorLooksLikeOurs, argvIsOurSupervisor,
  serviceInstall, UNIT, SUPERVISE,
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

test('SUPERVISE(): sources the env file safely and backs off, never respawning a clean exit', () => {
  const s = SUPERVISE();
  assert.ok(s.includes(`. "${ENV_FILE}"`), 'dot-sources the env file inside set -a/set +a');
  assert.match(s, /set -a; \. "/, 'exports while sourcing so children inherit config');
  assert.ok(s.includes('-eq 0 ] && exit 0'), 'a clean SIGTERM shutdown is not respawned');
  assert.ok(s.includes('-eq 3 ] && exit 3'), 'a lost-port exit is not hot-looped');
  assert.ok(s.includes('serve'), 'execs `fleetdeck serve`');
});
