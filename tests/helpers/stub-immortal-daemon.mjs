#!/usr/bin/env node
// tests/helpers/stub-immortal-daemon.mjs
//
// A minimal, SIGTERM-IMMUNE stand-in for fleetd, used by the takeover suite's
// "fail open onto a wedged daemon" case. It is deliberately NOT the real
// daemon — it only needs to look like one to the SessionStart hook's takeover
// path, and then refuse to die:
//
//   - binds 127.0.0.1:FLEETDECK_PORT and answers GET /health with the same
//     shape the real daemon does ({ ok, pid, version, ... }) so the hook's
//     /health probe reads a legitimate-looking older daemon;
//   - writes FLEETDECK_HOME/fleetd.pid as {pid,port} (the HOME ownership lock)
//     so verifyDaemonPid()'s pidfile-match check passes;
//   - IGNORES SIGTERM (and SIGINT) so terminateDaemon()'s wait-for-death poll
//     times out and the hook is forced to fail open. ONLY SIGKILL ends it —
//     every test that spawns this stub MUST SIGKILL it in teardown.
//
// verifyDaemonPid() also runs livePidLooksLikeFleetd(), which on Linux inspects
// /proc/<pid>/cmdline for a `fleetd*.mjs` argument. This file's own name does
// not satisfy that regex, so the SPAWNER passes a disguise argument (the string
// 'fleetd.mjs') as argv[2]; without it the hook would reject the stub at the
// verify gate and never reach the SIGTERM the test is exercising.

import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';

const PORT = Number(process.env.FLEETDECK_PORT || 4711);
const HOME = process.env.FLEETDECK_HOME || '/tmp';
// Default to an OLD version so a real hook is strictly-newer and commits to the
// takeover; a test may override via FLEETDECK_VERSION_OVERRIDE.
const VERSION = (process.env.FLEETDECK_VERSION_OVERRIDE || '0.0.1').trim();

// Never die on the graceful signal — that immortality is the whole point.
process.on('SIGTERM', () => {});
process.on('SIGINT', () => {});

const server = http.createServer((req, res) => {
  if (req.url === '/health') {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({
      ok: true, pid: process.pid, version: VERSION, fleet: 0,
      spawn: { available: false },
    }));
    return;
  }
  res.writeHead(404);
  res.end();
});

server.listen(PORT, '127.0.0.1', () => {
  // Claim HOME the way the real daemon does, so verifyDaemonPid sees a matching
  // pidfile. Best-effort: the tests assert against /health, not this write.
  try {
    fs.writeFileSync(
      path.join(HOME, 'fleetd.pid'),
      JSON.stringify({ pid: process.pid, port: PORT }),
      { encoding: 'utf8', mode: 0o600 },
    );
  } catch { /* /health is the contract the test relies on */ }
});
