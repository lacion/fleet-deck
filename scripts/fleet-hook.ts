#!/usr/bin/env bun
// fleet-hook.ts — authenticated command-shim for every hook event except
// SessionStart (fleet-sessionstart.ts) and the Stop asyncRewake leg
// (fleet-watch.ts).
//
// Why this exists: Claude Code http hooks cannot attach an Authorization
// header, so the daemon used to exempt /hook/* from the bearer entirely — and
// ANY local process could impersonate a session (forge /clear succession,
// plant permission holds, drain mailboxes) with one curl. This shim is a
// command hook, so it can do what an http hook cannot: read the daemon's token
// file and present it. hooks.json routes every former http hook through here.
//
// Design rule (same as fleet-sessionstart.ts): NEVER break the session. Every
// failure path is a silent exit 0 with '{}' on stdout, and the per-event
// watchdog guarantees we are gone before hooks.json's own timeout would fire.

import { resolveHome, resolvePort, resolveBase, readToken } from '../src/daemon/config.ts';
import { runNonce } from '../src/daemon/run-nonce.ts';
import { hasActiveClaudeCompatibility } from './claude-compat.ts';
import { canonicalHookOutput } from './hook-output.ts';

async function canonicalNoopAndExit(): Promise<never> {
  const hardExit = setTimeout(() => process.exit(0), 100);
  try {
    await new Promise<void>((resolve) => {
      process.stdout.write('{}', () => resolve());
    });
  } catch {
    /* a closed stdout is still a successful optional hook */
  } finally {
    clearTimeout(hardExit);
  }
  process.exit(0);
}

// argv[2] is the hook event name hooks.json passes; `string | undefined` only
// under a broken invocation. Kept honest so the membership tests below guard it
// explicitly, and the URL coerces it exactly as the .mjs did (undefined →
// "undefined", a path the daemon 404s harmlessly).
const EVENT: string | undefined = process.argv[2];
let runtime: { home: string; base: string } | null = null;
try {
  const home = resolveHome();
  // Compatibility is established exactly once by SessionStart. Missing,
  // corrupt, stale-policy, or unsupported verdicts make every later hook a
  // tiny local no-op before token/run identity/timers/network are touched.
  if (!hasActiveClaudeCompatibility(home)) await canonicalNoopAndExit();
  const port = resolvePort();
  runtime = { home, base: resolveBase(port) };
} catch {
  // Invalid configuration belongs in `fleetdeck doctor`, never in the user's
  // Claude turn. This happens before the normal watchdog exists, so flush the
  // canonical no-op explicitly and leave with success.
  await canonicalNoopAndExit();
}
if (!runtime) await canonicalNoopAndExit();
// canonicalNoopAndExit never returns; the assertion only teaches TypeScript
// what the async `never` path above already guarantees at runtime.
const readyRuntime = runtime as { home: string; base: string };
const HOME = readyRuntime.home;
const BASE = readyRuntime.base;

// Run generation (BUG-025): ONE nonce per CLI PROCESS, shared by every hook
// that process fires, so the daemon can tell a delayed SessionEnd from a dead
// run apart from the live one. Keyed on the CLI itself (the direct `/bin/sh`
// launcher derives and exports CLAUDE_PID from its stable parent) — NOT on this
// Bun shim's parent, which is a fresh launcher per hook and therefore minted a
// new nonce every single time, making every tagged SessionEnd look stale.
// Mint-and-attach only when the payload carries none; any failure leaves the
// event untagged, which the daemon treats as the historical
// unconditional-tombstone path. See run-nonce.ts for the measured failure.
const RUN = runNonce(HOME);

function withRun(raw: string): string {
  if (!RUN) return raw;
  try {
    const parsed: unknown = raw ? JSON.parse(raw) : {};
    if (parsed && typeof parsed === 'object') {
      const body = parsed as { fleet_run?: unknown };
      if (body.fleet_run == null) {
        body.fleet_run = RUN;
        return JSON.stringify(body);
      }
    }
  } catch {
    /* unparseable body — forward verbatim */
  }
  return raw;
}

