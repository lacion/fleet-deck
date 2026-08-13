// tests/accept-phase3-portability.test.ts
//
// BUG-093: demo/run-accept-phase3.sh is a documented live acceptance gate and
// the README lists macOS as a supported platform, but the script invoked GNU
// `timeout` (absent from stock macOS) to supervise the `claude -p` runs and
// used the Linux-only `fuser -k` as its port reset. The script must ship a
// portable fallback (gtimeout or Node process supervision) and must not rely
// on `fuser -k` without a portable alternative.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const scriptPath = path.join(repoRoot, 'demo', 'run-accept-phase3.sh');
const script = readFileSync(scriptPath, 'utf8');

test('accept-phase3: no bare GNU `timeout` invocation — supervision goes through the portable helper', () => {
  for (const [i, line] of script.split('\n').entries()) {
    assert.ok(
      !/(^|[^\w.])timeout\s+\d/.test(line),
      `line ${i + 1} invokes bare \`timeout <secs>\`, which does not exist on stock macOS: ${line.trim()}`,
    );
  }
  assert.match(script, /run_with_timeout\(\)/, 'script should define a portable timeout wrapper');
  assert.match(script, /command -v gtimeout/, "wrapper should accept Homebrew coreutils' gtimeout");
  assert.match(script, /node -e/, 'wrapper should fall back to Node process supervision');
});

test('accept-phase3: port reset prefers portable lsof over Linux-only `fuser -k`', () => {
  assert.match(
    script,
    /command -v lsof/,
    'reset should probe for lsof (ships on both Linux and macOS)',
  );
  const fuserLine = script.split('\n').findIndex((l) => /^\s*fuser -k/.test(l));
  const lsofLine = script.split('\n').findIndex((l) => /^\s*lsof /.test(l));
  assert.ok(lsofLine !== -1, 'reset should kill by port via lsof');
  assert.ok(
    fuserLine === -1 || lsofLine < fuserLine,
    'fuser -k may remain only as a fallback after lsof',
  );
});

test('accept-phase3: script still parses (bash -n)', () => {
  const res = spawnSync('bash', ['-n', scriptPath], { encoding: 'utf8' });
  assert.equal(res.status, 0, res.stderr);
});
