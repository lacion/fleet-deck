// tests/smoke-state-poll.test.ts
//
// BUG-101 — demo/run-smoke.sh must not snapshot /state immediately after the
// workers exit. The smoke's rendered SessionEnd hook is async ("async": true),
// and Claude Code does not await async hooks before exiting, so the tombstone
// post can still be in flight when `wait` returns. A single immediate /state
// fetch races the shim and falsely fails the lifecycle criterion on slower
// machines. The script must poll /state on a bounded deadline until both
// sessions are tombstoned (offline with endedAt) before capturing evidence.
//
// These are structural regressions on the script text: running the smoke for
// real spends Claude usage and requires `claude` on PATH, so the guarantee is
// pinned by asserting the capture is a bounded tombstone poll, not a one-shot
// fetch.

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../demo/run-smoke.sh');
const src = readFileSync(SCRIPT, 'utf8');

// Slice the capture region: after the second worker wait, before verify.
const waitB = src.indexOf('wait "$PB"');
const verify = !src.includes('final-state.json', waitB) ? -1 : src.indexOf('# ---', waitB);
assert.notEqual(waitB, -1, 'smoke script must wait for worker B');
assert.notEqual(verify, -1, 'smoke script must have a verify section after the waits');
const capture = src.slice(waitB, verify);

test('final-state capture is a bounded poll, not a one-shot fetch (BUG-101)', () => {
  assert.match(capture, /while/, 'capture must loop while tombstones are pending');
  assert.match(capture, /sleep/, 'poll must wait between attempts');
  assert.match(
    capture,
    /DEADLINE|deadline/,
    'poll must be bounded by a deadline, never an unbounded hang',
  );
});

test('poll gate requires both sessions tombstoned offline with endedAt (BUG-101)', () => {
  assert.match(capture, /col\s*={2,3}\s*["']offline["']/, 'gate must require col offline');
  assert.match(capture, /endedAt/, 'gate must require endedAt');
  assert.match(capture, /\$SA/, 'gate must check session A explicitly');
  assert.match(capture, /\$SB/, 'gate must check session B explicitly');
});
