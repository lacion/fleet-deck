#!/usr/bin/env node
// The release-version contract, machine-checked.
//
// Four human-authored manifests carry the same version string, and a release is
// only honest if they all agree: package.json, .claude-plugin/plugin.json,
// .claude-plugin/marketplace.json, and board/package.json. bun.lock has no root
// "version" field (and is not JSON), so it can't drift the way an npm lock root
// could — `bun install --frozen-lockfile` keeps the lockfiles consistent with
// the manifests instead. This script is the single verifier of those four
// version strings: ci.yml runs it on branches, publish.yml runs it against the
// tag before anything irreversible, and tests/release-version.test.ts exercises
// it directly.
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
import { VERSION_MANIFEST_PATHS, versionFromManifest } from './release-contract.mjs';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const [tag, root = repoRoot] = process.argv.slice(2);

const readJson = (rel) => JSON.parse(readFileSync(path.join(root, rel), 'utf8'));

const versions = VERSION_MANIFEST_PATHS.map((file) => [
  file,
  versionFromManifest(file, readJson(file)),
]);
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
