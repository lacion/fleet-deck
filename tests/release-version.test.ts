// tests/release-version.test.ts
//
// BUG-003 — the publish workflow used to compare the tag only with
// package.json, so a release could ship with package.json bumped while
// plugin.json, marketplace.json, and board/package.json stayed behind
// (ci.yml's version job never runs on tags). scripts/check-release-version.mjs
// is the shared verifier publish.yml now runs before anything irreversible;
// these tests pin the contract it enforces: the tag and all four manifests must
// agree exactly.

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const script = path.join(repoRoot, 'scripts', 'check-release-version.mjs');

// A minimal repo-shaped fixture: every file the verifier reads, nothing else.
function makeFixture(t: TestContext, { version }: { version?: string } = {}) {
  const root = mkdtempSync(path.join(tmpdir(), 'fleetdeck-release-version-'));
  t.after(() => {
    rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  mkdirSync(path.join(root, '.claude-plugin'), { recursive: true });
  mkdirSync(path.join(root, 'board'), { recursive: true });
  const write = (rel: string, data: unknown) => {
    writeFileSync(path.join(root, rel), JSON.stringify(data, null, 2) + '\n');
  };
  write('package.json', { name: 'fleetdeck', version });
  write('.claude-plugin/plugin.json', { name: 'fleetdeck', version });
  write('.claude-plugin/marketplace.json', { plugins: [{ name: 'fleetdeck', version }] });
  write('board/package.json', { name: 'fleetdeck-board', version });
  return root;
}

function run(...args: string[]) {
  return spawnSync(process.execPath, [script, ...args], { encoding: 'utf8' });
}

test('the real repo satisfies the release-version contract', () => {
  // Also guards against a future bump that forgets a manifest or lock root.
  const out = execFileSync(process.execPath, [script], { encoding: 'utf8' });
  assert.match(out, /agree on /);
});

test('tag and all four manifest version strings agreeing passes', (t) => {
  const root = makeFixture(t, { version: '1.2.3' });
  const result = run('v1.2.3', root);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /all 5 version strings agree on 1\.2\.3/);
});

test('a tag that disagrees with the manifests fails', (t) => {
  const root = makeFixture(t, { version: '1.2.3' });
  const result = run('v1.2.4', root);
  assert.equal(result.status, 1, 'a mismatched tag must fail the verifier');
  assert.match(result.stderr, /version drift/);
});

test('a stale marketplace manifest fails', (t) => {
  const root = makeFixture(t, { version: '0.21.0' });
  const marketplacePath = path.join(root, '.claude-plugin', 'marketplace.json');
  const marketplace = JSON.parse(readFileSync(marketplacePath, 'utf8')) as {
    plugins: { version: string }[];
  };
  const entry = marketplace.plugins[0];
  assert.ok(entry, 'fixture marketplace has a plugin entry');
  entry.version = '0.20.0';
  writeFileSync(marketplacePath, JSON.stringify(marketplace, null, 2) + '\n');
  const result = run('v0.21.0', root);
  assert.equal(result.status, 1, 'a stale marketplace.json must fail the verifier');
  assert.match(result.stderr, /marketplace\.json says 0\.20\.0/);
});

test('a stale board manifest fails', (t) => {
  const root = makeFixture(t, { version: '0.21.0' });
  const boardPath = path.join(root, 'board', 'package.json');
  const board = JSON.parse(readFileSync(boardPath, 'utf8')) as { version: string };
  board.version = '0.20.0';
  writeFileSync(boardPath, JSON.stringify(board, null, 2) + '\n');
  const result = run('v0.21.0', root);
  assert.equal(result.status, 1, 'a stale board/package.json must fail the verifier');
  assert.match(result.stderr, /board\/package\.json says 0\.20\.0/);
});

test('a stale plugin manifest fails', (t) => {
  const root = makeFixture(t, { version: '0.21.0' });
  const pluginPath = path.join(root, '.claude-plugin', 'plugin.json');
  const plugin = JSON.parse(readFileSync(pluginPath, 'utf8')) as { version: string };
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

test('ci.yml uses the same shared verifier for branch version checks', () => {
  const ci = readFileSync(path.join(repoRoot, '.github', 'workflows', 'ci.yml'), 'utf8');
  assert.match(ci, /run: node scripts\/check-release-version\.mjs(?:\s|$)/);
  assert.doesNotMatch(
    ci,
    /require\('\.\/\.claude-plugin\/plugin\.json'\)\.version/,
    'CI must not grow a second hand-maintained manifest/version implementation',
  );
});

test('publish.yml rebuilds and checks every generated npm artifact', () => {
  // A tag may point at a commit that did not pass the branch workflow. The
  // irreversible publish job must independently prove that no committed
  // daemon, CLI, hook, or board artifact is stale.
  const publish = readFileSync(path.join(repoRoot, '.github', 'workflows', 'publish.yml'), 'utf8');
  for (const command of [
    'bun run bundle',
    'bun run bundle:bin',
    'bun run bundle:hooks',
    'bun run build',
  ]) {
    assert.match(publish, new RegExp(command.replaceAll(':', '\\:')));
  }
  for (const artifact of [
    'src/daemon/fleetd.bundle.mjs',
    'bin/fleetdeck.mjs',
    'scripts/fleet-hook.mjs',
    'scripts/fleet-sessionstart.mjs',
    'scripts/fleet-watch.mjs',
    'src/daemon/board-dist',
  ]) {
    assert.match(publish, new RegExp(artifact.replaceAll('.', '\\.')));
  }
});