// Hook events whose daemon response may park until the board answers (the
// hold-open relay in http.ts). A Fleet Deck-launched Claude process carries its
// exact pre-created session id in FLEETDECK_BOARD_SESSION. The shim compares it
// with the hook payload before extending the watchdog. A mismatched marker, the
// legacy boolean marker "1", or a nested/manual Claude process therefore keeps
// the short fail-open ceiling: even a wedged daemon cannot hold its native
// prompt for minutes. The daemon independently checks its active spawn ledger
// before it creates a hold, so this marker is defense in depth, not the
// authorization boundary.
//
// For a marked board pane, THE LOCKSTEP INVARIANT: the daemon's hold
// window (questions.ts resolveHoldMs, default 600 s, clamp ceiling 650 s)
// must stay UNDER this watchdog, and this watchdog must stay UNDER the
// hooks.json `timeout` for these three events (720 s) — otherwise a board
// answer lands on a dead socket and the hook fails open. All three places
// carry this comment; change them together. The shim does not know the
// daemon's configured window, so the watchdog tracks the hooks.json timeout
// (its own hard ceiling) with margin.
//
// The long ceiling is NOT permission for a wedged daemon to stall Claude for
// eleven minutes. While a marked hold POST is pending, a separate short /health
// lease must keep renewing. Three consecutive misses abort and fail open; a
// healthy daemon therefore preserves the full human answer window, while a
// dead/frozen one is bounded to roughly 12–17 seconds.
const HOLD_EVENTS = new Set(['PermissionRequest', 'Elicitation', 'AskUserQuestion']);
const BOARD_SESSION_MARKER = process.env['FLEETDECK_BOARD_SESSION'];
const HEALTH_PROBE_INTERVAL_MS = (() => {
  const n = Number(process.env['FLEETDECK_TEST_HOOK_HEALTH_INTERVAL_MS']);
  return Number.isFinite(n) && n >= 25 ? Math.min(n, 4_000) : 4_000;
})();
const HEALTH_PROBE_TIMEOUT_MS = (() => {
  const n = Number(process.env['FLEETDECK_TEST_HOOK_HEALTH_TIMEOUT_MS']);
  return Number.isFinite(n) && n >= 25 ? Math.min(n, 1_500) : 1_500;
})();
const HEALTH_PROBE_FAILURES = 3;
let watchdogMs = EVENT === 'SessionEnd' ? 800 : 2_500;
let outputStarted = false;
const failOpen = (): void => {
  // `process.exit()` immediately after write() can drop the only `{}` the CLI
  // is waiting for when stdout is a pipe. Give the tiny no-op one event-loop
  // turn to flush, with a hard fallback so a closed/backpressured consumer can
  // never extend the watchdog into a session stall.
  // If the normal footer already owns stdout, it has installed the same hard
  // flush bound below; exiting here would race and truncate that valid answer.
  if (outputStarted) return;
  outputStarted = true;
  const hardExit = setTimeout(() => process.exit(0), 100);
  try {
    process.stdout.write('{}', () => {
      clearTimeout(hardExit);
      process.exit(0);
    });
  } catch {
    clearTimeout(hardExit);
    process.exit(0);
  }
};
let watchdog = setTimeout(failOpen, watchdogMs);

function rearmWatchdog(ms: number): void {
  clearTimeout(watchdog);
  watchdogMs = ms;
  watchdog = setTimeout(failOpen, watchdogMs);
}

function payloadSessionId(raw: string): string | null {
  try {
    const parsed: unknown = JSON.parse(raw || '{}');
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
    const sid = (parsed as { session_id?: unknown }).session_id;
    return typeof sid === 'string' && sid ? sid : null;
  } catch {
    return null;
  }
}

