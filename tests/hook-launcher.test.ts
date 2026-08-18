import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, mkdtempSync, mkdirSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const LAUNCHER = path.join(ROOT, 'scripts', 'hook-launcher.sh');

interface Fixture {
  root: string;
  bin: string;
  bundle: string;
}

interface Result {
  status: number | null;
  stdout: string;
  stderr: string;
}

function fixture(t: TestContext): Fixture {
  const root = mkdtempSync(path.join(tmpdir(), 'fleetdeck-hook-launcher-test-'));
  t.after(() => rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const bin = path.join(root, 'bin');
  mkdirSync(bin);
  const bun = path.join(bin, 'bun');
  writeFileSync(
    bun,
    `#!/bin/sh
printf '%s' "\${FAKE_STDOUT-}"
printf '%s' "\${FAKE_STDERR-}" >&2
exit "\${FAKE_STATUS-0}"
`,
    { mode: 0o700 },
  );
  chmodSync(bun, 0o700);
  const bundle = path.join(root, 'bundle with spaces.mjs');
  writeFileSync(bundle, '// fixture bundle\n');
  return { root, bin, bundle };
}

function run(
  fixture: Fixture,
  mode: 'decision' | 'sessionstart' | 'watch',
  overrides: NodeJS.ProcessEnv = {},
  bundle = fixture.bundle,
): Result {
  const result = spawnSync('/bin/sh', [LAUNCHER, mode, bundle, 'arg with spaces'], {
    encoding: 'utf8',
    input: '{"hook_event_name":"Stop"}',
    env: {
      ...process.env,
      PATH: `${fixture.bin}:/usr/bin:/bin`,
      TMPDIR: fixture.root,
      ...overrides,
    },
  });
  assert.equal(result.error, undefined);
  return { status: result.status, stdout: result.stdout, stderr: result.stderr };
}

function assertCapturesRemoved(fixture: Fixture): void {
  assert.deepEqual(
    readdirSync(fixture.root).filter((name) => name.startsWith('fleetdeck-hook.')),
    [],
    'private stdout/stderr captures are removed after every launch',
  );
}

test('decision launcher commits stdout only after a clean Bun exit', (t) => {
  const fx = fixture(t);
  assert.deepEqual(
    run(fx, 'decision', {
      FAKE_STDOUT: '{"hookSpecificOutput":{"hookEventName":"Stop"}}',
      FAKE_STDERR: 'internal diagnostic',
    }),
    {
      status: 0,
      stdout: '{"hookSpecificOutput":{"hookEventName":"Stop"}}',
      stderr: '',
    },
  );
  assert.deepEqual(
    run(fx, 'decision', {
      FAKE_STDOUT: '{"partial":',
      FAKE_STDERR: 'stack trace',
      FAKE_STATUS: '1',
    }),
    { status: 0, stdout: '{}', stderr: '' },
  );
  assertCapturesRemoved(fx);
});

test('SessionStart launcher hides every failed or diagnostic write', (t) => {
  const fx = fixture(t);
  assert.deepEqual(
    run(fx, 'sessionstart', {
      FAKE_STDOUT: '[FLEETDECK] roster',
      FAKE_STDERR: 'hidden diagnostic',
    }),
    { status: 0, stdout: '[FLEETDECK] roster', stderr: '' },
  );
  assert.deepEqual(
    run(fx, 'sessionstart', {
      FAKE_STDOUT: '[FLEETDECK] partial roster',
      FAKE_STDERR: 'stack trace',
      FAKE_STATUS: '1',
    }),
    { status: 0, stdout: '', stderr: '' },
  );
  assertCapturesRemoved(fx);
});

test('watch launcher relays only the intentional mail wake signal', (t) => {
  const fx = fixture(t);
  assert.deepEqual(
    run(fx, 'watch', {
      FAKE_STDOUT: 'never relay watcher stdout',
      FAKE_STDERR: 'board mail',
      FAKE_STATUS: '2',
    }),
    { status: 2, stdout: '', stderr: 'board mail' },
  );
  assert.deepEqual(
    run(fx, 'watch', {
      FAKE_STDOUT: 'partial output',
      FAKE_STDERR: 'failure noise',
      FAKE_STATUS: '3',
    }),
    { status: 0, stdout: '', stderr: '' },
  );
  assertCapturesRemoved(fx);
});

test('missing Bun or bundle silently disables the optional integration', (t) => {
  const fx = fixture(t);
  for (const mode of ['decision', 'sessionstart', 'watch'] as const) {
    const expected = mode === 'decision' ? '{}' : '';
    assert.deepEqual(run(fx, mode, { PATH: path.join(fx.root, 'missing-bin') }), {
      status: 0,
      stdout: expected,
      stderr: '',
    });
    assert.deepEqual(run(fx, mode, {}, path.join(fx.root, 'missing-bundle.mjs')), {
      status: 0,
      stdout: expected,
      stderr: '',
    });
  }
  assertCapturesRemoved(fx);
});
