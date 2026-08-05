// tests/release-gate.test.mjs — BUG-002 regression coverage.
//
// The hook-integrity CI gate used to treat any version-bearing PATHNAME change
// as a version bump: edit package.json's description, leave all four manifests
// at the same version, and a hook-closure change was accepted as release-bound
// even though Claude Code's plugin cache key (the version value) never moved.
// scripts/check-release-gate.mjs is the fix: it resolves the plugin version at
// base and head, requires a semantic increase, and requires every release
// manifest and lock root at head to equal the new value.
//
// These tests build scratch git repos and run the real checker script against
// them — no mocks of the version comparison.

import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const CHECKER = fileURLToPath(new URL('../scripts/check-release-gate.mjs', import.meta.url));

function git(args, cwd) {
  return execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}

const manifest = (version, extra = '') =>
  `{"name":"fleetdeck","version":"${version}"${extra}}\n`;
const pluginJson = (version) => `{"name":"fleetdeck","version":"${version}"}\n`;
const marketplaceJson = (version) =>
  `{"name":"fleetdeck","plugins":[{"name":"fleetdeck","source":"./","version":"${version}"}]}\n`;
const lockJson = (version) =>
  `{"name":"fleetdeck","version":"${version}","lockfileVersion":3,"packages":{"":{"name":"fleetdeck","version":"${version}"}}}\n`;

