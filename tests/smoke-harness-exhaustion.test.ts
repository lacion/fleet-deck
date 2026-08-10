// tests/smoke-harness-exhaustion.test.ts
//
// BUG-018: the smoke workers run under `timeout 300` and the launches must
// NOT pin a hard --max-turns ceiling. A fixed 24-turn cap cuts a successful
// worker off mid-tool-use so the Stop hook never fires, and the verifier then
// reports both harness exhaustion and the necessarily missing Stop-boundary
// delivery as product failures. This test pins two properties of
// demo/run-smoke.sh:
//   1. the claude launches carry no --max-turns flag (the authored 300s
//      wall-clock timeout remains the safety bound);
//   2. the verifier classifies harness exhaustion (rc 124 or a max-turns
//      error result) as INCONCLUSIVE and unscored rather than FAIL.

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { spawnSync, type SpawnSyncReturns } from 'node:child_process';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const smokePath = path.join(repoRoot, 'demo', 'run-smoke.sh');
const smoke = readFileSync(smokePath, 'utf8');

interface RenderVars {
  demoLogs: string;
  sidA: string;
  sidB: string;
  rcA: number;
  rcB: number;
}

// Extract the inline node verifier from run-smoke.sh and render its
// bash-interpolated placeholders ($DEMO_LOGS, $SA, $SB, $RC_A, $RC_B) with
// concrete values, exactly as the shell would.
function renderVerifier(vars: RenderVars): string {
  const startToken = 'node --input-type=module -e "';
  const start = smoke.indexOf(startToken);
  assert.notEqual(start, -1, 'verifier launch line present in run-smoke.sh');
  const end = smoke.indexOf('\n"', start);
  assert.notEqual(end, -1, 'verifier closing quote present');
  const script = smoke.slice(start + startToken.length, end);
  return script
    .replace(/const demoLogs = '[^']*';/, `const demoLogs = ${JSON.stringify(vars.demoLogs)};`)
    .replace(/const sidA = '[^']*';/, `const sidA = '${vars.sidA}';`)
    .replace(/const sidB = '[^']*';/, `const sidB = '${vars.sidB}';`)
    .replace(/const rcA = Number\('[^']*'\);/, `const rcA = ${vars.rcA};`)
    .replace(/const rcB = Number\('[^']*'\);/, `const rcB = ${vars.rcB};`);
}

function runVerifier(script: string): SpawnSyncReturns<string> {
  return spawnSync('node', ['--input-type=module', '-e', script], { encoding: 'utf8' });
}

test('run-smoke.sh launches workers without a hard --max-turns ceiling', () => {
  const launches = smoke.match(/setsid timeout 300 claude -p/g) ?? [];
  assert.ok(launches.length >= 2, 'expected the two worker launches under timeout 300');
  assert.ok(!smoke.includes('--max-turns'), 'no --max-turns flag anywhere in run-smoke.sh');
});

test('run-smoke.sh verifier treats harness exhaustion as inconclusive, not failure', (t) => {
  const tmp = mkdtempSync(path.join(tmpdir(), 'bugbash-018-'));
  t.after(() => {
    rmSync(tmp, { recursive: true, force: true });
  });

  const baseState = {
    sessions: [
      { session_id: 'sid-a', callsign: 'alpha', col: 'offline', endedAt: 1 },
      { session_id: 'sid-b', callsign: 'bravo', col: 'offline', endedAt: 2 },
    ],
    conflicts: [{ rel_path: 'util.js' }, { rel_path: 'test.js' }],
    ticker: [
      { msg: 'alpha got fleet mail at the turn boundary' },
      { msg: 'bravo got fleet mail at the turn boundary' },
    ],
  };
  const healthy = { is_error: false, subtype: 'success', result: 'done' };
  const cutOff = { is_error: true, subtype: 'error_max_turns', num_turns: 25 };

  const run = (rcB: number): SpawnSyncReturns<string> =>
    runVerifier(renderVerifier({ demoLogs: tmp, sidA: 'sid-a', sidB: 'sid-b', rcA: 0, rcB }));

  // Healthy run: everything passes.
  writeFileSync(path.join(tmp, 'final-state.json'), JSON.stringify(baseState));
  writeFileSync(path.join(tmp, 'worker-a.json'), JSON.stringify(healthy));
  writeFileSync(path.join(tmp, 'worker-b.json'), JSON.stringify(healthy));
  let out = run(0);
  assert.equal(
    out.status,
    0,
    'healthy run must exit 0; stderr=' + out.stderr + ' stdout=' + out.stdout,
  );
  assert.ok(
    out.stdout.includes('PASS: mail delivered at Stop boundary to both sessions'),
    out.stdout,
  );

  // Harness-exhausted run: worker B cut off at a turn ceiling, so its Stop
  // hook never fired and its boundary delivery line is absent. Must NOT fail;
  // must report inconclusive.
  writeFileSync(
    path.join(tmp, 'final-state.json'),
    JSON.stringify({
      ...baseState,
      ticker: [{ msg: 'alpha got fleet mail at the turn boundary' }],
    }),
  );
  writeFileSync(path.join(tmp, 'worker-b.json'), JSON.stringify(cutOff));
  out = run(0);
  assert.equal(
    out.status,
    0,
    'harness-exhausted run must not fail; stderr=' + out.stderr + ' stdout=' + out.stdout,
  );
  assert.ok(
    out.stdout.includes('INCONCLUSIVE'),
    'expected INCONCLUSIVE classification; stdout=' + out.stdout,
  );
  assert.ok(!out.stdout.includes('FAIL: mail delivered at Stop boundary'), out.stdout);
  assert.ok(!out.stdout.includes('FAIL: worker B completed successfully'), out.stdout);

  // Wall-clock exhaustion (rc 124) is also inconclusive, never a failure.
  writeFileSync(path.join(tmp, 'worker-b.json'), JSON.stringify(healthy));
  out = run(124);
  assert.equal(
    out.status,
    0,
    'rc=124 run must not fail; stderr=' + out.stderr + ' stdout=' + out.stdout,
  );
  assert.ok(out.stdout.includes('INCONCLUSIVE'), out.stdout);

  // Genuine product failure still fails: worker B ends with an error result
  // that is NOT harness exhaustion.
  writeFileSync(path.join(tmp, 'final-state.json'), JSON.stringify(baseState));
  writeFileSync(
    path.join(tmp, 'worker-b.json'),
    JSON.stringify({ is_error: true, subtype: 'error_during_execution' }),
  );
  out = run(0);
  assert.equal(out.status, 1, 'real product failure must still exit 1; stdout=' + out.stdout);
  assert.ok(out.stdout.includes('FAIL: worker B completed successfully'), out.stdout);
});
