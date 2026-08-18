import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import {
  chmodSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  utimesSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  compatibilityVerdictFile,
  detectRunningClaudeVersion,
  establishClaudeCompatibility,
  hasActiveClaudeCompatibility,
  parseStableClaudeVersion,
  pruneClaudeCompatibilityVerdicts,
  sameDarwinProcessGeneration,
  supportsClaudeVersion,
  type ExecutableFingerprint,
  type ClaudeCompatibilityPolicy,
} from '../scripts/claude-compat.ts';

function scratch(t: TestContext): string {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-claude-compat-'));
  t.after(() => {
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return dir;
}

function env(version: string, pid = '424242'): NodeJS.ProcessEnv {
  return { CLAUDE_PID: pid, FLEETDECK_TEST_CLAUDE_VERSION: version };
}

test('stable Claude version parser accepts exact CLI output and rejects ambiguous versions', () => {
  assert.equal(parseStableClaudeVersion('2.1.234')?.raw, '2.1.234');
  assert.equal(parseStableClaudeVersion('2.1.234 (Claude Code)\n')?.raw, '2.1.234');
  for (const invalid of [
    '',
    'v2.1.234',
    '02.1.234',
    '2.1',
    '2.1.234-beta.1',
    '2.1.234 beta',
    '2.1.234\n2.1.233',
    'Claude Code 2.1.234',
  ]) {
    assert.equal(parseStableClaudeVersion(invalid), null, invalid);
  }
});

test('compatibility policy enforces a minimum without rejecting newer stable releases', () => {
  assert.equal(supportsClaudeVersion('2.1.205'), false);
  assert.equal(supportsClaudeVersion('2.1.206'), true);
  assert.equal(supportsClaudeVersion('2.1.220'), true);
  assert.equal(supportsClaudeVersion('2.1.234'), true);
  assert.equal(supportsClaudeVersion('2.1.235'), true);
  assert.equal(supportsClaudeVersion('999.0.0'), true);
  assert.equal(supportsClaudeVersion('2.1.234-beta.1'), false);
});

test('test seam is strict and never falls through to a local executable', async () => {
  assert.equal(await detectRunningClaudeVersion({ env: env('2.1.220') }), '2.1.220');
  assert.equal(await detectRunningClaudeVersion({ env: env('') }), null);
  assert.equal(await detectRunningClaudeVersion({ env: env('latest') }), null);
});

test('SessionStart verdict is atomic, owner-only, and keyed to runtime plus policy', async (t) => {
  const home = scratch(t);
  const hookEnv = env('2.1.220');
  assert.equal(await establishClaudeCompatibility(home, { env: hookEnv }), true);
  const file = compatibilityVerdictFile(home, hookEnv);
  assert.equal(statSync(file).mode & 0o777, 0o600);
  assert.equal(
    readdirSync(home).some((name) => name.endsWith('.tmp')),
    false,
  );
  assert.equal(hasActiveClaudeCompatibility(home, { env: hookEnv }), true);
  assert.match(readFileSync(file, 'utf8'), /"active":true/);

  assert.equal(
    hasActiveClaudeCompatibility(home, { env: hookEnv, fleetdeckVersion: '999.0.0' }),
    false,
  );
  const shifted: ClaudeCompatibilityPolicy = {
    schema: 2,
    claudeCode: { min: '2.1.221' },
  };
  assert.equal(hasActiveClaudeCompatibility(home, { env: hookEnv, policy: shifted }), false);
});

test('too-old, corrupt, oversized, and non-owner-only verdicts are inactive', async (t) => {
  const home = scratch(t);
  const hookEnv = env('2.1.205');
  assert.equal(await establishClaudeCompatibility(home, { env: hookEnv }), false);
  const file = compatibilityVerdictFile(home, hookEnv);
  assert.equal(hasActiveClaudeCompatibility(home, { env: hookEnv }), false);

  writeFileSync(file, '{not-json', { mode: 0o600 });
  assert.equal(hasActiveClaudeCompatibility(home, { env: hookEnv }), false);
  writeFileSync(file, 'x'.repeat(5000), { mode: 0o600 });
  assert.equal(hasActiveClaudeCompatibility(home, { env: hookEnv }), false);

  await establishClaudeCompatibility(home, { env: env('2.1.220') });
  chmodSync(file, 0o644);
  assert.equal(hasActiveClaudeCompatibility(home, { env: env('2.1.220') }), false);
});

test('same PID cannot reuse a verdict across a version generation; SessionStart refreshes it', async (t) => {
  const home = scratch(t);
  const first = env('2.1.206');
  const upgraded = env('2.1.234');
  assert.equal(await establishClaudeCompatibility(home, { env: first }), true);
  assert.equal(hasActiveClaudeCompatibility(home, { env: upgraded }), false);
  assert.equal(await establishClaudeCompatibility(home, { env: upgraded }), true);
  assert.equal(hasActiveClaudeCompatibility(home, { env: upgraded }), true);

  const downgraded = env('2.1.205');
  assert.equal(await establishClaudeCompatibility(home, { env: downgraded }), false);
  assert.equal(hasActiveClaudeCompatibility(home, { env: downgraded }), false);
});

test('a new SessionStart invalidates stale active state before asynchronous detection', async (t) => {
  const home = scratch(t);
  const prior = env('2.1.220');
  assert.equal(await establishClaudeCompatibility(home, { env: prior }), true);
  assert.equal(hasActiveClaudeCompatibility(home, { env: prior }), true);

  const unknownEnv: NodeJS.ProcessEnv = { CLAUDE_PID: prior['CLAUDE_PID'] };
  const pending = establishClaudeCompatibility(home, { env: unknownEnv, platform: 'win32' });
  assert.equal(
    hasActiveClaudeCompatibility(home, { env: prior }),
    false,
    'old active file is gone synchronously, before detector settlement',
  );
  assert.equal(await pending, false);
});

test('Darwin PID reuse requires both the start identity and current executable', () => {
  const executable: ExecutableFingerprint = {
    path: '/Users/dev/.local/share/claude/versions/2.1.234',
    dev: '1',
    ino: '42',
    size: 1234,
    mtimeMs: 5678,
  };
  assert.equal(
    sameDarwinProcessGeneration(
      'Mon Aug 18 12:00:00 2026',
      executable,
      'Mon Aug 18 12:00:00 2026',
      executable,
    ),
    true,
  );
  assert.equal(
    sameDarwinProcessGeneration(
      'Mon Aug 18 12:00:00 2026',
      executable,
      'Mon Aug 18 13:00:00 2026',
      executable,
    ),
    false,
    'same PID and same executable cannot survive a different process start token',
  );
  assert.equal(
    sameDarwinProcessGeneration(
      'Mon Aug 18 12:00:00 2026',
      executable,
      'Mon Aug 18 12:00:00 2026',
      { ...executable, path: '/usr/bin/other-process', ino: '99' },
    ),
    false,
    'same PID and same-second start token cannot authorize a different current executable',
  );
});

test('verdict lease expires and stale marker cleanup is bounded', async (t) => {
  const home = scratch(t);
  const now = 2_000_000_000_000;
  const hookEnv = env('2.1.220');
  assert.equal(await establishClaudeCompatibility(home, { env: hookEnv, now }), true);
  assert.equal(hasActiveClaudeCompatibility(home, { env: hookEnv, now }), true);
  assert.equal(
    hasActiveClaudeCompatibility(home, { env: hookEnv, now: now + 31 * 24 * 3600_000 }),
    false,
  );

  const file = compatibilityVerdictFile(home, hookEnv);
  const old = new Date(now - 32 * 24 * 3600_000);
  utimesSync(file, old, old);
  assert.equal(pruneClaudeCompatibilityVerdicts(home, now), 1);
});

test('invalid or obsolete policy can never activate a verdict', async (t) => {
  const home = scratch(t);
  const hookEnv = env('2.1.220');
  const invalid: ClaudeCompatibilityPolicy = {
    schema: 2,
    claudeCode: { min: 'not-a-version' },
  };
  assert.equal(await establishClaudeCompatibility(home, { env: hookEnv, policy: invalid }), false);
  assert.equal(hasActiveClaudeCompatibility(home, { env: hookEnv, policy: invalid }), false);

  const obsolete = {
    schema: 1,
    claudeCode: { min: '2.1.234', max: '2.1.206' },
  } as unknown as ClaudeCompatibilityPolicy;
  assert.equal(await establishClaudeCompatibility(home, { env: hookEnv, policy: obsolete }), false);
  assert.equal(hasActiveClaudeCompatibility(home, { env: hookEnv, policy: obsolete }), false);
});
