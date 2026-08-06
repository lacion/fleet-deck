// tests/accept-phase3-perm-proof.test.mjs
//
// BUG-011 regression: demo/run-accept-phase3.sh's permission-relay check used
// to pass whenever the model's JSON output contained FLEET_PERMISSION_OK —
// but the prompt itself names that marker, so the grep proved nothing about
// the approved Bash command actually executing. The verdict now lives in
// demo/perm-proof-check.sh (sourced by the acceptance script) and requires:
// a clean claude exit status, a successful board answer POST, and the proof
// file on disk with exactly the bytes the prompt specified.

import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const HELPER = path.join(REPO, 'demo', 'perm-proof-check.sh');
const MARKER = 'FLEET_PERMISSION_OK';

function check({ rc = 0, proof = MARKER, json = '', approved = 'yes' } = {}) {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-perm-proof-'));
  try {
    const proofFile = path.join(dir, 'fleet-perm-proof.txt');
    const permJson = path.join(dir, 'p3-perm.json');
    if (proof != null) writeFileSync(proofFile, proof);
    writeFileSync(permJson, json);
    try {
      const out = execFileSync('bash', ['-c',
        `. "$1" && perm_proof_check "$2" "$3" "$4" "$5"`,
        'bash', HELPER, String(rc), proofFile, permJson, approved,
      ], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
      return { pass: true, detail: out.trim() };
    } catch (e) {
      return { pass: false, detail: String(e.stdout ?? '').trim() };
    }
  } finally {
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
}

test('BUG-011: marker in model prose alone does not pass — proof file must exist on disk', () => {
  // The exact false-positive the old grep allowed: claude exits 0 and its
  // JSON output contains the marker (the prompt asked it to report it), but
  // the approved command never created the file.
  const r = check({ proof: null, json: `{"result":"done: ${MARKER}"}` });
  assert.equal(r.pass, false, 'prose marker without the proof file must FAIL');
  assert.match(r.detail, /proof file missing/);
  assert.match(r.detail, /model prose only/, 'failure should note the prose marker is not proof');
});

test('BUG-011: proof file bytes must match exactly — wrong or padded content fails', () => {
  for (const proof of ['WRONG', `${MARKER}\n`, ` ${MARKER}`]) {
    const r = check({ proof });
    assert.equal(r.pass, false, `proof bytes ${JSON.stringify(proof)} must FAIL`);
    assert.match(r.detail, /bytes differ/);
  }
});

test('BUG-011: non-zero claude exit status fails even with the proof file present', () => {
  const r = check({ rc: 1 });
  assert.equal(r.pass, false);
  assert.match(r.detail, /rc=1/);
});

test('BUG-011: failed board answer POST fails even with the proof file present', () => {
  const r = check({ approved: '' });
  assert.equal(r.pass, false);
  assert.match(r.detail, /answer POST did not succeed/);
});

test('BUG-011: clean exit + successful answer POST + exact proof bytes passes', () => {
  const r = check({ json: '{"result":"anything"}' });
  assert.equal(r.pass, true, r.detail);
});
