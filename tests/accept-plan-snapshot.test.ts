// tests/accept-plan-snapshot.test.ts
//
// BUG-012 regression: demo/run-accept-plan.sh must not destroy pre-existing
// project content. The gate's setup used to copy the seed over util.js,
// delete test.js, and overwrite .claude/settings.json BEFORE any backup, and
// its cleanup restored the seed rather than the original bytes — silently
// erasing uncommitted work and local Claude configuration.
//
// The fix snapshots every affected path (including its previous nonexistence)
// before mutating and restores exact bytes in cleanup. These tests drive the
// gate's real snapshot/mutate/restore code: the script emits a standalone
// harness (`--emit-snapshot-harness`) built from its own function bodies, so
// the harness can never drift from the live gate. The "legacy" assertions
// replay the pre-fix restore to prove the test would have caught the defect.

import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs, { cpSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const GATE_SCRIPT = path.join(REPO_ROOT, 'demo', 'run-accept-plan.sh');

const SEED_UTIL = 'exports.seed = true;\n';
const LOCAL_UTIL = 'exports.mine = "local-work";\n';
const LOCAL_TEST = '// my local tests\n';
const LOCAL_SETTINGS = '{"my":"local settings"}\n';

function fixture(t: TestContext): { demoDir: string; projectDir: string; harness: string } {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-accept-plan-'));
  t.after(() => {
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const demoDir = path.join(dir, 'demo');
  const projectDir = path.join(demoDir, 'project');
  mkdirSync(path.join(projectDir, '.seed'), { recursive: true });
  mkdirSync(path.join(dir, 'scripts'), { recursive: true });
  cpSync(GATE_SCRIPT, path.join(demoDir, 'run-accept-plan.sh'));
  writeFileSync(path.join(projectDir, '.seed', 'util.js'), SEED_UTIL);
  execFileSync('bash', [
    path.join(demoDir, 'run-accept-plan.sh'),
    '--emit-snapshot-harness',
    path.join(demoDir, 'harness.sh'),
  ]);
  return { demoDir, projectDir, harness: path.join(demoDir, 'harness.sh') };
}

function runHarness(harness: string, mode: string): void {
  execFileSync('bash', [harness, mode], { stdio: 'pipe' });
}

function readMaybe(file: string): string | null {
  return fs.existsSync(file) ? fs.readFileSync(file, 'utf8') : null;
}

function seedLocalContent(projectDir: string): void {
  writeFileSync(path.join(projectDir, 'util.js'), LOCAL_UTIL);
  writeFileSync(path.join(projectDir, 'test.js'), LOCAL_TEST);
  mkdirSync(path.join(projectDir, '.claude'), { recursive: true });
  writeFileSync(path.join(projectDir, '.claude', 'settings.json'), LOCAL_SETTINGS);
}

test('gate snapshot+restore preserves pre-existing project bytes and files', (t: TestContext) => {
  const { projectDir, harness } = fixture(t);
  seedLocalContent(projectDir);

  runHarness(harness, 'restore');

  assert.equal(
    readMaybe(path.join(projectDir, 'util.js')),
    LOCAL_UTIL,
    'util.js must be restored to its pre-run bytes, not the seed',
  );
  assert.equal(
    readMaybe(path.join(projectDir, 'test.js')),
    LOCAL_TEST,
    'a pre-existing test.js must survive the gate',
  );
  assert.equal(
    readMaybe(path.join(projectDir, '.claude', 'settings.json')),
    LOCAL_SETTINGS,
    'project .claude/settings.json must be restored, not left clobbered',
  );
  assert.deepEqual(
    readdirSync(projectDir).filter((name) => name.startsWith('.pre-accept-')),
    [],
    'no snapshot backup dir may linger in the project',
  );
});

test('gate restore returns previously-absent files to nonexistence', (t: TestContext) => {
  const { projectDir, harness } = fixture(t);
  // pre-run state: only the seed util.js content, no test.js, no settings
  writeFileSync(path.join(projectDir, 'util.js'), LOCAL_UTIL);

  runHarness(harness, 'restore');

  assert.equal(readMaybe(path.join(projectDir, 'util.js')), LOCAL_UTIL);
  assert.equal(
    readMaybe(path.join(projectDir, 'test.js')),
    null,
    'a test.js the executor created must be removed when none existed pre-run',
  );
  assert.equal(
    readMaybe(path.join(projectDir, '.claude', 'settings.json')),
    null,
    'settings.json must be removed when none existed pre-run',
  );
});

test('pre-fix restore behavior destroys local content (defect proof)', (t: TestContext) => {
  const { projectDir, harness } = fixture(t);
  seedLocalContent(projectDir);

  // "legacy" replays the original cleanup: restore the seed, delete test.js,
  // leave the regenerated settings in place. If the gate ever reverts to
  // this, the first test above fails exactly like this one does here.
  runHarness(harness, 'legacy');

  assert.equal(
    readMaybe(path.join(projectDir, 'util.js')),
    SEED_UTIL,
    'the pre-fix cleanup overwrote local util.js work with the seed',
  );
  assert.equal(
    readMaybe(path.join(projectDir, 'test.js')),
    null,
    'the pre-fix cleanup deleted a pre-existing test.js',
  );
  assert.notEqual(
    readMaybe(path.join(projectDir, '.claude', 'settings.json')),
    LOCAL_SETTINGS,
    'the pre-fix cleanup left clobbered settings behind',
  );
});
