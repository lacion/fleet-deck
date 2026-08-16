#!/usr/bin/env node
// tests/fixtures/crash-daemon-stub.ts
//
// A fleetd stand-in that crashes on startup WITHOUT ever binding its port,
// used by daemon-helper.test.ts to drive startDaemon()'s startup-failure
// path. It writes a marker file into FLEETDECK_HOME first, so the test can
// prove the helper removed the whole home (not just the process) — the marker
// stands in for the db/token/log/pid state a real crashed daemon would have
// left behind.

import fs from 'node:fs';
import path from 'node:path';

const HOME = process.env['FLEETDECK_HOME'] || '/tmp';
try {
  fs.writeFileSync(path.join(HOME, 'crashed.marker'), 'crash on startup\n', { mode: 0o600 });
} catch {
  /* the test's assertion target is the HOME dir itself */
}
console.error('crash-daemon-stub: simulated startup crash');
process.exit(1);
