// run-tests-filewise.mjs — run every test file DIRECTLY, one process per file,
// with no `node --test` runner in front.
//
// WHY THIS EXISTS (remove when it stops being true): the Node 22.x test
// runner's parent<->child IPC has a deserialization bug — under CI load the
// parent throws "Unable to deserialize cloned data due to invalid or
// unsupported version" out of #processRawBuffer and fails whole test files
// that pass everywhere else (nodejs/node#64061; fixed by nodejs/node#64706,
// which current Node 24 already ships — our Node 24 lane is green on the
// plain runner — but whose 22.x backport has not been released as of
// 2026-08). Running each file as a plain script executes node:test
// IN-PROCESS: no runner child, no IPC, nothing to mis-deserialize, while the
// one-process-per-file loop below preserves exactly the isolation the runner
// gives us (fresh process, fresh env, per-file exit code).
//
// REVERT PATH: when a Node 22.x release carries the #64706 backport, bump the
// CI floor to it and point the Node 22 lanes back at plain `npm test`.
//
// Semantics kept identical to `npm test`:
//   - files run sequentially (the suite's contract is --test-concurrency=1)
//   - per-test {timeout} options still apply (node:test honors them in-process)
//   - a wedged FILE is killed at the same 300s ceiling npm test uses per test,
//     scaled to the file (600s), and named loudly instead of hanging the job
//   - FLEETDECK_TEST_DAEMON_SCRIPT et al. pass through the environment

import { spawnSync } from 'node:child_process';
import { readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const TESTS_DIR = path.join(REPO_ROOT, 'tests');

// Recursive: `node --test` discovers test files in subdirectories too
// (tests/helpers/gitrepo.test.mjs), and a filewise run that silently drops a
// file is exactly the vacuous-pass hazard this suite guards against — the
// summary line prints the file count so a discovery regression is visible,
// and the per-file `# tests` totals let CI logs be diffed against `npm test`.
const files = readdirSync(TESTS_DIR, { recursive: true })
  .filter(name => String(name).endsWith('.test.mjs'))
  .sort()
  .map(name => path.join('tests', String(name)));

if (files.length === 0) {
  console.error('run-tests-filewise: no tests/*.test.mjs files found');
  process.exit(1);
}

const failures = [];
const started = Date.now();
for (const [i, file] of files.entries()) {
  console.log(`\n# [${i + 1}/${files.length}] ${file}`);
  const result = spawnSync(process.execPath, [file], {
    cwd: REPO_ROOT,
    stdio: 'inherit',
    env: process.env,
    timeout: 600_000,
  });
  if (result.status !== 0) {
    const why = result.signal
      ? `killed by ${result.signal}${result.error?.code === 'ETIMEDOUT' ? ' (600s file watchdog)' : ''}`
      : `exit ${result.status}`;
    failures.push(`${file} — ${why}`);
    console.error(`# FAIL ${file} (${why})`);
  }
}

const secs = Math.round((Date.now() - started) / 1000);
console.log(`\n# filewise summary: ${files.length - failures.length}/${files.length} files passed in ${secs}s`);
if (failures.length) {
  console.error('# failing files:');
  for (const f of failures) console.error(`#   ${f}`);
  process.exit(1);
}
