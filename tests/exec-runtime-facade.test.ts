import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from './helpers/harness-test.ts';

const FIXTURE = fileURLToPath(new URL('./fixtures/exec-runtime-facade.ts', import.meta.url));

test('execFileP publishes one bindable plain facade for normal and bounded policies', () => {
  const result = spawnSync(process.execPath, [FIXTURE], {
    encoding: 'utf8',
    timeout: 10_000,
  });
  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.equal(result.signal, null);
  assert.equal(result.stdout, 'exec runtime facade ok\n');
  assert.equal(result.stderr, '');
});
