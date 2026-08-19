// tests/effect-ci-policy.test.ts
//
// P0.8 CI policy tripwire: Bun 1.3.14 remains the exact blocking floor while
// latest stable gets a separate, advisory compatibility lane until P14.

import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from './helpers/harness-test.ts';

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const WORKFLOW_SOURCE = fs.readFileSync(path.join(REPO, '.github', 'workflows', 'ci.yml'), 'utf8');

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function requireRecord(value: unknown, message: string): Record<string, unknown> {
  assert.ok(isRecord(value), message);
  return value;
}

const workflow = requireRecord(Bun.YAML.parse(WORKFLOW_SOURCE), 'ci.yml must parse as YAML');
const jobs = requireRecord(workflow['jobs'], 'ci.yml must have a top-level jobs mapping');

function requireJob(name: string): Record<string, unknown> {
  return requireRecord(jobs[name], `ci.yml must define the ${name} job`);
}

function stepsOf(job: Record<string, unknown>, name: string): Array<Record<string, unknown>> {
  const steps = job['steps'];
  assert.ok(Array.isArray(steps), `${name} must define steps`);
  return steps.map((step, index) => requireRecord(step, `${name} step ${index} must be a mapping`));
}

function bunVersions(job: Record<string, unknown>, name: string): unknown[] {
  return stepsOf(job, name)
    .filter(
      (step) => typeof step['uses'] === 'string' && step['uses'].startsWith('oven-sh/setup-bun@'),
    )
    .map(
      (step) => requireRecord(step['with'], `${name} setup-bun must define with`)['bun-version'],
    );
}

const FLOOR_JOBS = ['toolchain', 'test', 'bundle', 'board', 'test-macos'];
const BLOCKING_FLOOR_JOBS = ['toolchain', 'test', 'bundle', 'board'];

test('effect CI policy: floor jobs stay on exact Bun 1.3.14', () => {
  for (const name of FLOOR_JOBS) {
    assert.deepEqual(
      bunVersions(requireJob(name), name),
      ['1.3.14'],
      `${name} must use one setup-bun step with exact Bun 1.3.14`,
    );
  }

  for (const name of BLOCKING_FLOOR_JOBS) {
    assert.notEqual(requireJob(name)['continue-on-error'], true, `${name} must remain blocking`);
  }
});

test('effect CI policy: latest stable runs in one separate advisory canary', () => {
  const canary = requireJob('bun-latest-canary');
  assert.match(String(canary['name']), /advisory until P14/);
  assert.equal(canary['continue-on-error'], true);
  assert.deepEqual(bunVersions(canary, 'bun-latest-canary'), ['latest']);

  const latestJobs = Object.entries(jobs)
    .filter(([name, job]) =>
      bunVersions(requireRecord(job, `${name} must be a mapping`), name).includes('latest'),
    )
    .map(([name]) => name);
  assert.deepEqual(latestJobs, ['bun-latest-canary']);
});

test('effect CI policy: latest canary exercises the required compatibility gates', () => {
  const steps = stepsOf(requireJob('bun-latest-canary'), 'bun-latest-canary');
  const commands = steps.flatMap((step) => (typeof step['run'] === 'string' ? [step['run']] : []));

  assert.ok(commands.includes('sudo apt-get update && sudo apt-get install -y tmux'));
  assert.ok(commands.includes('bun install --frozen-lockfile --ignore-scripts'));
  assert.ok(commands.includes('cd board && bun install --frozen-lockfile --ignore-scripts'));

  const combinedCommands = commands.join('\n');
  assert.match(combinedCommands, /(?:^|\n)bun --version(?:\n|$)/);
  assert.match(combinedCommands, /Bun\.revision/);
  assert.match(combinedCommands, /(?:^|\n)uname -a(?:\n|$)/);
  assert.match(combinedCommands, /bun:sqlite adapter contract/);
  assert.match(combinedCommands, /openDatabase\(":memory:"\)/);
  assert.match(combinedCommands, /miss === undefined/);
  assert.ok(commands.includes('bun run typecheck'));

  const sourceSuite = steps.find((step) => step['run'] === 'bun run test');
  assert.ok(sourceSuite, 'latest canary must run the full source suite');
  const sourceSuiteEnv = requireRecord(sourceSuite['env'], 'source suite must define WAIT_SCALE');
  const waitScale = sourceSuiteEnv['FLEETDECK_TEST_WAIT_SCALE'];
  assert.ok(
    typeof waitScale === 'number' && waitScale >= 2,
    'latest canary must give the source suite WAIT_SCALE headroom',
  );
});
