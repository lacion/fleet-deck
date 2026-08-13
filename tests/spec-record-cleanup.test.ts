// tests/spec-record-cleanup.test.ts
//
// BUG-213 regression: the spawn suites' spec-record scratch directories must
// not leak. Each spawn test records the FLEETDECK_SPAWN_CMD spec capture into
// <mkdtemp dir>/specs.jsonl but used to keep only the FILE path — the owning
// directory was lost the moment it was created, so teardown could never
// remove it and every successful run left its record dirs in the OS temp
// tree. The fix routes record-file creation through
// tests/helpers/wait.ts's makeSpecRecordFile(t), which registers the owning
// directory's removal on t.after.
//
// Proof: run both spawn suites as child `node --test` processes with TMPDIR
// pointed at a fresh, empty directory. After a fully successful run that
// directory must contain NO leftover record dirs — before the fix it held
// every one of them (17 dirs containing specs.jsonl in the audit's run).

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { childRunArgv, childPassCount } from './helpers/child-runner.ts';

const HERE = path.dirname(fileURLToPath(import.meta.url));
// These are runtime `node --test` targets resolved on disk, not TS imports.
const SUITES = ['spawn.test.ts', 'spawn-unsupervised.test.ts'];

function* walk(dir: string): Generator<string> {
  for (const entry of readdirSync(dir)) {
    const full = path.join(dir, entry);
    yield full;
    if (statSync(full).isDirectory()) yield* walk(full);
  }
}

test(
  'BUG-213: spawn suites leave no spec-record scratch directories behind',
  { timeout: 300_000 },
  (t) => {
    const sandbox = mkdtempSync(path.join(tmpdir(), 'fleetdeck-spec-cleanup-'));
    t.after(() => {
      rmSync(sandbox, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });

    // NODE_TEST_CONTEXT is set by the outer `node --test` runner and inherited
    // by every child; a nested `node --test` that sees it refuses to run any
    // files ("run() is being called recursively"), silently exiting 0 with zero
    // tests. Strip it so the suite processes are real, independent runs.
    const parentEnv: NodeJS.ProcessEnv = { ...process.env };
    Reflect.deleteProperty(parentEnv, 'NODE_TEST_CONTEXT');

    for (const suite of SUITES) {
      const argv = childRunArgv({ file: path.join(HERE, suite), serial: true });
      const result = spawnSync(process.execPath, argv, {
        encoding: 'utf8',
        timeout: 240_000,
        env: { ...parentEnv, TMPDIR: sandbox },
      });
      // node prints `# pass N` on stdout, bun ` N pass` on stderr — parse combined.
      const out = `${result.stdout ?? ''}${result.stderr ?? ''}`;
      assert.equal(
        result.status,
        0,
        `${suite} must pass inside the TMPDIR sandbox (status ${String(result.status)})\n${out}`,
      );
      assert.ok(
        childPassCount(out) > 0,
        `${suite} must actually RUN its tests inside the sandbox (a silent all-skip would make this regression test vacuous); summary:\n${out.slice(-600)}`,
      );
    }

    const leftovers = [...walk(sandbox)].filter((p) => p !== sandbox);
    assert.deepEqual(
      leftovers,
      [],
      `no spec-record scratch dirs/files may survive a successful spawn-suite run; found: ${leftovers.join(', ')}`,
    );
  },
);
