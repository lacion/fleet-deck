// Regression for BUG-176: tests/spawn-repo.test.mjs carried a FILE-LOCAL,
// UNSCALED waitUntil (raw `Date.now() + timeoutMs`) that ignored
// FLEETDECK_TEST_WAIT_SCALE — so on the slow macOS advisory lane (issue #2,
// WAIT_SCALE=3) its clone-launch / tombstone waits kept the authored 12s
// budget instead of the intended 36s and produced false failures.
//
// The behavioural assertion runs helpers/wait-scaling-probe.mjs in a
// SUBPROCESS with the scale knob set (WAIT_SCALE is read once, at module
// load): the probe imports the target module and asserts BY IDENTITY that the
// target's waitUntil is the scaled shared helper from tests/helpers/wait.ts.
// A raw source scan pins the count of waitUntil occurrences in the target so a
// re-introduced file-local shadow (which would add occurrences) fails here.
//
// The probe's verdict is one JSON line on stdout. Under `node --test` a child
// process's stderr is folded into the TAP comment stream, so stdout is the
// reliable channel; the probe exits before the runner starts executing any of
// the target's registered test bodies, so no daemon churn can wedge it.

import test from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const execFileP = promisify(execFile);
const HERE = path.dirname(fileURLToPath(import.meta.url));
const TARGET = path.join(HERE, 'spawn-repo.test.mjs');
const PROBE = path.join(HERE, 'helpers/wait-scaling-probe.mjs');

test('every waitUntil in spawn-repo.test.mjs is the scaled shared helper (BUG-176)', async () => {
  // Every authored waitUntil must route through the ONE exported binding; a
  // second, file-local definition would add occurrences beyond that count.
  // (The import line contributes 1, the test-only export re-declaration 2,
  // and each call site 1 — the exact split is pinned by the count.)
  const occurrences = (readFileSync(TARGET, 'utf8').match(/\bwaitUntil\b/g) || []).length;
  const callSites = (readFileSync(TARGET, 'utf8').match(/[^.\w]waitUntil\(/g) || []).length;
  assert.equal(occurrences, callSites + 3,
    `expected ${callSites} call sites + import + export declaration (2); got ${occurrences} occurrences — a file-local waitUntil may have crept back in`);

  // Scrub NODE_TEST_CONTEXT from the child's environment: node REFUSES to run
  // `node --test` recursively inside a test worker ("node:test run() is being
  // called recursively ... skipping running files") and silently exits 0 with
  // empty stdout. The probe must be a real, fresh test run.
  const probeEnv = {
    ...process.env,
    FLEETDECK_TEST_WAIT_SCALE: '2',
    FLEETDECK_PROBE_TARGET: TARGET,
  };
  delete probeEnv.NODE_TEST_CONTEXT;
  let stdout, code = 0;
  try {
    ({ stdout } = await execFileP(process.execPath, ['--test', PROBE], {
      cwd: HERE,
      env: probeEnv,
      maxBuffer: 4 * 1024 * 1024,
      timeout: 60_000,
      killSignal: 'SIGKILL',
    }));
  } catch (err) {
    code = err.code;
    stdout = err.stdout ?? '';
  }
  // The probe runs UNDER `node --test`, whose TAP reporter comments the child
  // module's stdout (`# PROBE {...}`) — match with or without the prefix.
  const verdict = stdout.match(/^#?\s*PROBE (.+)$/m)?.[1];
  assert.ok(verdict, `probe subprocess emitted no verdict (exit ${code}): ${stdout.trim().split('\n').pop()}`);
  const { exported, ok } = JSON.parse(verdict);
  assert.equal(exported, 'function', 'spawn-repo.test.mjs must export its waitUntil binding for the scale check');
  assert.equal(ok, true, 'the waitUntil used by spawn-repo.test.mjs is NOT the scaled shared helper');
  assert.equal(code, 0);
});
