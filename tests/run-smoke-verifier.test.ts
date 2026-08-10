// tests/run-smoke-verifier.test.ts
//
// BUG-190: demo/run-smoke.sh interpolated $DEMO_LOGS/$SA/$SB/$RC_A/$RC_B into
// single-quoted JavaScript literals inside the inline verifier. A checkout or
// log path containing an apostrophe corrupted the program, and the gate failed
// after spending the full model cost. The fix passes those values through node
// argv instead. This test extracts the real verifier invocation from the
// script, runs it through bash with an apostrophe-laden DEMO_LOGS, and asserts
// the verifier still parses and reports full PASS.

import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const RUN_SMOKE = path.join(REPO_ROOT, 'demo', 'run-smoke.sh');

// Extract the full `node --input-type=module -e "..." ...args` invocation from
// the script, exactly as bash will see it (interpolation and all).
function extractVerifierInvocation(scriptText: string): string {
  const m = /node --input-type=module -e "\n[\s\S]*?\n"[^\n]*\n/.exec(scriptText);
  assert.ok(m, 'could not locate the inline verifier invocation in demo/run-smoke.sh');
  return m[0];
}

// POSIX single-quote a value for safe embedding in a bash -c snippet.
function shq(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

test('run-smoke verifier survives an apostrophe in DEMO_LOGS (BUG-190)', (t) => {
  const base = mkdtempSync(path.join(tmpdir(), 'fleetdeck-smoke-verifier-'));
  t.after(() => {
    rmSync(base, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const demoLogs = path.join(base, "demo-logs-o'clock");
  mkdirSync(demoLogs);

  const sidA = 'aaaaaaaa-1111-2222-3333-444444444444';
  const sidB = 'bbbbbbbb-1111-2222-3333-444444444444';

  // Fixture state that satisfies every acceptance check in the verifier.
  writeFileSync(
    path.join(demoLogs, 'final-state.json'),
    JSON.stringify({
      sessions: [
        { session_id: sidA, callsign: 'alpha', col: 'offline', endedAt: '2026-08-05T00:00:00Z' },
        { session_id: sidB, callsign: 'bravo', col: 'offline', endedAt: '2026-08-05T00:00:00Z' },
      ],
      conflicts: [{ rel_path: 'util.js' }, { rel_path: 'test.js' }],
      ticker: [
        { msg: 'alpha got fleet mail at the turn boundary' },
        { msg: 'bravo got fleet mail at the turn boundary' },
      ],
    }),
  );
  for (const f of ['worker-a.json', 'worker-b.json']) {
    writeFileSync(
      path.join(demoLogs, f),
      JSON.stringify({
        is_error: false,
        subtype: 'success',
        result: 'done\nFLEET-NOTE: util.js',
      }),
    );
  }

  const invocation = extractVerifierInvocation(readFileSync(RUN_SMOKE, 'utf8'));
  const snippet = [
    `DEMO_LOGS=${shq(demoLogs)}`,
    `SA=${shq(sidA)}`,
    `SB=${shq(sidB)}`,
    'RC_A=0',
    'RC_B=0',
    invocation,
  ].join('\n');

  const run = spawnSync('bash', ['-c', snippet], { encoding: 'utf8' });
  assert.equal(
    run.status,
    0,
    `verifier exited ${String(run.status)}\nstdout:\n${run.stdout}\nstderr:\n${run.stderr}`,
  );
  assert.match(run.stdout, /PASS: both sessions registered/);
  assert.match(run.stdout, /PASS: conflict recorded on util\.js AND test\.js/);
  assert.match(run.stdout, /PASS: both tombstoned offline at the end/);
  assert.doesNotMatch(run.stdout, /^FAIL:/m);
});
