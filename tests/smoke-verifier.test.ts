// tests/smoke-verifier.test.ts
//
// BUG-191 — the demo/run-smoke.sh conflict check used to flatten every
// conflict rel_path and test the unanchored substrings /util\.js/ and
// /test\.js/. Decoy rows like `not-util.js.bak` and `contest.js` satisfied
// both regexes, so the smoke printed PASS even though neither required file
// had a conflict. The check now requires exact normalized-path membership
// (util.js and test.js) AND both smoke session IDs in each conflict's
// participants. This test runs the extracted verifier body from the script
// against synthetic final-state.json files, proving the fail-before behavior.

import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, rmSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '..');
const SMOKE_SCRIPT = path.join(REPO_ROOT, 'demo', 'run-smoke.sh');
const SID_A = 'session-a-uuid';
const SID_B = 'session-b-uuid';

interface Conflict {
  rel_path: string;
  sessions: string[];
  severity: string;
}

interface Session {
  session_id: string;
  callsign: string;
  col: string;
  endedAt: number;
}

interface SmokeState {
  sessions: Session[];
  ticker: { msg: string }[];
  conflicts: Conflict[];
}

interface ExecFailure {
  status?: number;
  stdout?: string;
  stderr?: string;
}

function extractVerifierBody(): string {
  const source = readFileSync(SMOKE_SCRIPT, 'utf8');
  const start = source.indexOf('node --input-type=module -e "');
  assert.notEqual(start, -1, 'run-smoke.sh embeds a node verifier block');
  const afterStart = start + 'node --input-type=module -e "'.length;
  const end = source.indexOf('\n"', afterStart);
  assert.notEqual(end, -1, 'verifier block has a terminating quote line');
  return source.slice(afterStart, end);
}

// Rebuild the verifier exactly the way bash does: the body is a double-quoted
// string, so bash expands $DEMO_LOGS/$SA/$SB/$RC_A/$RC_B inside it.
function materializeVerifier(dir: string, state: SmokeState): string {
  mkdirSync(dir, { recursive: true });
  writeFileSync(path.join(dir, 'final-state.json'), JSON.stringify(state));
  writeFileSync(
    path.join(dir, 'worker-a.json'),
    JSON.stringify({ is_error: false, subtype: 'success' }),
  );
  writeFileSync(
    path.join(dir, 'worker-b.json'),
    JSON.stringify({ is_error: false, subtype: 'success' }),
  );
  const body = extractVerifierBody()
    .replaceAll('$DEMO_LOGS', dir)
    .replaceAll('$SA', SID_A)
    .replaceAll('$SB', SID_B)
    .replaceAll('$RC_A', '0')
    .replaceAll('$RC_B', '0');
  const scriptPath = path.join(dir, 'verifier.mjs');
  writeFileSync(scriptPath, body);
  return scriptPath;
}

function baseSession(id: string, callsign: string): Session {
  return { session_id: id, callsign, col: 'offline', endedAt: Date.now() };
}

function baseState(conflicts: Conflict[]): SmokeState {
  return {
    sessions: [baseSession(SID_A, 'alpha-aa'), baseSession(SID_B, 'bravo-bb')],
    ticker: [
      { msg: 'alpha-aa got fleet mail at the turn boundary' },
      { msg: 'bravo-bb got fleet mail at the turn boundary' },
    ],
    conflicts,
  };
}

function runVerifier(t: TestContext, state: SmokeState): { code: number; out: string } {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-smokeverify-'));
  t.after(() => {
    rmSync(dir, { recursive: true, force: true });
  });
  const scriptPath = materializeVerifier(dir, state);
  let out: string;
  let code = 0;
  try {
    out = execFileSync(process.execPath, [scriptPath], { encoding: 'utf8' });
  } catch (err) {
    const e = err as ExecFailure;
    code = e.status ?? 1;
    out = (e.stdout ?? '') + (e.stderr ?? '');
  }
  return { code, out };
}

test('conflict check passes when both required files collide with both workers', (t) => {
  const state = baseState([
    { rel_path: 'util.js', sessions: [SID_A, SID_B], severity: 'same-worktree' },
    { rel_path: 'test.js', sessions: [SID_A, SID_B], severity: 'same-worktree' },
  ]);
  const { code, out } = runVerifier(t, state);
  assert.equal(code, 0, 'verifier must pass: ' + out);
  assert.match(out, /PASS: conflict recorded on util\.js AND test\.js/);
});

test('BUG-191: decoy paths (not-util.js.bak, contest.js) must NOT satisfy the conflict check', (t) => {
  const state = baseState([
    { rel_path: 'not-util.js.bak', sessions: [SID_A, SID_B], severity: 'same-worktree' },
    { rel_path: 'contest.js', sessions: [SID_A, SID_B], severity: 'same-worktree' },
  ]);
  const { code, out } = runVerifier(t, state);
  assert.notEqual(code, 0, 'verifier must fail: ' + out);
  assert.match(out, /FAIL: conflict recorded on util\.js AND test\.js/);
});

test('BUG-191: correct paths with only one worker participating must fail', (t) => {
  const state = baseState([
    { rel_path: 'util.js', sessions: [SID_A], severity: 'same-worktree' },
    { rel_path: 'test.js', sessions: [SID_B], severity: 'same-worktree' },
  ]);
  const { code, out } = runVerifier(t, state);
  assert.notEqual(code, 0, 'verifier must fail: ' + out);
  assert.match(out, /FAIL: conflict recorded on util\.js AND test\.js/);
});

test('conflict check accepts repo-relative rel_paths like demo/project/util.js', (t) => {
  const state = baseState([
    { rel_path: 'demo/project/util.js', sessions: [SID_A, SID_B], severity: 'same-worktree' },
    { rel_path: 'demo/project/test.js', sessions: [SID_A, SID_B], severity: 'same-worktree' },
  ]);
  const { code, out } = runVerifier(t, state);
  assert.equal(code, 0, 'verifier must pass: ' + out);
  assert.match(out, /PASS: conflict recorded on util\.js AND test\.js/);
});
