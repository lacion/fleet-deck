#!/usr/bin/env node
// The release-version contract, machine-checked.
//
// Seven places carry the same version string, and a release is only honest if
// they all agree: package.json, .claude-plugin/plugin.json,
// .claude-plugin/marketplace.json, board/package.json, and the root entry of
// both lockfiles. npm ci does NOT enforce this — it happily installs with
// package.json at 0.21.0 and a stale lock root of 0.20.0 — so nothing in
// install, test, or build will catch the drift. This script is the single
// verifier: publish.yml runs it against the tag before anything irreversible,
// and tests/release-version.test.mjs exercises it directly.
//
// Usage: node scripts/check-release-version.mjs [tag] [root]
//   tag  — the git tag being published (e.g. "v0.21.1"); when given it must
//          also agree with the manifests. publish.yml passes GITHUB_REF_NAME.
//   root — repo root to check (defaults to this repo; tests point it at
//          fixture trees).
//
// Exit 0 when everything agrees, exit 1 listing every mismatch otherwise.

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const [tag, root = repoRoot] = process.argv.slice(2);

const readJson = (rel) => JSON.parse(readFileSync(path.join(root, rel), 'utf8'));

const pkg = readJson('package.json').version;
const lock = readJson('package-lock.json');
const boardLock = readJson('board/package-lock.json');

const versions = [
  ['package.json', pkg],
  ['.claude-plugin/plugin.json', readJson('.claude-plugin/plugin.json').version],
  ['.claude-plugin/marketplace.json', readJson('.claude-plugin/marketplace.json').plugins[0].version],
  ['board/package.json', readJson('board/package.json').version],
  ['package-lock.json (root)', lock.version],
  ['package-lock.json packages[""]', lock.packages?.['']?.version],
  ['board/package-lock.json (root)', boardLock.version],
  ['board/package-lock.json packages[""]', boardLock.packages?.['']?.version],
];
if (tag) versions.unshift([`tag ${tag}`, tag.replace(/^v/, '')]);

for (const [where, v] of versions) console.log(`${where.padEnd(40)} ${v}`);

const expected = versions[0][1];
const mismatches = versions.filter(([, v]) => v !== expected);
if (mismatches.length > 0) {
  console.error('');
  for (const [where, v] of mismatches) {
    console.error(`version drift — ${versions[0][0]} says ${expected} but ${where} says ${v}`);
  }
  process.exit(1);
}
console.log(`✓ all ${versions.length} version strings agree on ${expected}`);
