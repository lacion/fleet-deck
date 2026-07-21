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
import { resolveHome, resolvePort, resolveBase } from './fleetd/config.mjs';

const EVENT = process.argv[2];
const HOME = resolveHome();
const BASE = resolveBase(resolvePort());

// Hook events whose daemon response parks until the board answers (the
// hold-open relay in http.mjs). Their watchdog must sit just inside the 65s
// hooks.json timeout; everything else mirrors the 3s telemetry hooks.
const HOLD_EVENTS = new Set(['PermissionRequest', 'Elicitation', 'AskUserQuestion']);
const WATCHDOG_MS = HOLD_EVENTS.has(EVENT) ? 63_000 : 2_500;
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
      method: 'POST', headers, body: raw || '{}', signal: ctl.signal,
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
