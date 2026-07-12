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

// The daemon's env seeds any tmux SERVER it creates: tmux bakes the FIRST
// client's environment into the server's global env, which every later pane
// inherits (the 2026-07-11 ghost-daemon scar — a test-run daemon poisoned the
// default server with a test FLEETDECK_PORT/HOME). This hook runs INSIDE a
// Claude session, so scrub the session markers before boot. Deliberately does
// NOT scrub FLEETDECK_* tuning knobs — tests/demos pass those through here on
// purpose. Keep the marker list in sync with the spawn() scrub in
// fleetd/derive.mjs.
function bootEnv() {
  const env = { ...process.env, FLEETDECK_PORT: String(PORT), FLEETDECK_HOME: HOME };
  for (const k of [
    'CLAUDECODE', 'CLAUDE_CODE_SESSION_ID', 'CLAUDE_CODE_CHILD_SESSION',
    'CLAUDE_CODE_BRIDGE_SESSION_ID', 'CLAUDE_CODE_ENTRYPOINT', 'CLAUDE_CODE_EXECPATH',
    'CLAUDE_ENV_FILE', 'CLAUDE_PROJECT_DIR', 'CLAUDE_PLUGIN_ROOT', 'CLAUDE_PLUGIN_DATA',
    'CLAUDE_EFFORT', 'AI_AGENT', 'CODEX_COMPANION_TRANSCRIPT_PATH',
    'CODEX_COMPANION_SESSION_ID', 'TMUX', 'TMUX_PANE',
  ]) delete env[k];
  return env;
}

// Election: whoever gets here first launches fleetd. The port bind is the
// lock — a concurrent launcher's daemon exits 3 on EADDRINUSE and we just poll.
async function ensureServer() {
  if (await api('/health', { timeout: 250 })) return true;
  let out = null;
  try {
    fs.mkdirSync(HOME, { recursive: true });
    const logFile = path.join(HOME, 'fleetd.log');
    // WHY mode is not enough: open(append) preserves an existing file's old
    // permissions. chmod repairs logs created by older versions before this
    // hook gives a daemon another chance to write credentials into them.
    out = fs.openSync(logFile, 'a', 0o600);
    fs.chmodSync(logFile, 0o600);
    const child = spawn(process.execPath, ['--no-warnings=ExperimentalWarning', FLEETD], {
      detached: true,
      stdio: ['ignore', out, out],
      env: bootEnv(),
    });
    // spawn() reports resource exhaustion and similar launch failures on the
    // next turn. Without a listener that 'error' would violate this hook's
    // foundational promise to fail silently instead of breaking SessionStart.
    child.once('error', () => {});
    child.unref();
  } catch { return false; }
  finally {
    // The detached child owns duplicated descriptors after a successful
    // spawn; the launcher must release its copy on every success/failure path.
    if (out !== null) try { fs.closeSync(out); } catch { /* silent hook */ }
  }
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
