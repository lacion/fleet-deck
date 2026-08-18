// tests/helpers/daemon.ts
//
// Spawns src/daemon/fleetd.ts on a per-test random port with a fresh
// FLEETDECK_HOME under the OS tmpdir, waits for /health, and tears down
// (kill + rm) when the test is done.
//
// Written against the daemon's contract, not against the daemon implementation —
// src/daemon/fleetd.ts may not exist yet when this file is loaded. Tests
// that spawn it will simply fail/skip until the sibling daemon lands; that is
// expected and not a bug in this harness.

import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtempSync, rmSync, existsSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import type { TestContext } from 'node:test';
import { fileURLToPath } from 'node:url';

import { releaseHookBoardClient } from './http.ts';
import { cleanupOwnedTmuxSocket } from './tmux.ts';
import { scaleMs } from './wait.ts';

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = path.resolve(HERE, '../..');
export const FLEETD_PATH = path.join(REPO_ROOT, 'src/daemon/fleetd.ts');

// Testing contract: scratch port range 21600-21999. Deliberately BELOW the
// kernel's ephemeral range (WSL2 default 44620-48715 — check
// /proc/sys/net/ipv4/ip_local_port_range): the old 47xxx range sat inside
// it, so the suite's own outbound health/state polls would occasionally grab
// a 47xxx source port and a later test daemon binding it lost the election
// ("fleetd already running") — the long-blamed "WSL2 flake".
const PORT_MIN = 21600;
const PORT_MAX = 21999;

export function randomPort(): number {
  return PORT_MIN + Math.floor(Math.random() * (PORT_MAX - PORT_MIN + 1));
}

export function fleetdExists(): boolean {
  return existsSync(FLEETD_PATH);
}

function freshHome(): string {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-test-'));
}

/** Shape of the daemon's /health JSON we depend on (it carries more). */
interface HealthResponse {
  pid?: number;
  [key: string]: unknown;
}

/**
 * Poll GET <baseUrl>/health until it responds 2xx or the timeout elapses.
 */
export async function waitForHealth(baseUrl: string, timeoutMs = 10000): Promise<HealthResponse> {
  // Scale the health budget by WAIT_SCALE so slow CI lanes get headroom. This
  // covers BOTH the default AND every explicit caller (startDaemon's
  // healthTimeoutMs, takeover's fixed 8000/3000) — the effective deadline is
  // always the authored value * WAIT_SCALE.
  const deadline = Date.now() + scaleMs(timeoutMs);
  let lastErr: unknown = null;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(baseUrl + '/health', { signal: AbortSignal.timeout(500) });
      if (res.ok) return (await res.json()) as HealthResponse;
    } catch (e) {
      lastErr = e;
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(
    `daemon at ${baseUrl} never became healthy: ${lastErr instanceof Error ? lastErr.message : 'timeout'}`,
  );
}

function waitForExit(child: ChildProcess, timeoutMs = 5000): Promise<number | null> {
  return new Promise((resolve, reject) => {
    if (child.exitCode !== null) {
      resolve(child.exitCode);
      return;
    }
    const t = setTimeout(() => {
      reject(new Error('process did not exit in time'));
    }, timeoutMs);
    child.once('exit', (code) => {
      clearTimeout(t);
      resolve(code);
    });
  });
}

function killProcess(child: ChildProcess, timeoutMs = 3000): Promise<void> {
  return new Promise((resolve) => {
    if (child.exitCode !== null || child.killed) {
      resolve();
      return;
    }
    const t = setTimeout(() => {
      try {
        child.kill('SIGKILL');
      } catch {
        /* noop */
      }
    }, timeoutMs);
    child.once('exit', () => {
      clearTimeout(t);
      resolve();
    });
    try {
      child.kill('SIGTERM');
    } catch {
      resolve();
    }
  });
}

export interface SpawnRawOptions {
  port: number;
  home: string;
  scriptPath?: string;
  env?: Record<string, string>;
}

