import { expect, test } from 'bun:test';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import {
  captureStream,
  PackedSmokeOwnership,
  settleCapture,
  verifyBundleFreshness,
} from '../../scripts/effect-migration/p3-packed-install-smoke.ts';

const REPO_ROOT = path.resolve(import.meta.dir, '../..');
const FIXTURE = path.join(import.meta.dir, 'fixtures', 'p3-packed-smoke-signal-host.ts');

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function isPidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return !(
      error instanceof Error &&
      'code' in error &&
      typeof error.code === 'string' &&
      error.code === 'ESRCH'
    );
  }
}

async function waitForFile(file: string, timeoutMs = 5_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!existsSync(file) && Date.now() < deadline) await Bun.sleep(20);
  expect(existsSync(file)).toBe(true);
}

async function waitForPidGone(pid: number, timeoutMs = 3_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (isPidAlive(pid) && Date.now() < deadline) await Bun.sleep(20);
  expect(isPidAlive(pid)).toBe(false);
}

async function boundedExit(
  processHandle: Bun.ReadableSubprocess,
  timeoutMs: number,
): Promise<number> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      processHandle.exited,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => reject(new Error('fixture did not exit in time')), timeoutMs);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

test('packed smoke rebuild is deterministic and never mutates the committed bundle', async () => {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-packed-freshness-test-'));
  const ownership = new PackedSmokeOwnership(scratch);
  const bundle = path.join(REPO_ROOT, 'src', 'daemon', 'fleetd.bundle.mjs');
  const before = await Bun.file(bundle).bytes();
  try {
    await verifyBundleFreshness(ownership);
  } finally {
    await ownership.cleanup();
  }
  const after = await Bun.file(bundle).bytes();
  expect(Buffer.from(after).equals(Buffer.from(before))).toBe(true);
  expect(existsSync(scratch)).toBe(false);
});

test('packed smoke output capture returns a partial snapshot when a pipe never closes', async () => {
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode('partial output'));
    },
  });
  const started = performance.now();
  expect(await settleCapture(captureStream(stream), 25)).toBe('partial output');
  expect(performance.now() - started).toBeLessThan(750);
});

test('packed smoke is exposed by package.json and blocks the bundle CI lane', () => {
  const manifest: unknown = JSON.parse(readFileSync(path.join(REPO_ROOT, 'package.json'), 'utf8'));
  expect(isRecord(manifest)).toBe(true);
  const scripts = isRecord(manifest) && isRecord(manifest['scripts']) ? manifest['scripts'] : {};
  expect(scripts['effect:p3:packed-smoke']).toBe(
    'bun scripts/effect-migration/p3-packed-install-smoke.ts',
  );

  const workflow: unknown = Bun.YAML.parse(
    readFileSync(path.join(REPO_ROOT, '.github', 'workflows', 'ci.yml'), 'utf8'),
  );
  const jobs = isRecord(workflow) && isRecord(workflow['jobs']) ? workflow['jobs'] : {};
  const bundle = isRecord(jobs['bundle']) ? jobs['bundle'] : {};
  const steps = Array.isArray(bundle['steps']) ? bundle['steps'] : [];
  const commands = steps
    .filter(isRecord)
    .map((step) => step['run'])
    .filter((run): run is string => typeof run === 'string');
  expect(commands).toContain('bun run effect:p3:packed-smoke');
});

for (const { label, signal, exitCode, repeat, watchdog } of [
  { label: 'SIGINT', signal: 'SIGINT', exitCode: 130, repeat: false, watchdog: false },
  { label: 'SIGTERM', signal: 'SIGTERM', exitCode: 143, repeat: false, watchdog: false },
  {
    label: 'second SIGTERM',
    signal: 'SIGTERM',
    exitCode: 143,
    repeat: true,
    watchdog: false,
  },
  {
    label: 'watchdog SIGTERM',
    signal: 'SIGTERM',
    exitCode: 143,
    repeat: false,
    watchdog: true,
  },
] as const) {
  test(`${label} cleans a registered pre-marker daemon, its unassigned child, and scratch`, async () => {
    if (process.platform === 'win32') return;
    const testRoot = mkdtempSync(path.join(tmpdir(), 'fleetdeck-packed-signal-test-'));
    const scratch = path.join(testRoot, 'scratch');
    const ready = path.join(testRoot, 'ready.json');
    const privateChildPid = path.join(scratch, 'child.pid');
    const observedChildPid = path.join(testRoot, 'observed-child.pid');
    const forceMarker = path.join(testRoot, 'force-cleanup.marker');
    mkdirSync(scratch);
    const host = Bun.spawn([process.execPath, '--no-env-file', FIXTURE, 'host'], {
      env: {
        ...process.env,
        PACKED_SMOKE_SCRATCH: scratch,
        PACKED_SMOKE_READY: ready,
        PACKED_SMOKE_PRIVATE_CHILD_PID: privateChildPid,
        PACKED_SMOKE_OBSERVED_CHILD_PID: observedChildPid,
        ...(watchdog
          ? {
              PACKED_SMOKE_FORCE_MARKER: forceMarker,
              PACKED_SMOKE_PROCESS_TERM_MS: '1000',
              PACKED_SMOKE_SIGNAL_TIMEOUT_MS: '50',
            }
          : {}),
      },
      stdin: 'ignore',
      stdout: 'pipe',
      stderr: 'pipe',
    });
    const stdout = Bun.readableStreamToText(host.stdout).catch(() => '');
    const stderr = Bun.readableStreamToText(host.stderr).catch(() => '');
    let daemonPid = 0;
    let childPid = 0;
    try {
      await Promise.all([waitForFile(ready), waitForFile(observedChildPid)]);
      const readyBody: unknown = JSON.parse(readFileSync(ready, 'utf8'));
      expect(isRecord(readyBody)).toBe(true);
      daemonPid = Number((readyBody as Record<string, unknown>)['daemonPid']);
      childPid = Number(readFileSync(observedChildPid, 'utf8').trim());
      expect(isPidAlive(daemonPid)).toBe(true);
      expect(isPidAlive(childPid)).toBe(true);
      expect(existsSync(privateChildPid)).toBe(false);

      host.kill(signal);
      if (repeat) {
        await Bun.sleep(20);
        if (host.exitCode === null) host.kill(signal);
      }
      expect(await boundedExit(host, 4_000)).toBe(exitCode);
      await Promise.all([waitForPidGone(daemonPid), waitForPidGone(childPid)]);
      expect(existsSync(scratch)).toBe(false);
      expect(existsSync(forceMarker)).toBe(watchdog);
      expect(await stdout).toBe('');
      expect(await stderr).toBe('');
    } finally {
      if (host.exitCode === null) {
        host.kill('SIGKILL');
        await boundedExit(host, 1_000).catch(() => -1);
      }
      if (daemonPid > 0 && isPidAlive(daemonPid)) process.kill(daemonPid, 'SIGKILL');
      if (childPid > 0 && isPidAlive(childPid)) process.kill(childPid, 'SIGKILL');
      if (daemonPid > 0) await waitForPidGone(daemonPid, 1_000);
      if (childPid > 0) await waitForPidGone(childPid, 1_000);
      rmSync(testRoot, { recursive: true, force: true, maxRetries: 3, retryDelay: 20 });
    }
  });
}
