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

// argv[2] is the hook event name hooks.json passes; `string | undefined` only
// under a broken invocation. Kept honest so the membership tests below guard it
// explicitly, and the URL coerces it exactly as the .mjs did (undefined →
// "undefined", a path the daemon 404s harmlessly).
const EVENT: string | undefined = process.argv[2];
const HOME = resolveHome();
const BASE = resolveBase(resolvePort());

// Run generation (BUG-025): ONE nonce per CLI PROCESS, shared by every hook
// that process fires, so the daemon can tell a delayed SessionEnd from a dead
// run apart from the live one. Keyed on the CLI itself (CLAUDE_PID) — NOT on
// this shim's parent, which is a fresh shell per hook and therefore minted a
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

// Hook events whose daemon response parks until the board answers (the
// hold-open relay in http.ts). THE LOCKSTEP INVARIANT: the daemon's hold
// window (questions.ts resolveHoldMs, default 600 s, clamp ceiling 650 s)
// must stay UNDER this watchdog, and this watchdog must stay UNDER the
// hooks.json `timeout` for these three events (720 s) — otherwise a board
// answer lands on a dead socket and the hook fails open. All three places
// carry this comment; change them together. The shim does not know the
// daemon's configured window, so the watchdog tracks the hooks.json timeout
// (its own hard ceiling) with margin.
const HOLD_EVENTS = new Set(['PermissionRequest', 'Elicitation', 'AskUserQuestion']);
const WATCHDOG_MS = EVENT !== undefined && HOLD_EVENTS.has(EVENT) ? 660_000 : 2_500;
const watchdog = setTimeout(() => {
  try {
    process.stdout.write('{}');
  } catch {
    /* gone */
  }
  process.exit(0);
}, WATCHDOG_MS);

// The token the shim exists to present. Absent only in a broken/odd install —
// the daemon has minted one at every boot since 0.16.0 — in which case we send
// no header and the daemon's 401 (fail-open for hooks) applies.
const TOKEN: string | null = readToken(HOME);

async function readStdinRaw(): Promise<string> {
  let data = '';
  try {
    for await (const chunk of process.stdin) {
      // The daemon refuses bodies past 1MB anyway — stop accumulating rather
      // than letting a wedged writer pin the shim's memory until the watchdog.
      if (data.length < 1024 * 1024)
        data += typeof chunk === 'string' ? chunk : (chunk as Buffer).toString('utf8');
    }
  } catch {
    /* empty body is fine */
  }
  return data;
}

try {
  const raw = await readStdinRaw();
  const headers: { 'content-type': string; authorization?: string } = {
    'content-type': 'application/json',
  };
  if (TOKEN) headers.authorization = `Bearer ${TOKEN}`;
  const ctl = new AbortController();
  const t = setTimeout(() => {
    ctl.abort();
  }, WATCHDOG_MS - 400);
  try {
    const res = await fetch(`${BASE}/hook/${String(EVENT)}`, {
      method: 'POST',
      headers,

      body: withRun(raw) || '{}',
      signal: ctl.signal,
    });
    const text = await res.text();
    // Forward the daemon's response verbatim — hook stdout is how the CLI
    // receives additionalContext / hold decisions. A 401 carries no contract
    // body; emit the fail-open no-op instead.
    const body = res.ok && text ? text : '{}';
    process.stdout.write(body);
  } finally {
    clearTimeout(t);
  }
} catch {
  try {
    process.stdout.write('{}');
  } catch {
    /* gone */
  }
}
clearTimeout(watchdog);
process.exit(0);
