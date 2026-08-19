import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  analyzeEffectCohort,
  BUN_FLOOR,
  EFFECT_COHORT,
  MAX_RESOLVED_PRODUCTION_PACKAGES,
  parseBunLock,
  runBoundedCommand,
} from '../scripts/effect-migration/check-effect-cohort.ts';
import test from './helpers/harness-test.ts';

type JsonObject = Record<string, unknown>;

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const SCRIPT = path.join(REPO, 'scripts', 'effect-migration', 'check-effect-cohort.ts');
const manifest = JSON.parse(fs.readFileSync(path.join(REPO, 'package.json'), 'utf8')) as JsonObject;
const lock = parseBunLock(fs.readFileSync(path.join(REPO, 'bun.lock'), 'utf8'));

function object(value: unknown): JsonObject {
  assert.ok(value && typeof value === 'object' && !Array.isArray(value));
  return value as JsonObject;
}

function array(value: unknown): unknown[] {
  assert.ok(Array.isArray(value));
  return value;
}

function clone<T>(value: T): T {
  return structuredClone(value);
}

function violationCodes(report: ReturnType<typeof analyzeEffectCohort>): string[] {
  return report.violations.map((entry) => entry.code);
}

test('Effect cohort policy accepts the exact canonical dependency closure', () => {
  const report = analyzeEffectCohort(manifest, lock);
  assert.equal(report.ok, true);
  assert.equal(report.direct.count, 2);
  assert.equal(report.direct.typesBun, BUN_FLOOR);
  assert.equal(report.direct.lockAligned, true);
  assert.equal(report.closure.count, 20);
  assert.ok(report.closure.count <= MAX_RESOLVED_PRODUCTION_PACKAGES);
  assert.deepEqual(
    report.cohort.map(({ name, version }) => [name, version]),
    [
      ['@effect/platform-bun', EFFECT_COHORT],
      ['@effect/platform-node-shared', EFFECT_COHORT],
      ['effect', EFFECT_COHORT],
    ],
  );
  assert.equal(report.wsMovement.unchanged, true);
  assert.equal(report.wsMovement.deduped, true);
});

test('Effect cohort policy rejects mutable production specs and lock drift', () => {
  for (const spec of [`^${EFFECT_COHORT}`, 'latest', 'github:Effect-TS/effect#main']) {
    const candidate = clone(manifest);
    object(candidate['dependencies'])['effect'] = spec;
    const report = analyzeEffectCohort(candidate, lock);
    assert.ok(violationCodes(report).includes('mutable-direct-spec'), spec);
    assert.ok(violationCodes(report).includes('direct-cohort-spec'), spec);
    assert.ok(violationCodes(report).includes('lock-workspace-drift'), spec);
  }
});

test('Effect cohort policy scans every locked v4 package, including outside the closure', () => {
  const candidate = clone(lock);
  object(candidate['packages'])['@effect/fixture'] = [
    '@effect/fixture@4.0.0-rc.109',
    '',
    {},
    'sha512-fixture',
  ];
  const report = analyzeEffectCohort(manifest, candidate);
  assert.ok(violationCodes(report).includes('resolved-cohort-mismatch'));
});

test('Effect cohort policy rejects Bun type-floor drift', () => {
  const candidate = clone(manifest);
  object(candidate['devDependencies'])['@types/bun'] = `^${BUN_FLOOR}`;
  const report = analyzeEffectCohort(candidate, lock);
  assert.ok(violationCodes(report).includes('bun-types-floor'));
  assert.ok(violationCodes(report).includes('lock-bun-types-drift'));
});

test('Effect cohort policy enforces direct and resolved production ceilings', () => {
  const directManifest = clone(manifest);
  const directLock = clone(lock);
  object(directManifest['dependencies'])['fixture-direct'] = '1.0.0';
  object(object(object(directLock['workspaces'])[''])['dependencies'])['fixture-direct'] = '1.0.0';
  object(directLock['packages'])['fixture-direct'] = [
    'fixture-direct@1.0.0',
    '',
    {},
    'sha512-fixture',
  ];
  assert.ok(
    violationCodes(analyzeEffectCohort(directManifest, directLock)).includes(
      'direct-package-ceiling',
    ),
  );

  const closureLock = clone(lock);
  const packages = object(closureLock['packages']);
  const effectEntry = array(packages['effect']);
  const effectMetadata = object(effectEntry[2]);
  const dependencies = object(effectMetadata['dependencies']);
  for (let index = 0; index < 5; index++) {
    const name = `effect-closure-fixture-${index}`;
    dependencies[name] = '1.0.0';
    packages[name] = [`${name}@1.0.0`, '', {}, 'sha512-fixture'];
  }
  const closureReport = analyzeEffectCohort(manifest, closureLock);
  assert.equal(closureReport.closure.count, 25);
  assert.ok(violationCodes(closureReport).includes('production-closure-ceiling'));
});

test('Effect cohort command timeout terminates the scratch child', async () => {
  const result = await runBoundedCommand(
    [process.execPath, '-e', 'setInterval(() => {}, 1000)'],
    REPO,
    { timeoutMs: 20 },
  );
  assert.equal(result.timedOut, true);
  assert.equal(result.argv[0], 'bun');
  assert.ok(result.elapsedMs < 2_500, `bounded command took ${result.elapsedMs}ms`);
});

test('Effect cohort CLI writes normalized comparable JSON without mutating inputs', async (t) => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'fleetdeck-cohort-test-'));
  t.after(() => fs.rmSync(scratch, { recursive: true, force: true }));
  const packageBefore = await Bun.file(path.join(REPO, 'package.json')).bytes();
  const lockBefore = await Bun.file(path.join(REPO, 'bun.lock')).bytes();
  const hashes: string[] = [];
  for (let run = 0; run < 2; run++) {
    const output = path.join(scratch, `report-${run}.json`);
    const child = Bun.spawn([process.execPath, SCRIPT, '--out', output], {
      cwd: REPO,
      stdin: 'ignore',
      stdout: 'ignore',
      stderr: 'pipe',
    });
    const [exitCode, stderr] = await Promise.all([child.exited, new Response(child.stderr).text()]);
    assert.equal(exitCode, 0, stderr);
    const source = await Bun.file(output).text();
    assert.equal(source.includes(scratch), false);
    const report = object(JSON.parse(source));
    assert.equal(report['measurement'], null);
    assert.equal(
      object(report['hardGates'])['all'],
      false,
      'structural-only runs must not claim that measured supply-chain gates passed',
    );
    hashes.push(String(object(report['comparison'])['stableFactsSha256']));
  }
  assert.equal(hashes[0], hashes[1]);
  assert.deepEqual(await Bun.file(path.join(REPO, 'package.json')).bytes(), packageBefore);
  assert.deepEqual(await Bun.file(path.join(REPO, 'bun.lock')).bytes(), lockBefore);
});
