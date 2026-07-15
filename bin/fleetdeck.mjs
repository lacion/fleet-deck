#!/usr/bin/env node
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
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const execFileP = promisify(execFile);
const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..');

// Same resolution order as the SessionStart hook: the committed bundle is the
// production artifact; source is the dev-checkout fallback.
const BUNDLE = path.join(ROOT, 'scripts', 'fleetd', 'fleetd.bundle.mjs');
const SOURCE = path.join(ROOT, 'scripts', 'fleetd', 'fleetd.mjs');
const FLEETD = fs.existsSync(BUNDLE) ? BUNDLE : SOURCE;

const HOME = process.env.FLEETDECK_HOME || path.join(os.homedir() || '/tmp', '.fleetdeck');
const PORT = Number(process.env.FLEETDECK_PORT || 4711);
const SERVICE_NAME = 'fleetdeck';

const ENV_FILE = path.join(HOME, 'service.env');
const SUPERVISE_SH = path.join(HOME, 'supervise.sh');
const SUPERVISOR_PID = path.join(HOME, 'supervisor.pid');
const LOG_FILE = path.join(HOME, 'fleetd.log');
const UNIT_FILE = path.join(
  process.env.XDG_CONFIG_HOME || path.join(os.homedir() || '/tmp', '.config'),
  'systemd', 'user', `${SERVICE_NAME}.service`,
);

