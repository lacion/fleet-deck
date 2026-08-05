// tests/release-version.test.mjs
//
// BUG-003 — the publish workflow used to compare the tag only with
// package.json, so a release could ship with package.json bumped while
// plugin.json, marketplace.json, board/package.json, and the lockfile roots
// stayed behind (npm ci accepts a stale lock root, and ci.yml's version job
// never runs on tags). scripts/check-release-version.mjs is the shared
// verifier publish.yml now runs before anything irreversible; these tests pin
// the contract it enforces: tag, four manifests, and both lockfile roots must
// all agree exactly.

import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const script = path.join(repoRoot, 'scripts', 'check-release-version.mjs');

// A minimal repo-shaped fixture: every file the verifier reads, nothing else.
function makeFixture(t, { version, lockVersion = version, boardLockVersion = version } = {}) {
  const root = mkdtempSync(path.join(tmpdir(), 'fleetdeck-release-version-'));
  t.after(() => rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  mkdirSync(path.join(root, '.claude-plugin'), { recursive: true });
  mkdirSync(path.join(root, 'board'), { recursive: true });
  const write = (rel, data) => writeFileSync(path.join(root, rel), JSON.stringify(data, null, 2) + '\n');
  write('package.json', { name: 'fleetdeck', version });
  write('package-lock.json', { name: 'fleetdeck', version: lockVersion, lockfileVersion: 3, packages: { '': { name: 'fleetdeck', version: lockVersion } } });
  write('.claude-plugin/plugin.json', { name: 'fleetdeck', version });
  write('.claude-plugin/marketplace.json', { plugins: [{ name: 'fleetdeck', version }] });
  write('board/package.json', { name: 'fleetdeck-board', version });
  write('board/package-lock.json', { name: 'fleetdeck-board', version: boardLockVersion, lockfileVersion: 3, packages: { '': { name: 'fleetdeck-board', version: boardLockVersion } } });
  return root;
}

function run(...args) {
  return spawnSync(process.execPath, [script, ...args], { encoding: 'utf8' });
}

test('the real repo satisfies the release-version contract', () => {
  // Also guards against a future bump that forgets a manifest or lock root.
  const out = execFileSync(process.execPath, [script], { encoding: 'utf8' });
  assert.match(out, /agree on /);
});

test('tag and all seven version strings agreeing passes', (t) => {
  const root = makeFixture(t, { version: '1.2.3' });
  const result = run('v1.2.3', root);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /all 9 version strings agree on 1\.2\.3/);
});

test('a tag that disagrees with the manifests fails', (t) => {
  const root = makeFixture(t, { version: '1.2.3' });
  const result = run('v1.2.4', root);
  assert.equal(result.status, 1, 'a mismatched tag must fail the verifier');
  assert.match(result.stderr, /version drift/);
});

test('the BUG-003 scenario fails: manifests bumped, root lockfile root left stale', (t) => {
  const root = makeFixture(t, { version: '0.21.0', lockVersion: '0.20.0' });
  const result = run('v0.21.0', root);
  assert.equal(result.status, 1, 'npm ci accepts this stale lock root — the verifier must not');
  assert.match(result.stderr, /package-lock\.json \(root\) says 0\.20\.0/);
});

test('a stale board lockfile root fails', (t) => {
  const root = makeFixture(t, { version: '0.21.0', boardLockVersion: '0.20.0' });
  const result = run('v0.21.0', root);
  assert.equal(result.status, 1, 'a stale board lock root must fail the verifier');
  assert.match(result.stderr, /board\/package-lock\.json/);
});

test('a stale plugin manifest fails', (t) => {
  const root = makeFixture(t, { version: '0.21.0' });
  const pluginPath = path.join(root, '.claude-plugin', 'plugin.json');
  const plugin = JSON.parse(readFileSync(pluginPath, 'utf8'));
  plugin.version = '0.20.0';
  writeFileSync(pluginPath, JSON.stringify(plugin, null, 2) + '\n');
  const result = run('v0.21.0', root);
  assert.equal(result.status, 1, 'a stale plugin.json must fail the verifier');
  assert.match(result.stderr, /plugin\.json says 0\.20\.0/);
});

test('publish.yml runs the shared verifier against the tag', () => {
  // The contract is only worth anything if the irreversible workflow enforces
  // it — pin the wiring, not just the script.
  const publish = readFileSync(path.join(repoRoot, '.github', 'workflows', 'publish.yml'), 'utf8');
  assert.match(publish, /check-release-version\.mjs "\$GITHUB_REF_NAME"/);
});
