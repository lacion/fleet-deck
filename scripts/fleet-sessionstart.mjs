#!/usr/bin/env node
// fleet-sessionstart.mjs — the ONLY command hook. Election + spawn + brief.
//
// Reads the SessionStart hook payload on stdin, makes sure fleetd is up
// (health check → spawn detached → poll ~3 s), POSTs /hook/SessionStart and
// prints the daemon-composed roster brief to stdout (SessionStart stdout is
// added to the session context).
//
// Design rule #1: this script must NEVER break the session. EVERY failure
// path is a silent exit 0, and a watchdog guarantees we are gone in ~4 s.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const PORT = Number(process.env.FLEETDECK_PORT || 4711);
const BASE = `http://127.0.0.1:${PORT}`;
const HERE = path.dirname(fileURLToPath(import.meta.url));
// Prefer the committed bundle (self-contained — git-distributed installs have
// no node_modules); fall back to source for dev checkouts mid-iteration.
const FLEETD_BUNDLE = path.join(HERE, 'fleetd', 'fleetd.bundle.mjs');
const FLEETD = fs.existsSync(FLEETD_BUNDLE) ? FLEETD_BUNDLE : path.join(HERE, 'fleetd', 'fleetd.mjs');
const HOME = process.env.FLEETDECK_HOME || path.join(os.homedir() || '/tmp', '.fleetdeck');

// Hard deadline: whatever happens, exit 0 well inside the hook timeout.
const watchdog = setTimeout(() => process.exit(0), 3800);

async function readStdin() {
  let data = '';
  try {
    for await (const chunk of process.stdin) data += chunk;
    return JSON.parse(data || '{}');
  } catch { return {}; }
}

async function api(pathname, { method = 'GET', body, timeout = 400 } = {}) {
  const ctl = new AbortController();
  const t = setTimeout(() => ctl.abort(), timeout);
  try {
    const res = await fetch(BASE + pathname, {
      method,
      headers: body ? { 'content-type': 'application/json' } : undefined,
      body: body ? JSON.stringify(body) : undefined,
      signal: ctl.signal,
    });
    return await res.json();
  } catch { return null; }
  finally { clearTimeout(t); }
}

// Election: whoever gets here first launches fleetd. The port bind is the
// lock — a concurrent launcher's daemon exits 3 on EADDRINUSE and we just poll.
async function ensureServer() {
  if (await api('/health', { timeout: 250 })) return true;
  try {
    fs.mkdirSync(HOME, { recursive: true });
    const out = fs.openSync(path.join(HOME, 'fleetd.log'), 'a');
    const child = spawn(process.execPath, ['--no-warnings=ExperimentalWarning', FLEETD], {
      detached: true,
      stdio: ['ignore', out, out],
      env: { ...process.env, FLEETDECK_PORT: String(PORT), FLEETDECK_HOME: HOME },
    });
    child.unref();
    fs.closeSync(out);
  } catch { return false; }
  for (let i = 0; i < 12; i++) {
    await new Promise(r => setTimeout(r, 250));
    if (await api('/health', { timeout: 250 })) return true;
  }
  return false;
}

try {
  const payload = await readStdin();
  payload.hook_event_name = payload.hook_event_name || 'SessionStart';
  if (await ensureServer()) {
    const reg = await api('/hook/SessionStart', { method: 'POST', body: payload, timeout: 1200 });
    if (reg?.brief) process.stdout.write(reg.brief);
  }
} catch { /* no fleet, no drama */ }
clearTimeout(watchdog);
process.exit(0);
