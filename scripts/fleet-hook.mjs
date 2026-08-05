#!/usr/bin/env node
// fleet-hook.mjs — authenticated command-shim for every hook event except
// SessionStart (fleet-sessionstart.mjs) and the Stop asyncRewake leg
// (fleet-watch.mjs).
//
// Why this exists: Claude Code http hooks cannot attach an Authorization
// header, so the daemon used to exempt /hook/* from the bearer entirely — and
// ANY local process could impersonate a session (forge /clear succession,
// plant permission holds, drain mailboxes) with one curl. This shim is a
// command hook, so it can do what an http hook cannot: read the daemon's token
// file and present it. hooks.json routes every former http hook through here.
//
// Design rule (same as fleet-sessionstart.mjs): NEVER break the session. Every
// failure path is a silent exit 0 with '{}' on stdout, and the per-event
// watchdog guarantees we are gone before hooks.json's own timeout would fire.

import fs from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { resolveHome, resolvePort, resolveBase } from './fleetd/config.mjs';

const EVENT = process.argv[2];
const HOME = resolveHome();
const BASE = resolveBase(resolvePort());

// Run generation (BUG-025): ONE nonce per CLI process, minted by whichever
// shim runs first (the SessionStart hook, or this one when a mid-session event
// is the process's first) and shared via a dotfile in FLEETDECK_HOME —
// hooks spawn a fresh shim process per event, so the file is the handoff.
// SessionStart persists it as the card's active run; the daemon then refuses
// to tombstone on a SessionEnd from any OTHER (older, delayed-async) run of
// the same session id. Mint-and-attach only when the payload carries none —
// never overwrite a nonce a newer daemon-side flow already set. Any failure
// leaves the event untagged, which the daemon treats as the historical
// unconditional-tombstone path (fail open, never break the session).
let RUN = null;
try {
  const runFile = path.join(HOME, `run-${process.ppid}`);
  try {
    RUN = fs.readFileSync(runFile, 'utf8').trim() || null;
  } catch {
    RUN = randomUUID();
    // 0600 like the rest of HOME's state (token, log, db). A stale file from a
    // crashed CLI could be re-read by an unrelated later process only if the
    // pid was recycled AND the event carries the same session_id — worst case
    // that session's own SessionEnd is then skipped, failing safe toward a
    // live card the retention sweep later settles.
    fs.writeFileSync(runFile, RUN, { mode: 0o600 });
  }
} catch { RUN = null; }

function withRun(raw) {
  if (!RUN) return raw;
  try {
    const body = raw ? JSON.parse(raw) : {};
    if (body && typeof body === 'object' && body.fleet_run == null) {
      body.fleet_run = RUN;
      return JSON.stringify(body);
    }
  } catch { /* unparseable body — forward verbatim */ }
  return raw;
}

// Hook events whose daemon response parks until the board answers (the
// hold-open relay in http.mjs). THE LOCKSTEP INVARIANT: the daemon's hold
// window (questions.mjs resolveHoldMs, default 600 s, clamp ceiling 650 s)
// must stay UNDER this watchdog, and this watchdog must stay UNDER the
// hooks.json `timeout` for these three events (720 s) — otherwise a board
// answer lands on a dead socket and the hook fails open. All three places
// carry this comment; change them together. The shim does not know the
// daemon's configured window, so the watchdog tracks the hooks.json timeout
// (its own hard ceiling) with margin.
const HOLD_EVENTS = new Set(['PermissionRequest', 'Elicitation', 'AskUserQuestion']);
const WATCHDOG_MS = HOLD_EVENTS.has(EVENT) ? 660_000 : 2_500;
const watchdog = setTimeout(() => { try { process.stdout.write('{}'); } catch { /* gone */ } process.exit(0); }, WATCHDOG_MS);

// The token the shim exists to present. Absent only in a broken/odd install —
// the daemon has minted one at every boot since 0.16.0 — in which case we send
// no header and the daemon's 401 (fail-open for hooks) applies.
let TOKEN = null;
try { TOKEN = fs.readFileSync(path.join(HOME, 'token'), 'utf8').trim() || null; } catch { /* no token file */ }

async function readStdinRaw() {
  let data = '';
  try {
    for await (const chunk of process.stdin) {
      // The daemon refuses bodies past 1MB anyway — stop accumulating rather
      // than letting a wedged writer pin the shim's memory until the watchdog.
      if (data.length < 1024 * 1024) data += chunk;
    }
  } catch { /* empty body is fine */ }
  return data;
}

try {
  const raw = await readStdinRaw();
  const headers = { 'content-type': 'application/json' };
  if (TOKEN) headers.authorization = `Bearer ${TOKEN}`;
  const ctl = new AbortController();
  const t = setTimeout(() => ctl.abort(), WATCHDOG_MS - 400);
  try {
    const res = await fetch(`${BASE}/hook/${EVENT}`, {
      method: 'POST', headers, body: withRun(raw) || '{}', signal: ctl.signal,
    });
    const text = await res.text();
    // Forward the daemon's response verbatim — hook stdout is how the CLI
    // receives additionalContext / hold decisions. A 401 carries no contract
    // body; emit the fail-open no-op instead.
    process.stdout.write(res.ok && text ? text : '{}');
  } finally { clearTimeout(t); }
} catch { try { process.stdout.write('{}'); } catch { /* gone */ } }
clearTimeout(watchdog);
process.exit(0);
