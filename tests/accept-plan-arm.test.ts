import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// BUG-014: the v1.3 plan gate's executor spawn is unsupervised
// (dangerously_skip_permissions: true), and the daemon refuses those with 403
// unless the body echoes a fresh single-use arm token from the bearer-gated
// POST /api/spawn/arm-unsupervised. The acceptance script must arm before it
// spawns, or the billed release gate can never validate unsupervised
// execution. The script itself is a never-auto-run live gate (it spends
// billed Claude usage), so this regression test statically pins the
// gate-6 request flow: bearer read, arm call, arm_token in the executor body.

const SCRIPT = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  '..',
  'demo',
  'run-accept-plan.sh',
);

function gate6Body(src: string) {
  const start = src.indexOf(
    '# --------------------------------------------------------------- gate 6',
  );
  const end = src.indexOf(
    '# --------------------------------------------------------------- gate 7',
  );
  assert.ok(start !== -1 && end > start, 'could not locate gates 6-7 in run-accept-plan.sh');
  return src.slice(start, end);
}

test('BUG-014: plan gate arms an unsupervised executor spawn before POSTing it', () => {
  const src = readFileSync(SCRIPT, 'utf8');
  const gate6 = gate6Body(src);

  // Reads the bearer the daemon minted into FLEETDECK_HOME/token.
  assert.match(
    src,
    /TOKEN="\$\(cat "\$SCRATCH_HOME\/token"/,
    'script must read the daemon bearer from $SCRATCH_HOME/token',
  );

  // Calls the bearer-gated arm endpoint before building the executor body.
  assert.match(
    gate6,
    /POST "\$BASE\/api\/spawn\/arm-unsupervised"/,
    'gate 6 must POST the arm-unsupervised endpoint',
  );
  assert.match(
    gate6,
    /authorization: Bearer \$TOKEN/,
    'the arm call must present the bearer token',
  );

  const armIdx = gate6.indexOf('/api/spawn/arm-unsupervised');
  const bodyIdx = gate6.indexOf('EXECUTOR_BODY=');
  const spawnIdx = gate6.indexOf('POST "$BASE/api/spawn"');
  assert.ok(
    armIdx !== -1 && bodyIdx > armIdx && spawnIdx > bodyIdx,
    'arm call must precede the executor body build and the executor spawn POST',
  );

  // The executor body echoes the minted single-use token.
  assert.match(
    gate6,
    /arm_token: process\.env\.ARM_TOKEN/,
    'executor body must include arm_token from the arm response',
  );

  // A refused arm fails immediately instead of attempting a doomed 403 spawn.
  assert.match(
    gate6,
    /bad "arm unsupervised spawn"/,
    'gate 6 must fail loud when the arm is refused',
  );
  assert.match(
    gate6,
    /if \[ -s "\$PLAN_FILE" \] && \[ -n "\$ARM_TOKEN" \]/,
    'executor spawn must be skipped when no arm token was minted',
  );
});
