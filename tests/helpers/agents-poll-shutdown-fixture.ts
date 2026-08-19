// Direct-argv stand-in for `claude agents --json` used by the real-daemon P1
// shutdown proof. It publishes its pid before waiting so the test can signal
// fleetd only after execFileP owns a live child. A release marker starts a delay
// longer than the historical 1.5 s root watchdog but shorter than the poller's
// 5 s exec timeout; the eventual empty registry would tombstone every live
// agents-cli card if startAgentsPoll delivered it after stop began.

import { existsSync, writeFileSync } from 'node:fs';

const startedFile = process.env['FLEETDECK_TEST_AGENTS_POLL_STARTED'];
const releaseFile = process.env['FLEETDECK_TEST_AGENTS_POLL_RELEASE'];
const completedFile = process.env['FLEETDECK_TEST_AGENTS_POLL_COMPLETED'];

if (!startedFile || !releaseFile || !completedFile) {
  throw new Error('agents-poll shutdown fixture paths are required');
}

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

writeFileSync(startedFile, `${process.pid}\n`);

// Bound a failed test that never publishes its release marker. In the normal
// path this wait is only a few milliseconds, leaving ample room under the real
// 5 s exec timeout for the deliberate 2 s in-flight interval below.
const releaseDeadline = Date.now() + 2_000;
while (!existsSync(releaseFile)) {
  if (Date.now() >= releaseDeadline) throw new Error('agents-poll release marker timed out');
  await sleep(10);
}

await sleep(2_000);
writeFileSync(completedFile, `${Date.now()}\n`);
process.stdout.write('[]');
