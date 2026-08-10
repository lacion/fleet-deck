// tests/helpers/daemon.mjs
//
// Spawns scripts/fleetd/fleetd.ts on a per-test random port with a fresh
// FLEETDECK_HOME under the OS tmpdir, waits for /health, and tears down
// (kill + rm) when the test is done.
//
// Written against the daemon's contract, not against the daemon implementation —
// scripts/fleetd/fleetd.ts may not exist yet when this file is loaded. Tests
// that spawn it will simply fail/skip until the sibling daemon lands; that is
// expected and not a bug in this harness.

import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, existsSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { scaleMs } from './wait.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = path.resolve(HERE, '../..');
export const FLEETD_PATH = path.join(REPO_ROOT, 'scripts/fleetd/fleetd.ts');

// Testing contract: scratch port range 21600-21999. Deliberately BELOW the
// kernel's ephemeral range (WSL2 default 44620-48715 — check
// /proc/sys/net/ipv4/ip_local_port_range): the old 47xxx range sat inside
// it, so the suite's own outbound health/state polls would occasionally grab
// a 47xxx source port and a later test daemon binding it lost the election
// ("fleetd already running") — the long-blamed "WSL2 flake".
const PORT_MIN = 21600;
const PORT_MAX = 21999;

export function randomPort() {
  return PORT_MIN + Math.floor(Math.random() * (PORT_MAX - PORT_MIN + 1));
}

export function fleetdExists() {
  return existsSync(FLEETD_PATH);
}

function freshHome() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-test-'));
}

/**
 * Poll GET <baseUrl>/health until it responds 2xx or the timeout elapses.
 */
export async function waitForHealth(baseUrl, timeoutMs = 10000) {
  // Scale the health budget by WAIT_SCALE so slow CI lanes get headroom. This
  // covers BOTH the default AND every explicit caller (startDaemon's
  // healthTimeoutMs, takeover's fixed 8000/3000) — the effective deadline is
  // always the authored value * WAIT_SCALE.
  const deadline = Date.now() + scaleMs(timeoutMs);
  let lastErr = null;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(baseUrl + '/health', { signal: AbortSignal.timeout(500) });
      if (res.ok) return await res.json();
    } catch (e) { lastErr = e; }
    await new Promise(r => setTimeout(r, 100));
  }
  throw new Error(`daemon at ${baseUrl} never became healthy: ${lastErr?.message || 'timeout'}`);
}

function waitForExit(child, timeoutMs = 5000) {
  return new Promise((resolve, reject) => {
    if (child.exitCode !== null) return resolve(child.exitCode);
    const t = setTimeout(() => reject(new Error('process did not exit in time')), timeoutMs);
    child.once('exit', code => { clearTimeout(t); resolve(code); });
  });
}

function killProcess(child, timeoutMs = 3000) {
  return new Promise(resolve => {
    if (child.exitCode !== null || child.killed) return resolve();
    const t = setTimeout(() => { try { child.kill('SIGKILL'); } catch { /* noop */ } }, timeoutMs);
    child.once('exit', () => { clearTimeout(t); resolve(); });
    try { child.kill('SIGTERM'); } catch { resolve(); }
  });
}

/**
 * Spawn a fleetd-shaped process (or a stub with the same CLI contract:
 * reads FLEETDECK_PORT / FLEETDECK_HOME, binds 127.0.0.1, exits 3 on
 * EADDRINUSE) without waiting for health. Useful for the election test where
 * the second process is expected to exit quickly rather than come up.
 */