export interface RawDaemon {
  proc: ChildProcess;
  tmuxSocket: string | undefined;
  tmuxTmpDir: string | undefined;
  readonly stdout: string;
  readonly stderr: string;
  waitForExit: (timeoutMs?: number) => Promise<number | null>;
  kill: (timeoutMs?: number) => Promise<void>;
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
  scriptPath = process.env['FLEETDECK_TEST_DAEMON_SCRIPT'] ?? FLEETD_PATH,
  env = {},
}: SpawnRawOptions): RawDaemon {
  // Scrub the ambient environment BEFORE spreading it: a dev shell (or this
  // very Claude session) running under `fleetdeck serve` carries
  // FLEETDECK_MANAGED=1 (and often FLEETDECK_PORT/HOME pointing at the real
  // daemon). Inherited by a test daemon, MANAGED makes it report managed:true
  // — takeover.test.mjs's hook then refuses to evict the "service" and every
  // eviction test hangs. Tests that WANT the flag pass it via `env` (spread
  // after), so this delete must not come later than the spread.
  const ambient = { ...process.env };
  delete ambient['FLEETDECK_MANAGED'];
  // Same hazard, LAN edition: a dev shell configured for phone/Tailscale access
  // exports FLEETDECK_BIND=0.0.0.0 (+ FLEETDECK_TRUSTED_ORIGINS). Inherited by a
  // test daemon, a non-loopback BIND makes the loopback-trust daemons refuse to
  // start ("FLEETDECK_TRUST_LOOPBACK=on requires a loopback FLEETDECK_BIND") and
  // skews the hook-auth/loopback-gates origin checks — false local failures CI
  // never sees (its shell sets neither). Tests that exercise binding/origins
  // pass their own via `env` (spread after), so they win over this scrub.
  delete ambient['FLEETDECK_BIND'];
  delete ambient['FLEETDECK_TRUSTED_ORIGINS'];
  const childEnv: NodeJS.ProcessEnv = {
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
    // Most existing hook-relay tests exercise the explicit legacy/all scope.
    // Production defaults to `spawned`; dedicated hook-scope tests override
    // this and prove ordinary terminal sessions fail open immediately.
    FLEETDECK_HOLD_SCOPE: 'all',
    // Isolated tmux server per test daemon: the suite can never touch (or
    // poison) the developer's real tmux server — the 2026-07-11 env scar.
    FLEETDECK_TMUX_SOCKET: `fleetdeck-test-${port}`,
    ...env,
  };
  // Running the suite from inside tmux must not leak the outer server either.
  delete childEnv['TMUX'];
  delete childEnv['TMUX_PANE'];
  const child = spawn(process.execPath, [scriptPath], {
    env: childEnv,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (d: Buffer) => {
    stdout += d.toString();
  });
  child.stderr.on('data', (d: Buffer) => {
    stderr += d.toString();
  });
  return {
    proc: child,
    tmuxSocket: childEnv['FLEETDECK_TMUX_SOCKET'],
    tmuxTmpDir: childEnv['TMUX_TMPDIR'],
    get stdout() {
      return stdout;
    },
    get stderr() {
      return stderr;
    },
    waitForExit: (timeoutMs?: number) => waitForExit(child, timeoutMs),
    kill: async (timeoutMs?: number) => {
      await killProcess(child, timeoutMs);
      // tmux can leave a dead Unix socket inode after its server exits. Reap
      // the exact test-owned label on every raw lifecycle, including failed
      // starts that never return a DaemonHandle to the caller.
      cleanupOwnedTmuxSocket(childEnv['FLEETDECK_TMUX_SOCKET'], childEnv['TMUX_TMPDIR']);
    },
  };
}

export interface DaemonHandle {
  port: number;
  home: string;
  baseUrl: string;
  token: string | null;
  proc: ChildProcess;
  readonly stdout: string;
  readonly stderr: string;
  stop: (opts?: { keepHome?: boolean }) => Promise<void>;
}

export interface ScratchHolder {
  daemon: DaemonHandle | null;
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
export function guardScratchDirs(
  t: TestContext,
  dirs: string[],
  { keepHome = true }: { keepHome?: boolean } = {},
): ScratchHolder {
  const holder: ScratchHolder = { daemon: null };
  t.after(async () => {
    await holder.daemon?.stop({ keepHome });
    for (const dir of dirs) {
      rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    }
  });
  return holder;
}

export interface StartDaemonOptions {
  port?: number;
  home?: string;
  scriptPath?: string;
  env?: Record<string, string>;
  healthTimeoutMs?: number;
}

/**
 * Thrown when the scratch port we drew is already held by another daemon:
 * either a live foreign responder answers /health with a different PID, or our
 * own child loses the bind and exits 3 (EADDRINUSE). When the port was
 * AUTO-drawn, this is a disposable-environment hazard startDaemon recovers from
 * by redrawing; a caller-pinned port surfaces it as a hard failure.
 */
class PortCollisionError extends Error {}

/**
 * Classify an early daemon exit. fleetd exits 3 specifically on EADDRINUSE
 * (src/daemon/fleetd.ts: "port bind lost the election") — the drawn port
 * was already held, a retryable collision. Any other code is a genuine crash.
 */
function exitError(raw: RawDaemon, code: number | null): Error {
  const pid = raw.proc.pid ?? '(none)';
  if (code === 3) {
    return new PortCollisionError(
      `daemon (pid ${pid}) exited with code 3 (port bind lost the election) before becoming healthy`,
    );
  }
  return new Error(
    `daemon (pid ${pid}) exited with code ${code ?? '(none)'} before becoming healthy`,
  );
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
 *
 * Port-collision recovery: when the port is AUTO-drawn (no explicit `port`), a
 * leaked or foreign daemon squatting the port we happened to draw — the
 * recurring "port belongs to another daemon" collision on the shared
 * 21600-21999 scratch range, where one leaked daemon gives every later file a
 * ~1/400-per-draw chance of colliding — is redrawn and retried rather than
 * failing the test on a pure environment hazard. A caller-PINNED port is tried
 * exactly once: restart-in-place and election-race tests depend on the port
 * they chose, so silently moving would defeat the invariant they assert.
 */
export async function startDaemon({
  port,
  home,
  // FLEETDECK_TEST_DAEMON_SCRIPT lets this repo's own dry-check point the
  // whole suite at a local reference stub while src/daemon/fleetd.ts is
  // still being built, without editing any test file or touching scripts/.
  // Unset in normal use, so production runs always spawn the real daemon.
  scriptPath = process.env['FLEETDECK_TEST_DAEMON_SCRIPT'] ?? FLEETD_PATH,
  env = {},
  healthTimeoutMs = 10000,
}: StartDaemonOptions = {}): Promise<DaemonHandle> {
  // Track ownership explicitly: only the home THIS call allocated may be
  // removed by the helper (on startup failure or in stop()). A caller-owned
  // home (e.g. election.test's homeA) belongs to the caller's own teardown
  // (e.g. the election suite's t.after) and is never ours to delete, failed
  // startup or not.
  const ownsHome = home === undefined;
  const homeDir = home ?? freshHome();
  const autoPort = port === undefined;
  const maxAttempts = autoPort ? 8 : 1;
  let lastCollision: unknown = null;

  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    const thisPort = port ?? randomPort();
    const raw = spawnRaw({ port: thisPort, home: homeDir, scriptPath, env });
    const baseUrl = `http://127.0.0.1:${thisPort}`;
    // Any 2xx /health on the port is NOT proof our child came up: two test
    // processes can draw the same scratch port, and the election loser exits 3
    // while the winner keeps answering. Require the health body to name our
    // child's PID, and race the poll against the child exiting, so a dead
    // child is never handed back as a live handle onto another run's daemon.
    let healthSettled = false;
    const childExited = new Promise<never>((_resolve, reject) => {
      if (raw.proc.exitCode !== null) {
        reject(exitError(raw, raw.proc.exitCode));
        return;
      }
      raw.proc.once('exit', (code) => {
        if (!healthSettled) reject(exitError(raw, code));
      });
    });
    try {
      const health = await Promise.race([waitForHealth(baseUrl, healthTimeoutMs), childExited]);
      if (health.pid !== raw.proc.pid) {
        throw new PortCollisionError(
          `/health on ${baseUrl} answered for pid ${health.pid ?? '(none)'}, not our child's pid ${raw.proc.pid ?? '(none)'} — the port belongs to another daemon`,
        );
      }
    } catch (err) {
      healthSettled = true;
      await raw.kill();
      // A collided AUTO-drawn port is not a failure — redraw and try again
      // (leaving the foreign daemon untouched: it is not ours to reap). Only
      // when retries are exhausted, the error is a real crash, or the port was
      // caller-pinned does the start fail hard.
      if (autoPort && err instanceof PortCollisionError && attempt < maxAttempts - 1) {
        lastCollision = err;
        continue;
      }
      // Startup failed, so no handle — and with it stop() — ever reaches the
      // caller, and nothing else will ever clean up the scratch home. Remove it
      // here or it leaks one fleetdeck-test-* directory (database, token, log,
      // pid state) per failed start. Caller-owned homes survive untouched for
      // the caller's own teardown and post-mortem inspection.
      if (ownsHome) {
        rmSync(homeDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
      }
      const detail = raw.stderr || raw.stdout || '(no output captured)';
      const message = err instanceof Error ? err.message : String(err);
      throw new Error(`${message}\n--- daemon output ---\n${detail}`, { cause: err });
    }
    healthSettled = true;
    // 0.16.0: the daemon always mints/persists a token, and /hook/*, POST /mail,
    // /ws/term and gateway_* writes now require it. Surface it on the handle so
    // tests can act as the authenticated caller (postHook, postJson {token}).
    let token: string | null = null;
    try {
      token = readFileSync(path.join(homeDir, 'token'), 'utf8').trim() || null;
    } catch {
      /* persist failure — gated routes will 401, as the daemon warned */
    }
    return {
      port: thisPort,
      home: homeDir,
      baseUrl,
      token,
      proc: raw.proc,
      get stdout() {
        return raw.stdout;
      },
      get stderr() {
        return raw.stderr;
      },
      async stop({ keepHome = false }: { keepHome?: boolean } = {}) {
        await releaseHookBoardClient(baseUrl);
        await raw.kill();
        if (!keepHome) {
          rmSync(homeDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
        }
      },
    };
  }

  // Unreachable in practice: the loop returns a handle or throws inside. Kept
  // so every path is typed as returning DaemonHandle, and as a clear last word
  // if every auto-port attempt collided.
  if (ownsHome) {
    rmSync(homeDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
  throw new Error(
    `daemon never started: all ${maxAttempts} auto-port attempts hit a squatted scratch port — ${lastCollision instanceof Error ? lastCollision.message : String(lastCollision)}`,
  );
}
