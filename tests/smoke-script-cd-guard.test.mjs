// tests/smoke-script-cd-guard.test.mjs
//
// BUG-017 regression: demo/run-smoke.sh enabled nounset but not errexit, so a
// failed `cd "$PROJECT_DIR"` (fixture deleted or non-enterable between setup
// and launch) did not stop the script — the two `claude -p
// --dangerously-skip-permissions` workers then launched in the CALLER's
// directory and edited util.js/test.js outside the fixture.
//
// This test builds a scrubbed copy of the script in a tmp dir: a real fixture
// that becomes non-enterable right before the cd, and the paid worker
// launches replaced by a marker. The scrubbed copy runs from a different cwd;
// the script must ABORT at the cd instead of reaching the launch site. The
// marker doubles as proof of where the workers would have run: with the guard
// in place it is never written; without it, it lands in the caller's
// directory.

import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const SCRIPT = join(REPO_ROOT, 'demo', 'run-smoke.sh');

test('run-smoke.sh aborts on a failed cd instead of launching unrestricted workers in the caller cwd', () => {
  const work = mkdtempSync(join(tmpdir(), 'smoke-cd-guard-'));
  try {
    const fixture = join(work, 'project');
    const scrubHome = join(work, 'scratch');
    const callerCwd = join(work, 'caller');
    mkdirSync(join(fixture, '.seed'), { recursive: true });
    writeFileSync(join(fixture, '.seed', 'util.js'), 'export {};\n');
    writeFileSync(join(fixture, '.seed', 'app.js'), 'export {};\n');
    mkdirSync(scrubHome);
    mkdirSync(callerCwd);

    let src = readFileSync(SCRIPT, 'utf8');

    // Point the script at the tmp fixture.
    src = src.replace(/^PROJECT_DIR=.*$/m, `PROJECT_DIR='${fixture}'`);
    // Make the fixture non-enterable immediately before the cd — the exact
    // mid-run disappearance the finding describes. Matches both the bare
    // `cd` (unfixed) and the guarded `cd ... || {` (fixed) forms.
    src = src.replace(
      /^(\s*)cd "\$PROJECT_DIR"/m,
      '$1chmod 000 "$PROJECT_DIR"\n$1cd "$PROJECT_DIR"',
    );
    assert.match(src, /chmod 000 "\$PROJECT_DIR"/, 'could not locate the cd "$PROJECT_DIR" line in run-smoke.sh');
    // Skip everything from the first paid worker launch to the end of the
    // script; emit a marker (relative path — lands wherever the script's cwd
    // is) exactly where the unrestricted workers would have launched.
    const launch = src.indexOf('env "${CLAUDE_ENV_SCRUB[@]}"');
    assert.notEqual(launch, -1, 'could not locate the worker launch site in run-smoke.sh');
    src = src.slice(0, launch) + 'echo launched > smoke-launched.txt\n';

    const scrubbed = join(work, 'run-smoke-scrubbed.sh');
    writeFileSync(scrubbed, src);

    const env = {
      PATH: process.env.PATH,
      HOME: scrubHome,
      FLEETDECK_SMOKE_PORT: '28999',
    };
    const run = spawnSync('bash', [scrubbed], { cwd: callerCwd, env, encoding: 'utf8', timeout: 30000 });

    assert.notEqual(run.status, 0,
      `script must abort when the project fixture cannot be entered (status=${run.status})\nstdout:\n${run.stdout}\nstderr:\n${run.stderr}`);
    assert.match(run.stdout + run.stderr, /could not enter project fixture/,
      'script should report the failed cd into the project fixture');
    assert.equal(existsSync(join(callerCwd, 'smoke-launched.txt')), false,
      'the worker launch site was reached in the caller directory — a failed cd does not stop the smoke');
  } finally {
    // The scrubbed script chmods the fixture to 000; restore perms so the
    // tmp tree can be removed.
    spawnSync('chmod', ['-R', 'u+rwx', work]);
    rmSync(work, { recursive: true, force: true });
  }
});
