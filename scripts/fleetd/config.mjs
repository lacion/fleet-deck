// config.mjs — one place to resolve the daemon's runtime location from the
// environment, shared by every SOURCE entry point: the daemon (fleetd.mjs) and
// the SessionStart / watch hook scripts (scripts/fleet-*.mjs).
//
// bin/fleetdeck.mjs deliberately does NOT import this. The published npm package
// ships only `bin/` + the bundle + board-dist (see package.json "files"), never
// scripts/fleetd/*.mjs source, so the standalone CLI cannot import this module at
// runtime and keeps its own byte-identical HOME/PORT constants. Keep the two in
// sync by eye; a drift test would need the CLI to be importable, which it is not.
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
