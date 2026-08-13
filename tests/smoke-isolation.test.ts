// tests/smoke-isolation.test.ts
//
// BUG-098: the smoke's `claude -p` workers must not inherit ambient
// machine-local configuration. Without a pinned model/effort and a restricted
// set of settings sources, a contributor's hooks, plugins, default-model
// override, or settings.local.json changes the model, side effects, timing,
// and cost of the run — identical source can pass on one machine and fail on
// another. This test asserts the static invariants of demo/run-smoke.sh; it
// never launches Claude.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const smoke = readFileSync(path.join(REPO_ROOT, 'demo', 'run-smoke.sh'), 'utf8');

// The two worker launches are the only `claude -p` invocations in the script.
// Each launch spans several backslash-continued lines; join each launch from
// its `claude -p` line through the redirect line so flags on continuation
// lines are asserted against too.
const workerLines: string[] = [];
{
  const lines = smoke.split('\n');
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    if (line === undefined || !line.includes('claude -p ') || !line.includes('setsid')) continue;
    const launch = [line];
    while (launch.at(-1)?.endsWith('\\') && i + 1 < lines.length) {
      i += 1;
      const next = lines[i];
      if (next === undefined) break;
      launch.push(next);
    }
    workerLines.push(launch.join('\n'));
  }
}

test('run-smoke.sh launches exactly two claude -p workers (test asserts against every one)', () => {
  assert.equal(workerLines.length, 2, `expected 2 worker launches, found ${workerLines.length}`);
});

test('every smoke worker pins the model, the effort, and the settings sources', () => {
  for (const line of workerLines) {
    assert.match(
      line,
      /--model "\$SMOKE_MODEL"/,
      'worker must pin --model from the smoke-owned SMOKE_MODEL, not the ambient default',
    );
    assert.match(
      line,
      /--effort "\$SMOKE_EFFORT"/,
      'worker must pin --effort from the smoke-owned SMOKE_EFFORT, not the ambient default',
    );
    assert.match(
      line,
      /--setting-sources user,project/,
      'worker must exclude the local settings source (settings.local.json) so machine-local hooks never join the run',
    );
  }
});

test('run-smoke.sh defines smoke-owned model/effort defaults that ambient config cannot re-target', () => {
  // Follows the FLEETDECK_SMOKE_PORT pattern: a smoke-specific override name,
  // never a generic one an ambient environment might already export.
  assert.match(
    smoke,
    /SMOKE_MODEL="\$\{FLEETDECK_SMOKE_MODEL:-[^}]+\}"/,
    'SMOKE_MODEL must default inline and accept only the smoke-specific override',
  );
  assert.match(
    smoke,
    /SMOKE_EFFORT="\$\{FLEETDECK_SMOKE_EFFORT:-[^}]+\}"/,
    'SMOKE_EFFORT must default inline and accept only the smoke-specific override',
  );
});