function startDaemonLivenessLease(
  requestController: AbortController,
  authorization: string | null,
): () => void {
  let stopped = false;
  let failures = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let activeController: AbortController | null = null;

  const probe = async (): Promise<void> => {
    if (stopped) return;
    const ctl = new AbortController();
    activeController = ctl;
    const timeout = setTimeout(() => ctl.abort(), HEALTH_PROBE_TIMEOUT_MS);
    let healthy = false;
    try {
      const res = await fetch(`${BASE}/health`, {
        ...(authorization ? { headers: { authorization } } : {}),
        signal: ctl.signal,
      });
      if (res.ok) {
        const body: unknown = await res.json();
        healthy =
          body !== null &&
          typeof body === 'object' &&
          !Array.isArray(body) &&
          (body as { ok?: unknown }).ok === true;
      }
    } catch {
      healthy = false;
    } finally {
      clearTimeout(timeout);
      if (activeController === ctl) activeController = null;
    }
    if (stopped) return;
    failures = healthy ? 0 : failures + 1;
    if (failures >= HEALTH_PROBE_FAILURES) {
      // Abort the parked POST so the normal single-output path writes `{}`.
      // Tighten the watchdog as a second bound in case the runtime ever fails
      // to settle an aborted fetch; do not call failOpen() concurrently here,
      // or the catch path and watchdog could both write to hook stdout.
      requestController.abort();
      rearmWatchdog(500);
      return;
    }
    timer = setTimeout(() => {
      void probe();
    }, HEALTH_PROBE_INTERVAL_MS);
  };

  timer = setTimeout(() => {
    void probe();
  }, HEALTH_PROBE_INTERVAL_MS);
  return () => {
    stopped = true;
    if (timer) clearTimeout(timer);
    // A board answer that lands during a probe should return immediately, not
    // wait up to HEALTH_PROBE_TIMEOUT_MS for an irrelevant request to settle.
    activeController?.abort();
    activeController = null;
  };
}

// The token the shim exists to present. Absent only in a broken/odd install —
// the daemon has minted one at every boot since 0.16.0 — in which case we send
// no header and the daemon's 401 (fail-open for hooks) applies.
const TOKEN: string | null = readToken(HOME);

async function readStdinRaw(): Promise<string> {
  const maxBytes = 1024 * 1024;
  let data = '';
  let bytes = 0;
  try {
    for await (const chunk of process.stdin) {
      // The daemon refuses bodies past 1MB anyway — stop accumulating rather
      // than letting a wedged writer pin the shim's memory until the watchdog.
      if (bytes >= maxBytes) continue;
      const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk));
      const kept = buf.subarray(0, maxBytes - bytes);
      data += kept.toString('utf8');
      bytes += kept.length;
    }
  } catch {
    /* empty body is fine */
  }
  return data;
}

let output = '{}';
try {
  const raw = await readStdinRaw();
  let longBoardHold = false;
  if (
    EVENT !== undefined &&
    HOLD_EVENTS.has(EVENT) &&
    BOARD_SESSION_MARKER !== undefined &&
    BOARD_SESSION_MARKER !== '1' &&
    payloadSessionId(raw) === BOARD_SESSION_MARKER
  ) {
    longBoardHold = true;
    rearmWatchdog(660_000);
  }
  const headers: { 'content-type': string; authorization?: string } = {
    'content-type': 'application/json',
  };
  if (TOKEN) headers.authorization = `Bearer ${TOKEN}`;
  const ctl = new AbortController();
  const stopLivenessLease = longBoardHold
    ? startDaemonLivenessLease(ctl, TOKEN ? `Bearer ${TOKEN}` : null)
    : () => {
        /* short hooks are already bounded by their ordinary watchdog */
      };
  const t = setTimeout(() => {
    ctl.abort();
  }, watchdogMs - 400);
  try {
    const res = await fetch(`${BASE}/hook/${String(EVENT)}`, {
      method: 'POST',
      headers,

      body: withRun(raw) || '{}',
      signal: ctl.signal,
    });
    const text = await res.text();
    // Hook stdout is how the CLI receives additionalContext / hold decisions.
    // A non-2xx or malformed body becomes the fail-open no-op.
    output = res.ok ? canonicalHookOutput(EVENT, text, raw) : '{}';
  } finally {
    stopLivenessLease();
    clearTimeout(t);
  }
} catch {
  output = '{}';
}

// Do not force-exit immediately after write(): Node and Bun may otherwise
// truncate a structured answer when stdout is a pipe. The watchdog remains
// armed while the tiny JSON body flushes, so a broken consumer still cannot
// wedge the hook.
// A watchdog may have started its own canonical `{}` write in the tiny race
// between fetch settlement and this footer. In that case it owns stdout and
// its flush callback/hard-exit timer; never append a second JSON object.
if (!outputStarted) {
  outputStarted = true;
  const hardExit = setTimeout(() => process.exit(0), 100);
  try {
    await new Promise<void>((resolve) => {
      process.stdout.write(output, () => resolve());
    });
  } catch {
    /* closed stdout is still a successful fail-open hook */
  } finally {
    clearTimeout(hardExit);
    clearTimeout(watchdog);
  }
}
process.exitCode = 0;
