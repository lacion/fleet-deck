// tests/spec-record-cleanup.test.mjs
//
// BUG-213 regression: the spawn suites' spec-record scratch directories must
// not leak. Each spawn test records the FLEETDECK_SPAWN_CMD spec capture into
// <mkdtemp dir>/specs.jsonl but used to keep only the FILE path — the owning
// directory was lost the moment it was created, so teardown could never
// remove it and every successful run left its record dirs in the OS temp
// tree. The fix routes record-file creation through
// tests/helpers/wait.mjs's makeSpecRecordFile(t), which registers the owning
// directory's removal on t.after.
//
// Proof: run both spawn suites as child `node --test` processes with TMPDIR
// pointed at a fresh, empty directory. After a fully successful run that
// directory must contain NO leftover record dirs — before the fix it held
// every one of them (17 dirs containing specs.jsonl in the audit's run).

import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SUITES = ['spawn.test.mjs', 'spawn-unsupervised.test.mjs'];

function* walk(dir) {
  for (const entry of readdirSync(dir)) {
    const full = path.join(dir, entry);
    yield full;
    if (statSync(full).isDirectory()) yield* walk(full);
  }
}

test('BUG-213: spawn suites leave no spec-record scratch directories behind', { timeout: 300_000 }, async (t) => {
  const sandbox = mkdtempSync(path.join(tmpdir(), 'fleetdeck-spec-cleanup-'));
  t.after(() => { rmSync(sandbox, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  // NODE_TEST_CONTEXT is set by the outer `node --test` runner and inherited
  // by every child; a nested `node --test` that sees it refuses to run any
  // files ("run() is being called recursively"), silently exiting 0 with zero
  // tests. Strip it so the suite processes are real, independent runs.
  const { NODE_TEST_CONTEXT: _drop, ...parentEnv } = process.env;

  for (const suite of SUITES) {
    const result = spawnSync(process.execPath, ['--test', '--test-concurrency=1', '--test-reporter=tap', path.join(HERE, suite)], {
      encoding: 'utf8',
      timeout: 240_000,
      env: { ...parentEnv, TMPDIR: sandbox },
    });
    assert.equal(result.status, 0,
      `${suite} must pass inside the TMPDIR sandbox (status ${result.status})\n--- stdout ---\n${result.stdout}\n--- stderr ---\n${result.stderr}`);
    const passCount = (result.stdout.match(/^# pass (\d+)$/m) || [])[1];
    assert.ok(Number(passCount) > 0,
      `${suite} must actually RUN its tests inside the sandbox (a silent all-skip would make this regression test vacuous); TAP summary:\n${result.stdout.slice(-600)}`);
  }

  const leftovers = [...walk(sandbox)].filter(p => p !== sandbox);
  assert.deepEqual(leftovers, [],
    `no spec-record scratch dirs/files may survive a successful spawn-suite run; found: ${leftovers.join(', ')}`);
});