function version() {
  try { return JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8')).version || '0.0.0'; }
  catch { return '0.0.0'; }
}

const out = (s) => process.stdout.write(`${s}\n`);
const err = (s) => process.stderr.write(`${s}\n`);

// ------------------------------------------------------------------- serve

// Run the daemon in the FOREGROUND. This is what a supervisor execs, so it must
// not fork, must not detach, and must let the daemon's own SIGTERM handler run —
// fleetd already has a tested graceful shutdown and we must not shadow it.
async function serve() {
  process.env.FLEETDECK_MANAGED = '1';
  await import(`file://${FLEETD}`);
}

// ------------------------------------------------------------------ health

async function health({ timeout = 1000 } = {}) {
  try {
    const res = await fetch(`http://127.0.0.1:${PORT}/health`, {
      signal: AbortSignal.timeout(timeout),
    });
    if (!res.ok) return null;
    return await res.json();
  } catch { return null; }
}

async function status() {
  const h = await health();
  if (!h) {
    out(`fleetdeck: no daemon answering on 127.0.0.1:${PORT}`);
    return 1;
  }
  out(`fleetdeck v${h.version}${h.managed ? ' (managed service)' : ' (plugin-spawned)'}`);
  out(`  pid      ${h.pid}`);
  out(`  port     ${PORT}`);
  out(`  sessions ${h.fleet}`);
  out(`  spawn    ${h.spawn?.available ? `available (${h.spawn.active} active)` : `unavailable — ${h.spawn?.reason || 'unknown'}`}`);
  const own = version();
  if (h.managed && h.version !== own && own !== '0.0.0') {
    out(`  ⚠ this CLI is v${own}; restart the service to pick it up`);
  }
  return 0;
}

// ------------------------------------------------------------------ doctor

async function onPath(cmd) {
  try { await execFileP('sh', ['-c', `command -v ${cmd}`], { timeout: 5000 }); return true; }
  catch { return false; }
}

// Preflight for a standalone box. Split into HARD failures (the fleet cannot
// work) and WARNINGS (it will come up, but something you care about is missing).
// The Coder template runs `doctor` before `service start`, so a warning must
// never block boot — you need the board up to even read the warning.
async function doctor() {
  const problems = [];
  const warnings = [];

  const [major, minor] = process.versions.node.split('.').map(Number);
  if (major < 22 || (major === 22 && minor < 5)) {
    problems.push(`Node ${process.versions.node} is too old — fleetd needs >= 22.5 for node:sqlite (no polyfill exists)`);
  }

  if (!await onPath('tmux')) {
    problems.push('tmux is not on PATH — every agent runs in a tmux pane, so nothing can spawn without it');
  }
  if (!await onPath('claude')) {
    problems.push('the `claude` CLI is not on PATH — the board would have nothing to launch');
  }

  try {
    fs.mkdirSync(HOME, { recursive: true });
    fs.accessSync(HOME, fs.constants.W_OK);
  } catch (e) {
    problems.push(`FLEETDECK_HOME (${HOME}) is not writable: ${e?.code || e?.message}`);
  }

  // The plugin is what makes a spawned pane REPORT. Without it the board can
  // still launch `claude`, but no hook ever fires, so the card is created and
  // then never moves again — the single most confusing way for this to fail.
  let pluginSeen = false;
  try {
    const { stdout } = await execFileP('claude', ['plugin', 'list'], { timeout: 10_000 });
    pluginSeen = /fleetdeck/i.test(stdout);
  } catch { /* old CLI, or claude missing — already covered above */ }
  if (!pluginSeen) {
    warnings.push(
      'the fleetdeck plugin does not appear to be installed for the `claude` CLI. '
      + 'The board can still spawn agents, but their hooks will never report, so every card '
      + 'will sit at its initial state forever. Install it with:\n'
      + '    claude plugin marketplace add lacion/fleet-deck && claude plugin install fleetdeck@fleetdeck',
    );
  }

  const running = await health({ timeout: 500 });
  if (running) out(`ℹ a daemon is already up on :${PORT} (v${running.version}, pid ${running.pid})`);

  if (process.env.FLEETDECK_PROXY_AUTH === 'trust' && !process.env.FLEETDECK_TRUSTED_ORIGINS) {
    problems.push('FLEETDECK_PROXY_AUTH=trust with no FLEETDECK_TRUSTED_ORIGINS — the daemon will refuse to start');
  }

  for (const w of warnings) err(`⚠ ${w}`);
  for (const p of problems) err(`✗ ${p}`);
  if (!problems.length) out(`✓ fleetdeck v${version()} preflight passed${warnings.length ? ' (with warnings)' : ''}`);
  return problems.length ? 1 : 0;
}

// ----------------------------------------------------------------- service

// systemd is NOT a given. A Coder workspace container makes PID 1 the agent's
// init script — there is no init system at all — so the supervised-wrapper path
// below is the common case, not an exotic fallback.
async function hasSystemd() {
  try { await execFileP('systemctl', ['--user', 'show-environment'], { timeout: 5000 }); return true; }
  catch { return false; }
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
const ENV_VALUE_UNQUOTABLE = /[\u0000-\u001f\u0027\u005c]/;

function writeEnvFile() {
  const lines = [];
  for (const [k, v] of Object.entries(process.env)) {
    if (!k.startsWith('FLEETDECK_')) continue;
    if (k === 'FLEETDECK_MANAGED') continue; // owned by `serve`, never configuration
    if (ENV_VALUE_UNQUOTABLE.test(v)) {
      throw new Error(
        `${k} has a value unsafe for ${ENV_FILE}, which is BOTH shell-sourced (no-systemd `
        + `supervisor) AND parsed by systemd EnvironmentFile. A newline, control character, `
        + `embedded single quote, or backslash cannot be represented identically to both readers `
        + `and is refused. Fix or unset ${k}.`,
      );
    }
    // Bare when safe (unchanged); single-quoted otherwise (spaces / $ ; & | etc.
    // stay literal for both readers). No embedded single quote can reach here.
    lines.push(ENV_VALUE_BARE_SAFE.test(v) ? `${k}=${v}` : `${k}='${v}'`);
  }
  fs.mkdirSync(HOME, { recursive: true });
  // 0600: FLEETDECK_TOKEN may legitimately live here.
  fs.writeFileSync(ENV_FILE, `${lines.join('\n')}\n`, { encoding: 'utf8', mode: 0o600 });
  return lines.length;
}

const UNIT = () => `[Unit]
Description=Fleet Deck — the always-on board for your Claude Code fleet
After=network.target

[Service]
Type=simple
EnvironmentFile=-${ENV_FILE}
ExecStart=${process.execPath} ${path.join(HERE, 'fleetdeck.mjs')} serve
Restart=always
RestartSec=2
# exit 3 is "another daemon already owns the port" — restarting is a hot loop.
RestartPreventExitStatus=3

[Install]
WantedBy=default.target
`;

// The no-systemd supervisor. Deliberately a shell script rather than a Node
// parent: one less long-lived process, and it survives this CLI exiting, which
// a coder_script REQUIRES (a script that does not exit leaves the workspace
// stuck "starting" forever).
const SUPERVISE = () => `#!/bin/sh
# GENERATED by \`fleetdeck service install\` — do not edit; re-run install instead.
set -u
[ -f "${ENV_FILE}" ] && { set -a; . "${ENV_FILE}"; set +a; }

child=''
term() { [ -n "$child" ] && kill -TERM "$child" 2>/dev/null; exit 0; }
trap term TERM INT

delay=1
while :; do
  "${process.execPath}" "${path.join(HERE, 'fleetdeck.mjs')}" serve &
  child=$!
  wait "$child"
  code=$?
  child=''
  # 0 — a deliberate SIGTERM shutdown. Respawning would fight whoever stopped us.
  [ "$code" -eq 0 ] && exit 0
  # 3 — lost the port election; another daemon owns :${PORT}. Respawning is a hot loop.
  [ "$code" -eq 3 ] && exit 3
  sleep "$delay"
  delay=$(( delay < 30 ? delay * 2 : 30 ))
done
`;

async function serviceInstall() {
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
function argvIsOurSupervisor(argv) {
  return Array.isArray(argv) && argv.includes(SUPERVISE_SH);
}

function supervisorLooksLikeOurs(pid) {
  if (process.platform !== 'linux') return true; // no /proc — best-effort fallback
  try {
    const argv = fs.readFileSync(`/proc/${pid}/cmdline`).toString('utf8').split('\0').filter(Boolean);
    return argvIsOurSupervisor(argv);
  } catch (err) {
    // ENOENT is decisive: the pid died between kill(0) and here, so it is not our
    // supervisor. Permission/transient I/O errors are NOT decisive — treating a
    // still-live process as ours avoids falsely dropping a running supervisor.
    return err?.code !== 'ENOENT';
  }
}

function supervisorAlive() {
  try {
    const pid = Number(fs.readFileSync(SUPERVISOR_PID, 'utf8').trim());
    if (!Number.isInteger(pid) || pid <= 0) return 0;
    process.kill(pid, 0);
    if (!supervisorLooksLikeOurs(pid)) return 0; // stale pidfile after PID reuse
    return pid;
  } catch { return 0; }
}

// "Started" must mean "answering", not "spawned". systemctl returns as soon as
// the process exists, which is a good ~100ms before fleetd has opened SQLite and
// bound the port — long enough that a template's next step (or an impatient
// human running `fleetdeck status`) sees a dead board and concludes it failed.
async function waitForHealth({ tries = 20, everyMs = 250 } = {}) {
  for (let i = 0; i < tries; i += 1) {
    await new Promise(r => setTimeout(r, everyMs));
    const h = await health({ timeout: everyMs });
    if (h) return h;
  }
  return null;
}

async function serviceStart() {
  if (await hasSystemd() && fs.existsSync(UNIT_FILE)) {
    await execFileP('systemctl', ['--user', 'start', SERVICE_NAME], { timeout: 15_000 });
    if (!await waitForHealth()) {
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
  if (supervisorAlive()) {
    out('✓ already running');
    return 0;
  }
  // MUST return immediately: a coder_script that does not exit leaves the
  // workspace stuck "starting".
  fs.mkdirSync(HOME, { recursive: true });
  const log = fs.openSync(LOG_FILE, 'a', 0o600);
  try { fs.chmodSync(LOG_FILE, 0o600); } catch { /* pre-existing perms */ }
  const child = spawn('sh', [SUPERVISE_SH], { detached: true, stdio: ['ignore', log, log] });
  child.once('error', () => {});
  child.unref();
  fs.closeSync(log);
  fs.writeFileSync(SUPERVISOR_PID, String(child.pid), { encoding: 'utf8', mode: 0o600 });

  if (!await waitForHealth()) {
    err(`✗ supervisor started (pid ${child.pid}) but no daemon answered on :${PORT} within 5s — see ${LOG_FILE}`);
    return 1;
  }
  out(`✓ fleetdeck up on http://127.0.0.1:${PORT} (supervisor pid ${child.pid})`);
  return 0;
}

async function serviceStop() {
  if (await hasSystemd() && fs.existsSync(UNIT_FILE)) {
    await execFileP('systemctl', ['--user', 'stop', SERVICE_NAME], { timeout: 15_000 });
    out(`✓ stopped ${SERVICE_NAME}.service`);
    return 0;
  }
  // ORDER MATTERS: kill the supervisor FIRST. Signal the daemon while its
  // supervisor is alive and the supervisor dutifully restarts it.
  const sup = supervisorAlive();
  if (sup) {
    try { process.kill(sup, 'SIGTERM'); } catch { /* already gone */ }
  }
  try { fs.unlinkSync(SUPERVISOR_PID); } catch { /* best effort */ }

  const h = await health({ timeout: 500 });
  if (h?.pid) {
    try { process.kill(h.pid, 'SIGTERM'); } catch { /* already gone */ }
    for (let i = 0; i < 12; i += 1) {
      await new Promise(r => setTimeout(r, 250));
      if (!await health({ timeout: 250 })) break;
    }
  }
  out(sup || h ? '✓ stopped' : 'ℹ nothing was running');
  return 0;
}

async function serviceUninstall() {
  await serviceStop();
  if (await hasSystemd() && fs.existsSync(UNIT_FILE)) {
    try { await execFileP('systemctl', ['--user', 'disable', SERVICE_NAME], { timeout: 10_000 }); } catch { /* not enabled */ }
    try { fs.unlinkSync(UNIT_FILE); } catch { /* already gone */ }
    try { await execFileP('systemctl', ['--user', 'daemon-reload'], { timeout: 10_000 }); } catch { /* best effort */ }
  }
  try { fs.unlinkSync(SUPERVISE_SH); } catch { /* already gone */ }
  out('✓ uninstalled (state in FLEETDECK_HOME was left alone)');
  return 0;
}

async function service(sub) {
  switch (sub) {
    case 'install': return serviceInstall();
    case 'uninstall': return serviceUninstall();
    case 'start': return serviceStart();
    case 'stop': return serviceStop();
    case 'restart': await serviceStop(); return serviceStart();
    default:
      err('usage: fleetdeck service <install|uninstall|start|stop|restart>');
      return 2;
  }
}

// ------------------------------------------------------------------- token

async function token(args) {
  const file = path.join(HOME, 'token');
  if (args.includes('--rotate')) {
    const { randomBytes } = await import('node:crypto');
    fs.mkdirSync(HOME, { recursive: true });
    fs.writeFileSync(file, randomBytes(32).toString('hex'), { encoding: 'utf8', mode: 0o600 });
    out('✓ token rotated — restart the daemon for it to take effect');
  }
  try {
    out(fs.readFileSync(file, 'utf8').trim());
    return 0;
  } catch {
    err(`no token at ${file} — one is generated on first start when the board is reachable from off-box`);
    return 1;
  }
}

// -------------------------------------------------------------------- main

const HELP = `fleetdeck v${version()} — the board for your Claude Code fleet

  fleetdeck serve                  run the daemon in the foreground (what a supervisor execs)
  fleetdeck status                 ask the running daemon how it is doing
  fleetdeck doctor                 preflight this machine before running a fleet on it
  fleetdeck service install        install a supervisor (systemd user unit, or a wrapper)
  fleetdeck service start|stop     ...and drive it
  fleetdeck service restart
  fleetdeck service uninstall
  fleetdeck token [--rotate]       print (or replace) the bearer token

Configuration is entirely FLEETDECK_* environment variables — see the README.
For an always-on board on a remote dev box, see docs/CODER.md.
`;

async function main(argv) {
  const [cmd, ...rest] = argv;
  let code = 0;
  switch (cmd) {
    case 'serve': await serve(); return; // never returns until SIGTERM; serve owns the process
    case 'status': code = await status(); break;
    case 'doctor': code = await doctor(); break;
    case 'service': code = await service(rest[0]); break;
    case 'token': code = await token(rest); break;
    case '--version': case '-v': out(version()); break;
    case 'help': case '--help': case '-h': case undefined: out(HELP); break;
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
  try { return !!process.argv[1] && fs.realpathSync(process.argv[1]) === fileURLToPath(import.meta.url); }
  catch { return false; }
})();

if (IS_ENTRYPOINT) await main(process.argv.slice(2));

// exported for tests only — the env-file validation, supervisor identity check,
// and file generators are contracts. Nothing here runs on import (see
// IS_ENTRYPOINT above), so importing is side-effect-free.
export { writeEnvFile, ENV_VALUE_BARE_SAFE, ENV_VALUE_UNQUOTABLE, supervisorAlive, supervisorLooksLikeOurs, argvIsOurSupervisor, serviceInstall, UNIT, SUPERVISE };
