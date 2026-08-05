// tests/demo-perm-proof-cleanup.test.mjs
//
// BUG-091 regression: demo/run-accept-phase3.sh used to delete the permission
// proof only before the run, leaving the generated fleet-perm-proof.txt in the
// checkout after exit (and silently discarding any pre-existing local file).
// The EXIT handler must now remove the generated proof after validating its
// exact content, and snapshot/restore any pre-existing file.
//
// The live demo needs real Claude usage, so this test extracts the
// cleanup_perm_proof function body from the script and exercises it in bash
// against a scratch PROJECT_DIR — the same source the demo runs.

import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs, { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const SCRIPT = path.join(REPO_ROOT, 'demo', 'run-accept-phase3.sh');

// Extract cleanup_perm_proof() { ... } from the demo script (balanced braces,
// no nested functions in that body) so the test runs the real shipped code.
function extractCleanup() {
  const src = fs.readFileSync(SCRIPT, 'utf8');
  const start = src.indexOf('cleanup_perm_proof() {');
  assert.notEqual(start, -1, 'run-accept-phase3.sh defines cleanup_perm_proof');
  let depth = 0;
  for (let i = src.indexOf('{', start); i < src.length; i++) {
    if (src[i] === '{') depth++;
    else if (src[i] === '}') {
      depth--;
      if (depth === 0) return src.slice(start, i + 1);
    }
  }
  throw new Error('unbalanced braces in cleanup_perm_proof');
}

// Run the extracted function with a prepared proof/snapshot layout and report
// the resulting file states back as JSON.
function runCleanup(t, { proof, snapshot } = {}) {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-perm-proof-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const project = path.join(dir, 'project');
  const logs = path.join(dir, 'demo-logs');
  fs.mkdirSync(project);
  fs.mkdirSync(logs);
  const proofPath = path.join(project, 'fleet-perm-proof.txt');
  const prePath = path.join(logs, 'p3-perm-proof.pre-existing');
  if (proof !== undefined) fs.writeFileSync(proofPath, proof);
  if (snapshot !== undefined) fs.writeFileSync(prePath, snapshot);

  const bash = `
set -u
PERM_PROOF="$1"
PERM_PROOF_PRE="$2"
${extractCleanup()}
cleanup_perm_proof
node -e '
const fs = require("fs");
const out = {};
for (const [k, p] of Object.entries({ proof: process.argv[1], pre: process.argv[2] })) {
  out[k] = fs.existsSync(p) ? fs.readFileSync(p, "utf8") : null;
}
console.log(JSON.stringify(out));
' "$PERM_PROOF" "$PERM_PROOF_PRE"
`;
  const out = execFileSync('bash', ['-c', bash, 'bash', proofPath, prePath], { encoding: 'utf8' });
  return JSON.parse(out.trim().split('\n').pop());
}

test('exit cleanup removes a generated proof with the expected content', (t) => {
  const state = runCleanup(t, { proof: 'FLEET_PERMISSION_OK' });
  assert.equal(state.proof, null, 'the generated proof is removed at exit');
});

test('exit cleanup removes the generated proof even with a trailing newline', (t) => {
  const state = runCleanup(t, { proof: 'FLEET_PERMISSION_OK\n' });
  assert.equal(state.proof, null, 'echo-style proof content still matches');
});

test('exit cleanup leaves an unexpected file untouched', (t) => {
  const state = runCleanup(t, { proof: 'user work in progress' });
  assert.equal(state.proof, 'user work in progress', 'non-generated content is never deleted');
});

test('exit cleanup restores a snapshotted pre-existing file over the proof', (t) => {
  const state = runCleanup(t, { proof: 'FLEET_PERMISSION_OK', snapshot: 'local edits' });
  assert.equal(state.proof, 'local edits', 'the pre-existing file is restored verbatim');
  assert.equal(state.pre, null, 'the snapshot itself is cleaned up');
});

test('exit cleanup is a no-op when nothing was generated', (t) => {
  const state = runCleanup(t, {});
  assert.equal(state.proof, null);
  assert.equal(state.pre, null);
});

test('the demo script registers cleanup_perm_proof in the EXIT trap', () => {
  const src = fs.readFileSync(SCRIPT, 'utf8');
  assert.match(src, /cleanup\(\) \{[\s\S]*?cleanup_perm_proof[\s\S]*?\}/, 'EXIT cleanup calls cleanup_perm_proof');
  assert.match(src, /trap cleanup EXIT/, 'the EXIT trap runs the combined cleanup');
});