export function spawnRaw({
  port,
  home,
  scriptPath = process.env.FLEETDECK_TEST_DAEMON_SCRIPT || FLEETD_PATH,
  env = {},
} = {}) {
  // Scrub the ambient environment BEFORE spreading it: a dev shell (or this
  // very Claude session) running under `fleetdeck serve` carries
  // FLEETDECK_MANAGED=1 (and often FLEETDECK_PORT/HOME pointing at the real
  // daemon). Inherited by a test daemon, MANAGED makes it report managed:true
  // — takeover.test.mjs's hook then refuses to evict the "service" and every
  // eviction test hangs. Tests that WANT the flag pass it via `env` (spread
  // after), so this delete must not come later than the spread.
  const ambient = { ...process.env };
  delete ambient.FLEETDECK_MANAGED;
  // Same hazard, LAN edition: a dev shell configured for phone/Tailscale access
  // exports FLEETDECK_BIND=0.0.0.0 (+ FLEETDECK_TRUSTED_ORIGINS). Inherited by a
  // test daemon, a non-loopback BIND makes the loopback-trust daemons refuse to
  // start ("FLEETDECK_TRUST_LOOPBACK=on requires a loopback FLEETDECK_BIND") and
  // skews the hook-auth/loopback-gates origin checks — false local failures CI
  // never sees (its shell sets neither). Tests that exercise binding/origins
  // pass their own via `env` (spread after), so they win over this scrub.
  delete ambient.FLEETDECK_BIND;
  delete ambient.FLEETDECK_TRUSTED_ORIGINS;
  const childEnv = {
    ...ambient,
    FLEETDECK_PORT: String(port),
    FLEETDECK_HOME: home,
    // Default the agents-cli poller (handoff F1) OFF for every spawned
    // test daemon: left on, every daemon in the suite would shell out to
    // the real `claude agents --json` ~1s after listening, which is pure
    // background load unrelated to almost all tests and was observed to
    // destabilize unrelated timing-sensitive tests when many daemons spin
    // up concurrently. Tests that actually exercise the poller (see
    // tests/agents-ingest.test.mjs) pass their own FLEETDECK_AGENTS_CMD via
    // `env`, which — spread after this default — wins as usual.
    FLEETDECK_AGENTS_CMD: 'false',
    // Isolated tmux server per test daemon: the suite can never touch (or
    // poison) the developer's real tmux server — the 2026-07-11 env scar.
    FLEETDECK_TMUX_SOCKET: `fleetdeck-test-${port}`,
    ...env,
  };
  // Running the suite from inside tmux must not leak the outer server either.
  delete childEnv.TMUX;
  delete childEnv.TMUX_PANE;
  const child = spawn(process.execPath, [scriptPath], {
    env: childEnv,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', d => { stdout += d; });
  child.stderr.on('data', d => { stderr += d; });
  return {
    proc: child,
    tmuxSocket: childEnv.FLEETDECK_TMUX_SOCKET,
    get stdout() { return stdout; },
    get stderr() { return stderr; },
    waitForExit: (timeoutMs) => waitForExit(child, timeoutMs),
    kill: (timeoutMs) => killProcess(child, timeoutMs),
  };
}

/**
 * Register teardown for caller-owned scratch directories BEFORE attempting a
 * daemon start that may reject (port collision, early crash, health timeout).
 * t.after hooks run even when the test body throws, so cleanup registered at
 * allocation time is what keeps a failed boot from leaking the trees — a hook
 * registered only after a successful startDaemon never runs when the start
 * rejects. Returns a holder: assign `holder.daemon` once startDaemon resolves
 * and its stop folds into the same hook; left null, the stop is skipped.
 */
export function guardScratchDirs(t, dirs, { keepHome = true } = {}) {
  const holder = { daemon: null };
  t.after(async () => {
    await holder.daemon?.stop({ keepHome });
    for (const dir of dirs) {
      rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    }
  });
  return holder;
}

/**
 * Spawn fleetd and wait for it to become healthy. Returns a handle with
 * baseUrl plus a stop() that kills the process and (by default) removes the
 * scratch FLEETDECK_HOME.
 *
 * Options:
 *  - port: fixed port (default: random in the contract's scratch range)
 *  - home: fixed FLEETDECK_HOME (default: fresh tmpdir)
 *  - scriptPath: override the daemon script (used by this repo's own
 *    dry-check against a local stub; production callers should never pass
 *    this)
 *  - env: extra env vars merged in
 *  - healthTimeoutMs: how long to wait for /health before giving up
 */
export async function startDaemon({
  port = randomPort(),
  home,
  // FLEETDECK_TEST_DAEMON_SCRIPT lets this repo's own dry-check point the
  // whole suite at a local reference stub while scripts/fleetd/fleetd.ts is
  // still being built, without editing any test file or touching scripts/.
  // Unset in normal use, so production runs always spawn the real daemon.
  scriptPath = process.env.FLEETDECK_TEST_DAEMON_SCRIPT || FLEETD_PATH,
  env = {},
  healthTimeoutMs = 10000,
} = {}) {
  // Track ownership explicitly: only the home THIS call allocated may be
  // removed by the helper (on startup failure or in stop()). A caller-owned
  // home (e.g. election.test's homeA) belongs to the caller's own teardown
  // (e.g. the election suite's t.after) and is never ours to delete, failed
  // startup or not.
  const ownsHome = home === undefined;
  if (ownsHome) home = freshHome();
  const raw = spawnRaw({ port, home, scriptPath, env });
  const baseUrl = `http://127.0.0.1:${port}`;
  // Any 2xx /health on the port is NOT proof our child came up: two test
  // processes can draw the same scratch port, and the election loser exits 3
  // while the winner keeps answering. Require the health body to name our
  // child's PID, and race the poll against the child exiting, so a dead
  // child is never handed back as a live handle onto another run's daemon.
  let healthSettled = false;
  const childExited = new Promise((_, reject) => {
    if (raw.proc.exitCode !== null) {
      reject(new Error(`daemon (pid ${raw.proc.pid}) exited with code ${raw.proc.exitCode} before becoming healthy`));
      return;
    }
    raw.proc.once('exit', code => {
      if (!healthSettled) reject(new Error(`daemon (pid ${raw.proc.pid}) exited with code ${code} before becoming healthy`));
    });
  });
  try {
    const health = await Promise.race([waitForHealth(baseUrl, healthTimeoutMs), childExited]);
    if (health?.pid !== raw.proc.pid) {
      throw new Error(`/health on ${baseUrl} answered for pid ${health?.pid ?? '(none)'}, not our child's pid ${raw.proc.pid} — the port belongs to another daemon`);
    }
  } catch (err) {
    healthSettled = true;
    await raw.kill();
    // Startup failed, so no handle — and with it stop() — ever reaches the
    // caller, and nothing else will ever clean up the scratch home. Remove it
    // here or it leaks one fleetdeck-test-* directory (database, token, log,
    // pid state) per failed start. Caller-owned homes survive untouched for
    // the caller's own teardown and post-mortem inspection.
    if (ownsHome) {
      rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    }
    const detail = raw.stderr || raw.stdout || '(no output captured)';
    throw new Error(`${err.message}\n--- daemon output ---\n${detail}`);
  }
  healthSettled = true;
  // 0.16.0: the daemon always mints/persists a token, and /hook/*, POST /mail,
  // /ws/term and gateway_* writes now require it. Surface it on the handle so
  // tests can act as the authenticated caller (postHook, postJson {token}).
  let token = null;
  try { token = readFileSync(path.join(home, 'token'), 'utf8').trim() || null; } catch { /* persist failure — gated routes will 401, as the daemon warned */ }
  return {
    port,
    home,
    baseUrl,
    token,
    proc: raw.proc,
    get stdout() { return raw.stdout; },
    get stderr() { return raw.stderr; },
    async stop({ keepHome = false } = {}) {
      await raw.kill();
      // tmux servers outlive the daemon that started them: any test whose
      // daemon touched the real tmux path left a live
      // `tmux -L fleetdeck-test-<port>` server behind forever — observed
      // 2026-07-14: 89 leaked servers from the previous day's suite runs,
      // four still hosting live claude panes that haunted the production
      // board as ghost cards. Guarded to test-owned sockets so a test that
      // points FLEETDECK_TMUX_SOCKET at a shared server can never have that
      // server killed from here.
      if (raw.tmuxSocket?.startsWith('fleetdeck-test-')) {
        try {
          spawnSync('tmux', ['-L', raw.tmuxSocket, 'kill-server'], { stdio: 'ignore', timeout: 3000 });
        } catch { /* best-effort: no server on the socket is the common case */ }
      }
      if (!keepHome) {
        rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
      }
    },
  };
}
