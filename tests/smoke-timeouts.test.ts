// tests/smoke-timeouts.test.ts — BUG-100 regression: every control-plane
// request in demo/run-smoke.sh must be bounded. The authenticated /mail POST
// and the final /state GET previously used bare `curl -fsS`, so a daemon that
// accepts the TCP connection but stalls the response wedged the live smoke
// run forever (the 300 s `timeout` only wraps each Claude worker process).

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '..');
const SMOKE = path.join(REPO_ROOT, 'demo', 'run-smoke.sh');

test('demo/run-smoke.sh bounds every control-plane curl with connect and total timeouts', () => {
  const src = readFileSync(SMOKE, 'utf8');
  const lines = src.split('\n');

  // Reassemble each curl invocation (they use backslash continuations).
  const invocations: string[] = [];
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    if (line === undefined || !/\bcurl\b/.test(line)) continue;
    let cmd = line;
    while (cmd.endsWith('\\') && i + 1 < lines.length) {
      i += 1;
      const next = lines[i];
      if (next === undefined) break;
      cmd += '\n' + next;
    }
    invocations.push(cmd);
  }
  assert.ok(invocations.length >= 4, 'expected to find the smoke curl invocations');

  for (const cmd of invocations) {
    assert.match(
      cmd,
      /(^|\s)(--max-time|-m)(\s|=)/,
      `curl invocation must carry a total timeout:\n${cmd}`,
    );
  }

  // The two calls the audit flagged specifically: the /mail POST and the
  // final /state GET must have BOTH a connect and a total timeout.
  const mail = invocations.find((c) => c.includes('/mail'));
  const state = invocations.find((c) => c.includes('/state'));
  assert.ok(mail, 'found the /mail POST');
  assert.ok(state, 'found the final /state GET');
  const bounded: [string, string][] = [
    ['/mail POST', mail],
    ['/state GET', state],
  ];
  for (const [label, cmd] of bounded) {
    assert.match(cmd, /--connect-timeout\s+\d+/, `${label} must bound connect time`);
    assert.match(cmd, /--max-time\s+\d+/, `${label} must bound total time`);
  }
});

test('demo/run-smoke.sh folds the final /state GET into a bounded tombstone poll', () => {
  const src = readFileSync(SMOKE, 'utf8');
  const stateIdx = src.indexOf(':$FLEETDECK_PORT/state');
  assert.ok(stateIdx > 0, 'final /state capture exists');
  // A retry loop must wrap the capture so async SessionEnd tombstoning is
  // awaited instead of racing, and the loop must be bounded (seq N).
  const before = src.slice(Math.max(0, stateIdx - 600), stateIdx);
  assert.match(
    before,
    /for\s+\w+\s+in\s+\$\(seq\s+1\s+\d+\)/,
    'bounded retry loop wraps the /state capture',
  );
  assert.match(before, /tombstone/i, 'the poll waits for tombstones');
});
