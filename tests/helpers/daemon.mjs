// tests/helpers/daemon.mjs
//
// Spawns scripts/fleetd/fleetd.mjs on a per-test random port with a fresh
// FLEETDECK_HOME under the OS tmpdir, waits for /health, and tears down
// (kill + rm) when the test is done.
//
// Written against the daemon's contract, not against the daemon implementation —
// scripts/fleetd/fleetd.mjs may not exist yet when this file is loaded. Tests
// that spawn it will simply fail/skip until the sibling daemon lands; that is
// expected and not a bug in this harness.

import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = path.resolve(HERE, '../..');
export const FLEETD_PATH = path.join(REPO_ROOT, 'scripts/fleetd/fleetd.mjs');

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
  const deadline = Date.now() + timeoutMs;
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
  const childEnv = {
    ...process.env,
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
    get stdout() { return stdout; },
    get stderr() { return stderr; },
    waitForExit: (timeoutMs) => waitForExit(child, timeoutMs),
    kill: (timeoutMs) => killProcess(child, timeoutMs),
  };
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
  home = freshHome(),
  // FLEETDECK_TEST_DAEMON_SCRIPT lets this repo's own dry-check point the
  // whole suite at a local reference stub while scripts/fleetd/fleetd.mjs is
  // still being built, without editing any test file or touching scripts/.
  // Unset in normal use, so production runs always spawn the real daemon.
  scriptPath = process.env.FLEETDECK_TEST_DAEMON_SCRIPT || FLEETD_PATH,
  env = {},
  healthTimeoutMs = 10000,
} = {}) {
  const raw = spawnRaw({ port, home, scriptPath, env });
  const baseUrl = `http://127.0.0.1:${port}`;
  try {
    await waitForHealth(baseUrl, healthTimeoutMs);
  } catch (err) {
    await raw.kill();
    const detail = raw.stderr || raw.stdout || '(no output captured)';
    throw new Error(`${err.message}\n--- daemon output ---\n${detail}`);
  }
  return {
    port,
    home,
    baseUrl,
    proc: raw.proc,
    get stdout() { return raw.stdout; },
    get stderr() { return raw.stderr; },
    async stop({ keepHome = false } = {}) {
      await raw.kill();
      if (!keepHome) {
        rmSync(home, { recursive: true, force: true });
      }
    },
  };
}
