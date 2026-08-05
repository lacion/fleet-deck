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
//
// ALWAYS ABSOLUTE CONTRACT: the daemon and every hook/watch process run this
// resolver independently, each from its own working directory — a relative
// FLEETDECK_HOME would silently fork one fleet's state (token, pidfile, DB) into
// one tree per cwd, so every process would hold a different token for the same
// daemon and authenticated updates would be refused. Anchor a relative value to
// the user's home (the documented base) so all processes converge on ONE state
// dir, and normalize the result so dot segments can never fork identity either.
export function resolveHome() {
  const fallbackBase = os.homedir() || '/tmp';
  const configured = process.env.FLEETDECK_HOME?.trim();
  if (!configured) return path.join(fallbackBase, '.fleetdeck');
  if (!path.isAbsolute(configured)) return path.resolve(fallbackBase, configured);
  return path.normalize(configured);
}

// FLEETDECK_PORT, or the well-known default 4711. Must be an integer in
// 1..65535: port 0 asks Node for an ephemeral port, but the pidfile, health
// checks, hooks and board URLs would all keep advertising the literal 0 — a
// live daemon no client can reach. Reject it (and every other non-port value)
// here, before the daemon claims HOME under an unusable identity. The daemon
// (fleetd.mjs) catches this throw at startup and refuses BEFORE touching HOME;
// it also re-validates the returned range itself, so it stays safe under
// either resolvePort contract.
export function resolvePort() {
  const raw = process.env.FLEETDECK_PORT;
  if (raw === undefined || raw === '') return 4711;
  const port = Number(raw);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`invalid FLEETDECK_PORT ${JSON.stringify(raw)} — expected an integer port in 1..65535 (port 0 is not supported)`);
  }
  return port;
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