// A scratch repo at `version` with the full release-manifest set and one
// watched hook file, committed on the default branch.
function makeReleaseRepo(t, version) {
  const root = mkdtempSync(path.join(tmpdir(), 'fleetdeck-gate-'));
  t.after(() => rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  git(['init', '-q'], root);
  git(['config', 'user.email', 'test@fleetdeck.local'], root);
  git(['config', 'user.name', 'Fleet Deck Tests'], root);

  mkdirSync(path.join(root, '.claude-plugin'), { recursive: true });
  mkdirSync(path.join(root, 'board'), { recursive: true });
  mkdirSync(path.join(root, 'scripts'), { recursive: true });
  writeFileSync(path.join(root, 'package.json'), manifest(version));
  writeFileSync(path.join(root, 'package-lock.json'), lockJson(version));
  writeFileSync(path.join(root, '.claude-plugin/plugin.json'), pluginJson(version));
  writeFileSync(path.join(root, '.claude-plugin/marketplace.json'), marketplaceJson(version));
  writeFileSync(path.join(root, 'board/package.json'), manifest(version));
  writeFileSync(path.join(root, 'board/package-lock.json'), lockJson(version));
  writeFileSync(path.join(root, 'scripts/fleet-hook.mjs'), '// hook v1\n');
  git(['add', '.'], root);
  git(['commit', '-q', '-m', `release ${version}`], root);
  return root;
}

function commitAll(root, message) {
  git(['add', '.'], root);
  git(['commit', '-q', '-m', message], root);
}

function bumpAllManifests(root, version) {
  writeFileSync(path.join(root, 'package.json'), manifest(version));
  writeFileSync(path.join(root, 'package-lock.json'), lockJson(version));
  writeFileSync(path.join(root, '.claude-plugin/plugin.json'), pluginJson(version));
  writeFileSync(path.join(root, '.claude-plugin/marketplace.json'), marketplaceJson(version));
  writeFileSync(path.join(root, 'board/package.json'), manifest(version));
  writeFileSync(path.join(root, 'board/package-lock.json'), lockJson(version));
}

function runGate(root, base, head = 'HEAD') {
  try {
    const stdout = execFileSync('node', [CHECKER, base, head], { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
    return { status: 0, output: stdout };
  } catch (err) {
    return { status: err.status, output: `${err.stdout ?? ''}${err.stderr ?? ''}` };
  }
}

test('BUG-002: hook change + manifest edit at an UNCHANGED version is rejected (the fake bump)', (t) => {
  const root = makeReleaseRepo(t, '0.20.0');
  const base = git(['rev-parse', 'HEAD'], root);

  // The audit's exact probe: touch a watched hook script and edit only
  // package.json's description — every version stays 0.20.0.
  writeFileSync(path.join(root, 'scripts/fleet-hook.mjs'), '// hook v2 — behavior changed\n');
  writeFileSync(path.join(root, 'package.json'), manifest('0.20.0', ',"description":"reworded"'));
  commitAll(root, 'change hook behavior, reword description only');

  const res = runGate(root, base);
  assert.equal(res.status, 1, `expected rejection, got exit 0:\n${res.output}`);
  assert.match(res.output, /did not increase \(0\.20\.0 -> 0\.20\.0\)/);
});

test('BUG-002: hook change + real semver bump with all manifests and lock roots in agreement passes', (t) => {
  const root = makeReleaseRepo(t, '0.20.0');
  const base = git(['rev-parse', 'HEAD'], root);

  writeFileSync(path.join(root, 'scripts/fleet-hook.mjs'), '// hook v2 — behavior changed\n');
  bumpAllManifests(root, '0.21.0');
  commitAll(root, 'change hook behavior, bump 0.20.0 -> 0.21.0');

  const res = runGate(root, base);
  assert.equal(res.status, 0, `expected pass, got exit ${res.status}:\n${res.output}`);
  assert.match(res.output, /0\.20\.0 -> 0\.21\.0/);
});

test('BUG-002: hook change + bump where a sibling manifest disagrees is rejected', (t) => {
  const root = makeReleaseRepo(t, '0.20.0');
  const base = git(['rev-parse', 'HEAD'], root);

  writeFileSync(path.join(root, 'scripts/fleet-hook.mjs'), '// hook v2 — behavior changed\n');
  bumpAllManifests(root, '0.21.0');
  // board/package.json stays behind — the four-way contract is broken.
  writeFileSync(path.join(root, 'board/package.json'), manifest('0.20.0'));
  commitAll(root, 'bump but forget board/package.json');

  const res = runGate(root, base);
  assert.equal(res.status, 1, `expected rejection, got exit 0:\n${res.output}`);
  assert.match(res.output, /version drift at HEAD.*board\/package\.json says 0\.20\.0/);
});

test('BUG-002: hook change + bump where a lock root is stale is rejected', (t) => {
  const root = makeReleaseRepo(t, '0.20.0');
  const base = git(['rev-parse', 'HEAD'], root);

  writeFileSync(path.join(root, 'scripts/fleet-hook.mjs'), '// hook v2 — behavior changed\n');
  bumpAllManifests(root, '0.21.0');
  // package-lock.json packages[""] root left at the old version.
  writeFileSync(
    path.join(root, 'package-lock.json'),
    '{"name":"fleetdeck","version":"0.21.0","lockfileVersion":3,"packages":{"":{"name":"fleetdeck","version":"0.20.0"}}}\n'
  );
  commitAll(root, 'bump but leave the lock root stale');

  const res = runGate(root, base);
  assert.equal(res.status, 1, `expected rejection, got exit 0:\n${res.output}`);
  assert.match(res.output, /version drift at HEAD.*packages\[""\] says 0\.20\.0/);
});

test('BUG-002: untouched hook closure passes without any version bump', (t) => {
  const root = makeReleaseRepo(t, '0.20.0');
  const base = git(['rev-parse', 'HEAD'], root);

  // Unwatched change only — no release required.
  writeFileSync(path.join(root, 'README.md'), '# scratch\n');
  commitAll(root, 'docs only');

  const res = runGate(root, base);
  assert.equal(res.status, 0, `expected pass, got exit ${res.status}:\n${res.output}`);
  assert.match(res.output, /closure untouched/);
});

test('BUG-002: a semver DECREASE is not a bump', (t) => {
  const root = makeReleaseRepo(t, '0.21.0');
  const base = git(['rev-parse', 'HEAD'], root);

  writeFileSync(path.join(root, 'scripts/fleet-hook.mjs'), '// hook v2 — behavior changed\n');
  bumpAllManifests(root, '0.20.0');
  commitAll(root, 'change hook behavior, move version backwards');

  const res = runGate(root, base);
  assert.equal(res.status, 1, `expected rejection, got exit 0:\n${res.output}`);
  assert.match(res.output, /did not increase \(0\.21\.0 -> 0\.20\.0\)/);
});
