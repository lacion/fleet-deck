// config.mjs — one place to resolve the daemon's runtime location from the
// environment, shared by every SOURCE entry point: the daemon (fleetd.mjs) and
// the SessionStart / watch hook scripts (scripts/fleet-*.mjs).
//
// bin/fleetdeck.mjs deliberately does NOT import this. The published npm package
// ships only `bin/` + the bundle + board-dist (see package.json "files"), never
// scripts/fleetd/*.mjs source, so the standalone CLI cannot import this module at
// runtime and keeps its own byte-identical HOME/PORT constants. Keep the two in
// sync by eye; a drift test would need the CLI to be importable, which it is not.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

// FLEETDECK_HOME, or ~/.fleetdeck — with a /tmp fallback for the (rare) case where
// the OS reports no home directory. Read from the environment on EVERY call, so a
// test can point it elsewhere per-process; the entry points capture it once at
// startup exactly as they did when this lived inline in each of them.
export function resolveHome() {
  return process.env.FLEETDECK_HOME || path.join(os.homedir() || '/tmp', '.fleetdeck');
}

// FLEETDECK_PORT, or the well-known default 4711.
export function resolvePort() {
  return Number(process.env.FLEETDECK_PORT || 4711);
}

// The loopback base URL the hook scripts POST their events to.
export function resolveBase(port = resolvePort()) {
  return `http://127.0.0.1:${port}`;
}

// Are we on a Coder workspace whose persisted disk is `/workspace`? Coder sets
// CODER / CODER_WORKSPACE_NAME / CODER_AGENT_URL in the agent environment; any
// one of them (non-empty) plus an actual `/workspace` directory is the signal.
// Returns the probe dir (default '/workspace') when both hold, else null — the
// caller then seeds the repos root and browse root there instead of ~/projects
// and ~. Both inputs are injected so the unit tests need neither a real Coder
// box nor a real /workspace: `{ env, probeDir }`.
export function detectCoderWorkspaceRoot({ env = process.env, probeDir = '/workspace' } = {}) {
  const present = v => typeof v === 'string' && v !== '';
  const onCoder = present(env.CODER) || present(env.CODER_WORKSPACE_NAME) || present(env.CODER_AGENT_URL);
  if (!onCoder) return null;
  try {
    if (fs.statSync(probeDir).isDirectory()) return probeDir;
  } catch { /* no /workspace — not a persisted-disk Coder box */ }
  return null;
}
