import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';

import { startAgentsPoll } from '../src/daemon/agents-poll.ts';
import { startNetworkWatch } from '../src/daemon/network-watch.ts';
import test from './helpers/harness-test.ts';
import { waitUntil } from './helpers/wait.ts';

const NATURAL_EXIT_FIXTURE = fileURLToPath(
  new URL('./helpers/poll-owner-natural-exit.ts', import.meta.url),
);

interface NaturalExitReport {
  type: 'closed';
  mode: 'agents' | 'network';
  callbacksAtStop: number;
  callbacksAfterObservation: number;
  closedAtMs: number;
}

function latch(): { promise: Promise<void>; release: () => void } {
  let release = () => {};
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}

function pause(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 2_000): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} timed out`)), timeoutMs);
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function runNaturalExitFixture(mode: NaturalExitReport['mode']): Promise<NaturalExitReport> {
  const child = Bun.spawn([process.execPath, NATURAL_EXIT_FIXTURE, mode], {
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stdoutPromise = new Response(child.stdout).text();
  const stderrPromise = new Response(child.stderr).text();

  try {
    const exitCode = await within(child.exited, `${mode} poll owner natural exit`);
    const [stdout, stderr] = await Promise.all([stdoutPromise, stderrPromise]);
    assert.equal(exitCode, 0, stderr || stdout);

    const report = stdout
      .trim()
      .split('\n')
      .map((line) => JSON.parse(line) as NaturalExitReport | { type: 'started' })
      .find((message): message is NaturalExitReport => message.type === 'closed');
    assert.ok(report, `${mode} owner exited before its explicit stop callback:\n${stdout}`);
    return report;
  } finally {
    if (child.exitCode === null) child.kill('SIGKILL');
    await within(child.exited, `${mode} poll owner fixture cleanup`).catch(() => undefined);
  }
}

test('agents poll stop joins an in-flight tick and suppresses its late result', async (t) => {
  const releaseRun = latch();
  let runs = 0;
  let ingests = 0;
  let livenessTicks = 0;

  const owner = startAgentsPoll(
    {
      ingestAgentsPoll() {
        ingests++;
      },
      spawnLivenessTick() {
        livenessTicks++;
      },
    },
    {
      argv: ['agents-fixture'],
      firstRunDelayMs: 0,
      idlePollIntervalMs: 2,
      pollIntervalMs: 2,
      async runAgents() {
        runs++;
        await releaseRun.promise;
        return '[]';
      },
    },
  );
  t.after(async () => {
    releaseRun.release();
    await owner.stop();
  });

  await waitUntil(() => runs === 1, {
    timeoutMs: 1_000,
    intervalMs: 2,
    label: 'first agents poll to start',
  });
  const firstStop = owner.stop();
  const secondStop = owner.stop();
  assert.equal(secondStop, firstStop, 'double stop must return the same settlement');

  const settledBeforeRelease = await Promise.race([
    firstStop.then(() => true),
    pause(15).then(() => false),
  ]);
  assert.equal(settledBeforeRelease, false, 'stop must join the current poll command');

  releaseRun.release();
  await firstStop;
  assert.equal(runs, 1);
  assert.equal(ingests, 0, 'a process result arriving during stop must not mutate the core');
  assert.equal(livenessTicks, 0, 'stop must suppress later callbacks in the same tick');

  await pause(15);
  assert.equal(runs, 1, 'the recurring timer must remain stopped after quiescence');
  assert.equal(ingests, 0);
  assert.equal(livenessTicks, 0);
  assert.equal(owner.stop(), firstStop, 'stop remains idempotent after settlement');
});

test('agents poll stop aborts a production-shaped in-flight command immediately', async (t) => {
  let runs = 0;
  let observedAbort = false;
  const owner = startAgentsPoll(
    {
      ingestAgentsPoll() {
        throw new Error('an aborted poll must not ingest');
      },
    },
    {
      argv: ['agents-fixture'],
      firstRunDelayMs: 0,
      pollIntervalMs: 10_000,
      runAgents: async (_argv, signal) => {
        runs++;
        await new Promise<void>((resolve) => {
          if (signal?.aborted) {
            observedAbort = true;
            resolve();
            return;
          }
          signal?.addEventListener(
            'abort',
            () => {
              observedAbort = true;
              resolve();
            },
            { once: true },
          );
        });
        return '[]';
      },
    },
  );
  t.after(() => owner.stop());

  await waitUntil(() => runs === 1, {
    timeoutMs: 1_000,
    intervalMs: 2,
    label: 'abort-aware agents poll to start',
  });
  const stoppedAt = performance.now();
  await owner.stop();

  assert.equal(observedAbort, true);
  assert.ok(performance.now() - stoppedAt < 100, 'producer stop must not wait for exec timeout');
});

test('network watch stop joins its callback and prevents later reads or callbacks', async (t) => {
  const releaseCallback = latch();
  let current = ['192.0.2.20'];
  let previous: readonly string[] = ['192.0.2.10'];
  let reads = 0;
  let callbacks = 0;
  let mutations = 0;
  let errors = 0;
  let observedPrevious: readonly string[] = [];

  const owner = startNetworkWatch({
    intervalMs: 2,
    previousAddresses: () => previous,
    readAddresses() {
      reads++;
      return current;
    },
    async onChange(addresses, before) {
      callbacks++;
      observedPrevious = before;
      await releaseCallback.promise;
      previous = addresses;
      mutations++;
    },
    onError() {
      errors++;
    },
  });
  t.after(async () => {
    releaseCallback.release();
    await owner.stop();
  });

  await waitUntil(() => callbacks === 1, {
    timeoutMs: 1_000,
    intervalMs: 2,
    label: 'first network callback to start',
  });
  const firstStop = owner.stop();
  const secondStop = owner.stop();
  assert.equal(secondStop, firstStop, 'double stop must share one network settlement');

  const settledBeforeRelease = await Promise.race([
    firstStop.then(() => true),
    pause(15).then(() => false),
  ]);
  assert.equal(settledBeforeRelease, false, 'stop must join the current network callback');

  releaseCallback.release();
  await firstStop;
  assert.deepEqual(observedPrevious, ['192.0.2.10']);
  assert.equal(callbacks, 1);
  assert.equal(mutations, 1, 'the joined callback must finish before stop settles');
  assert.equal(errors, 0);

  const readsAtStop = reads;
  current = ['198.51.100.30'];
  await pause(15);
  assert.equal(reads, readsAtStop, 'the interval must not read interfaces after stop');
  assert.equal(callbacks, 1, 'no callback may begin after stop settles');
  assert.equal(mutations, 1, 'no mutation may occur after stop settles');
  assert.equal(owner.stop(), firstStop, 'stop remains idempotent after settlement');
});

test('stopping a network watch before its first interval prevents every callback', async () => {
  let reads = 0;
  let callbacks = 0;
  const owner = startNetworkWatch({
    intervalMs: 10,
    previousAddresses: () => ['192.0.2.10'],
    readAddresses() {
      reads++;
      return ['192.0.2.20'];
    },
    onChange() {
      callbacks++;
    },
  });

  const stopped = owner.stop();
  assert.equal(owner.stop(), stopped);
  await stopped;
  await pause(20);
  assert.equal(reads, 0);
  assert.equal(callbacks, 0);
});

test('poll owner timers keep Bun alive until stop and then allow natural exit', async () => {
  const reports = await Promise.all([
    runNaturalExitFixture('agents'),
    runNaturalExitFixture('network'),
  ]);

  for (const report of reports) {
    assert.ok(report.callbacksAtStop > 0, `${report.mode} cadence never ran before stop`);
    assert.equal(
      report.callbacksAfterObservation,
      report.callbacksAtStop,
      `${report.mode} owner invoked a callback after stop settled`,
    );
    assert.ok(
      report.closedAtMs >= 60,
      `${report.mode} owner did not keep Bun alive until the explicit close timer`,
    );
  }
});
