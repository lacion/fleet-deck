import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { test } from 'bun:test';

import { scaleMs } from '../helpers/wait.ts';

const FIXTURE = fileURLToPath(new URL('./fixtures/p5-background-natural-exit.ts', import.meta.url));

interface Observation {
  readonly event: string;
  readonly code?: number;
  readonly status?: string;
  readonly scheduler?: string;
  readonly ownerState?: string;
  readonly finalized?: string;
  readonly agents?: number;
  readonly lan?: number;
  readonly retentionSweeps?: number;
  readonly retentionPrunes?: number;
  readonly joinTimeoutFired?: boolean;
  readonly postCloseMutations?: number;
  readonly unchanged?: boolean;
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 5_000): Promise<T> {
  const effectiveTimeoutMs = scaleMs(timeoutMs);
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out after ${String(effectiveTimeoutMs)}ms`)),
          effectiveTimeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function forceRetire(child: Bun.Subprocess): Promise<void> {
  if (child.exitCode === null) child.kill('SIGKILL');
  await within(child.exited, 'P5 background fixture forced reap', 2_000);
}

function parseObservations(stdout: string): Observation[] {
  return stdout
    .split('\n')
    .filter(Boolean)
    .map((line): Observation => JSON.parse(line) as Observation);
}

function find(items: readonly Observation[], event: string): Observation {
  const matches = items.filter((item) => item.event === event);
  assert.equal(matches.length, 1, `expected exactly one ${event} observation`);
  const item = matches[0];
  assert.ok(item);
  return item;
}

function index(items: readonly Observation[], event: string): number {
  const position = items.findIndex((item) => item.event === event);
  assert.notEqual(position, -1, `missing ${event} observation`);
  return position;
}

test('real Bun clock closes and joins the aggregate Background owner before natural exit', async () => {
  const source = readFileSync(FIXTURE, 'utf8');
  assert.doesNotMatch(source, /\.unref\s*\(/);
  assert.doesNotMatch(source, /process\.exit\s*\(/);

  const child = Bun.spawn([process.execPath, '--no-env-file', FIXTURE], {
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stdout = new Response(child.stdout).text();
  const stderr = new Response(child.stderr).text();

  try {
    const [exitCode, capturedStdout, capturedStderr] = await within(
      Promise.all([child.exited, stdout, stderr]),
      'P5 background natural exit and pipe drain',
    );
    const items = parseObservations(capturedStdout);
    const names = items.map((item) => item.event);

    assert.equal(exitCode, 0, capturedStderr || capturedStdout);
    assert.equal(capturedStderr, '');

    const clear = index(items, 'boot-clear-forks');
    const reconcileStart = index(items, 'boot-reconcile-start');
    const reconcileComplete = index(items, 'boot-reconcile-complete');
    const firstRetention = index(items, 'retention-sweep-first');
    const broadcast = index(items, 'boot-broadcast-idle');
    const readyIndex = index(items, 'ready');
    assert.ok(clear < reconcileStart);
    assert.ok(clear < firstRetention);
    assert.ok(reconcileStart < reconcileComplete);
    assert.ok(reconcileComplete < broadcast);
    assert.ok(firstRetention < broadcast);
    assert.ok(broadcast < readyIndex);

    const ready = find(items, 'ready');
    assert.equal(ready.status, 'settled');
    assert.ok((ready.retentionSweeps ?? 0) >= 1);

    const scheduled = find(items, 'schedules-observed');
    assert.ok((scheduled.agents ?? 0) >= 1, 'agents liveness cadence did not run');
    assert.ok((scheduled.lan ?? 0) >= 1, 'LAN refresh cadence did not run');
    assert.ok((scheduled.retentionSweeps ?? 0) >= 2, 'periodic retention sweep did not run');
    assert.ok((scheduled.retentionPrunes ?? 0) >= 1, 'periodic retention prune did not run');

    const storeCloseIndex = index(items, 'store-close');
    const finalized = items
      .filter((item) => item.event === 'scheduler-finalized')
      .map((item) => item.scheduler)
      .sort();
    assert.deepEqual(finalized, ['agents', 'lan', 'retention']);
    for (const item of items.filter((candidate) => candidate.event === 'scheduler-finalized')) {
      assert.ok(items.indexOf(item) < storeCloseIndex);
    }

    const backgroundClose = find(items, 'background-closed');
    assert.equal(backgroundClose.ownerState, 'closed');
    assert.equal(backgroundClose.finalized, 'agents,lan,retention');
    assert.ok(index(items, 'background-closed') < storeCloseIndex);

    const storeClose = find(items, 'store-close');
    const stable = find(items, 'post-close-stable');
    assert.equal(storeClose.finalized, 'agents,lan,retention');
    assert.deepEqual(
      {
        agents: stable.agents,
        lan: stable.lan,
        retentionSweeps: stable.retentionSweeps,
        retentionPrunes: stable.retentionPrunes,
      },
      {
        agents: storeClose.agents,
        lan: storeClose.lan,
        retentionSweeps: storeClose.retentionSweeps,
        retentionPrunes: storeClose.retentionPrunes,
      },
    );
    assert.equal(stable.postCloseMutations, 0);
    assert.equal(stable.unchanged, true);

    assert.deepEqual(names.slice(-4), ['root-scope-closed', 'teardown', 'before-exit', 'exit']);
    assert.deepEqual(find(items, 'teardown'), { event: 'teardown', code: 0 });
    assert.deepEqual(find(items, 'before-exit'), {
      event: 'before-exit',
      code: 0,
      joinTimeoutFired: false,
      postCloseMutations: 0,
    });
    assert.deepEqual(find(items, 'exit'), {
      event: 'exit',
      code: 0,
      joinTimeoutFired: false,
      postCloseMutations: 0,
    });
    assert.equal(pidAlive(child.pid), false, 'the successful fixture process was not reaped');
  } finally {
    await forceRetire(child);
    await Promise.allSettled([stdout, stderr]);
  }
});
