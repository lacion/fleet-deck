// tests/smoke-mail-gate.test.ts
//
// BUG-099: demo/run-smoke.sh used to send the `to:"all"` fanout mail on
// wall-clock sleeps (launch A, sleep 15, launch B, sleep 14, POST /mail).
// resolveTargets("all") only reaches ACTIVE sessions (ended_at IS NULL), so a
// worker that finished early — a faster model or machine — was silently
// omitted from the fanout and could never emit its Stop-boundary delivery
// ticker entry, turning correct behavior into a repeatable false FAIL.
// The smoke now gates every fanout step on the daemon's own /state: launch B
// only once A is proven registered and live, and mail only once BOTH exact
// session ids are. These tests pin the gate.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SMOKE = path.resolve(HERE, '../demo/run-smoke.sh');
const smoke = readFileSync(SMOKE, 'utf8');

test('smoke script has no wall-clock sleep between the worker launches and the fanout mail', () => {
  const launchA = smoke.indexOf('--session-id "$SA"');
  const mailPost = smoke.indexOf('curl -fsS -X POST "http://127.0.0.1:$FLEETDECK_PORT/mail"');
  assert.notEqual(launchA, -1, 'worker A launch found');
  assert.notEqual(mailPost, -1, 'mail POST found');
  const between = smoke.slice(launchA, mailPost);
  // Bash sleep lines only — the JS `sleep` inside the /state poll loop is the
  // gate itself, not wall-clock pacing.
  assert.ok(
    !/^\s*sleep\s+\d+/m.test(between),
    'no wall-clock sleep is allowed to pace the mail fanout — gate on fleet state instead',
  );
});

test('smoke script gates B launch and the mail POST on both sessions being proven active', () => {
  const launchB = smoke.indexOf('--session-id "$SB"');
  const mailPost = smoke.indexOf('curl -fsS -X POST "http://127.0.0.1:$FLEETDECK_PORT/mail"');
  assert.notEqual(launchB, -1, 'worker B launch found');
  assert.notEqual(mailPost, -1, 'mail POST found');

  // A must be proven registered/live before B is even launched...
  const beforeB = smoke.slice(0, launchB);
  assert.match(beforeB, /wait_for_fleet "\$SA"/, 'B launch is gated on A being active');

  // ...and BOTH exact session ids must be proven active before the to:"all"
  // fanout, so neither worker can be omitted by resolveTargets.
  const beforeMail = smoke.slice(0, mailPost);
  assert.match(
    beforeMail,
    /wait_for_fleet "\$SA" "\$SB"/,
    'mail is gated on both sessions being active',
  );

  // The gate itself must read the daemon's live /state and require every
  // listed sid to be present AND not ended (the same liveness rule
  // resolveTargets applies to the fanout).
  assert.match(smoke, /\/state/, 'gate polls the daemon /state');
  assert.match(smoke, /filter\(s => !s\.endedAt\)/, 'gate requires the sessions to not have ended');
  assert.match(
    smoke,
    /sids\.every\(sid => live\.has\(sid\)\)/,
    'gate requires every listed session id to be live',
  );
});
