// tests/accept-plan-mark.test.ts
//
// BUG-015 — demo/run-accept-plan.sh gate 8 must not POST status:"executed"
// unless gate 7 actually proved execution (implementation + function check
// passed AND no executor permission card was observed). Before the fix, gate 8
// marked the plan executed whenever a plan ID existed — even when the executor
// failed to spawn, timed out, or hit a permission card — and could corrupt a
// real production plan's state if pointed at the wrong daemon.

import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import {
  readFileSync,
  writeFileSync,
  mkdtempSync,
  mkdirSync,
  rmSync,
  chmodSync,
  existsSync,
} from 'node:fs';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const SCRIPT = path.join(REPO_ROOT, 'demo', 'run-accept-plan.sh');

function gate8Block(): string {
  const src = readFileSync(SCRIPT, 'utf8');
  const start = src.indexOf(
    '# --------------------------------------------------------------- gate 8',
  );
  const end = src.indexOf(
    '# --------------------------------------------------------------- gate 9',
  );
  assert.notEqual(start, -1, 'gate 8 marker exists in demo/run-accept-plan.sh');
  assert.notEqual(end, -1, 'gate 9 marker exists in demo/run-accept-plan.sh');
  return src.slice(start, end);
}

// Runs the extracted gate 8 block under bash with stubbed curl/node on PATH.
// The curl stub appends any plan-mark request to $MARK_LOG instead of touching
// the network; the node stub emits a fixed executed-mark body. Returns
// { output, marks }.
interface Gate8Vars {
  PLAN_ID?: string;
  EXECUTED?: string;
  NO_PERMISSION_CARD?: string;
}

function runGate8(t: TestContext, vars: Gate8Vars): { output: string; marks: string } {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-accept-plan-mark-'));
  t.after(() => {
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const bin = path.join(home, 'bin');
  mkdirSync(bin);
  writeFileSync(
    path.join(bin, 'curl'),
    '#!/bin/bash\n' +
      'for a in "$@"; do\n' +
      '  case "$a" in\n' +
      '    */api/plans/*/mark) echo "$a" >> "$MARK_LOG";;\n' +
      '  esac\n' +
      'done\n' +
      'printf 200\n',
  );
  writeFileSync(
    path.join(bin, 'node'),
    '#!/bin/bash\n' + 'printf \'%s\' \'{"status":"executed","via":"accept-script"}\'\n',
  );
  chmodSync(path.join(bin, 'curl'), 0o755);
  chmodSync(path.join(bin, 'node'), 0o755);

  const markLog = path.join(home, 'mark.log');
  const harness = path.join(home, 'gate8.sh');
  writeFileSync(
    harness,
    '#!/bin/bash\n' +
      'set -u\n' +
      'PASS=0\n' +
      'FAIL=0\n' +
      'SCRATCH_HOME="' +
      home +
      '"\n' +
      'BASE="http://127.0.0.1:1"\n' +
      'MARK_LOG="' +
      markLog +
      '"\n' +
      'export MARK_LOG\n' +
      'ok() { echo "PASS: $1"; PASS=$((PASS + 1)); }\n' +
      'bad() { echo "FAIL: $1${2:+ -- $2}"; FAIL=$((FAIL + 1)); }\n' +
      gate8Block() +
      'echo "gates: PASS=$PASS FAIL=$FAIL"\n',
  );

  const env = {
    PATH: bin + ':' + String(process.env['PATH']),
    PLAN_ID: vars.PLAN_ID ?? '',
    EXECUTED: vars.EXECUTED ?? '',
    NO_PERMISSION_CARD: vars.NO_PERMISSION_CARD ?? '',
  };
  const output = execFileSync('bash', [harness], { env, encoding: 'utf8' });
  const marks = existsSync(markLog) ? readFileSync(markLog, 'utf8').trim() : '';
  return { output, marks };
}

test('gate 8 does not mark the plan executed when execution was never proven (BUG-015)', (t: TestContext) => {
  const noExecution = runGate8(t, { PLAN_ID: '42', EXECUTED: '', NO_PERMISSION_CARD: 'yes' });
  assert.equal(
    noExecution.marks,
    '',
    'no mark request is issued when the implementation check failed',
  );
  assert.match(
    noExecution.output,
    /FAIL: plan marked executed/,
    'the gate reports FAIL instead of a false PASS',
  );

  const permissionCard = runGate8(t, { PLAN_ID: '42', EXECUTED: 'yes', NO_PERMISSION_CARD: '' });
  assert.equal(
    permissionCard.marks,
    '',
    'no mark request is issued when a permission card was observed',
  );
  assert.match(
    permissionCard.output,
    /FAIL: plan marked executed/,
    'the gate reports FAIL instead of a false PASS',
  );
});

test('gate 8 marks the plan executed only after gate 7 proved execution', (t: TestContext) => {
  const proven = runGate8(t, { PLAN_ID: '42', EXECUTED: 'yes', NO_PERMISSION_CARD: 'yes' });
  assert.equal(
    proven.marks,
    'http://127.0.0.1:1/api/plans/42/mark',
    'the mark request targets the captured plan',
  );
  assert.match(proven.output, /PASS: plan marked executed/, 'the gate reports PASS');
});
