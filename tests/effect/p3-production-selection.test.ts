import { expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import path from 'node:path';

const REPO_ROOT = path.resolve(import.meta.dir, '../..');
const DAEMON_ROOT = path.join(REPO_ROOT, 'src', 'daemon');

function source(pathname: string): string {
  return readFileSync(path.join(REPO_ROOT, pathname), 'utf8');
}

function typescriptFiles(root: string): string[] {
  const files: string[] = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const absolute = path.join(root, entry.name);
    if (entry.isDirectory()) files.push(...typescriptFiles(absolute));
    else if (entry.isFile() && entry.name.endsWith('.ts')) files.push(absolute);
  }
  return files;
}

test('P3 production composition selects the Bun driver and excludes both comparison drivers', () => {
  const liveLayer = source('src/daemon/app/live-layer.ts');
  const processRunnerLive = source('src/daemon/platform/bun/process-runner-live.ts');
  const bundle = source('src/daemon/fleetd.bundle.mjs');

  expect(liveLayer).toContain("from '../platform/bun/process-runner-live.ts'");
  expect(liveLayer).not.toContain('platform/node');
  expect(processRunnerLive).toContain("from './process-driver.ts'");
  expect(processRunnerLive).toContain('Effect.sync(makeBunProcessDriver)');

  const productionReferences = typescriptFiles(DAEMON_ROOT)
    .filter(
      (file) =>
        file !== path.join(DAEMON_ROOT, 'platform', 'node', 'process-driver-reference.ts') &&
        file !== path.join(DAEMON_ROOT, 'platform', 'bun', 'child-process-spawner.ts'),
    )
    .filter((file) => {
      const contents = readFileSync(file, 'utf8');
      return (
        contents.includes('process-driver-reference') || contents.includes('child-process-spawner')
      );
    });
  expect(productionReferences).toEqual([]);

  // The generated daemon is part of the production contract. Pin both the
  // selected implementation and the absence of the two differential oracles,
  // whose source remains available only to tests and the upstream patch.
  expect(bundle).toContain('var BunProcessDriver = class');
  expect(bundle).toContain('sync2(makeBunProcessDriver)');
  expect(bundle).not.toContain('makeNodeProcessDriverReference');
  expect(bundle).not.toContain('startNodeExecution');
  expect(bundle).not.toContain('BunChildProcessSpawner');
});

test('P3 files execution has one bounded facade and every git subprocess uses it', () => {
  const files = source('src/daemon/files.ts');
  const bundle = source('src/daemon/fleetd.bundle.mjs');

  expect(files).toContain("import { execFileP } from './exec.ts'");
  expect(files).not.toContain('node:child_process');
  expect(files).not.toContain('Bun.spawn');
  expect(files.match(/\bexecFileP\s*\(/g)).toHaveLength(1);
  expect(files.match(/\brunBounded\s*\(/g)).toHaveLength(5);
  expect(files).toMatch(
    /export function runBounded\([\s\S]*?\): Promise<RunResult> \{\s*return execFileP\(cmd, args,/,
  );

  expect(bundle).toMatch(/function runBounded\([\s\S]*?\) \{\s*return execFileP\(cmd, args2,/);
});
