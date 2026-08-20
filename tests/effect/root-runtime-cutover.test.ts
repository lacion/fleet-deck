import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'bun:test';
import { scaleMs } from '../helpers/wait.ts';

const FIXTURE = fileURLToPath(
  new URL('./fixtures/p4-root-runtime-characterization.ts', import.meta.url),
);

type Mode = 'natural-exit' | 'first-sigterm';

interface Observation {
  readonly event: string;
  readonly mode: Mode;
  readonly code?: number;
  readonly defaultCode?: number;
  readonly exitCode?: number;
  readonly interruptionOnly?: boolean;
  readonly sigintListeners?: number;
  readonly sigtermListeners?: number;
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 2_000): Promise<T> {
  const effectiveTimeoutMs = scaleMs(timeoutMs);
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out after ${effectiveTimeoutMs}ms`)),
          effectiveTimeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function forceRetire(child: Bun.Subprocess, label: string): Promise<void> {
  if (child.exitCode === null) child.kill('SIGKILL');
  try {
    await within(child.exited, `${label} forced reap`);
  } catch (error) {
    child.unref();
    throw error;
  }
}

function observations(stdout: string): Observation[] {
  return stdout
    .split('\n')
    .filter(Boolean)
    .map((line): Observation => JSON.parse(line) as Observation);
}

function eventNames(items: readonly Observation[]): string[] {
  return items.map((item) => item.event);
}

async function firstLine(stream: ReadableStream<Uint8Array>): Promise<string> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let buffered = '';
  try {
    while (!buffered.includes('\n')) {
      const chunk = await within(reader.read(), 'root fixture readiness');
      if (chunk.done) throw new Error(`root fixture stdout closed before readiness: ${buffered}`);
      buffered += decoder.decode(chunk.value, { stream: true });
    }
    return buffered.slice(0, buffered.indexOf('\n'));
  } finally {
    // With a tee'd stream, awaiting cancellation waits for the capture branch
    // to finish as well, which cannot happen before the parent sends SIGTERM.
    void reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}

function spawnFixture(mode: Mode): {
  readonly child: Bun.Subprocess;
  readonly ready: Promise<Observation>;
  readonly stdout: Promise<string>;
  readonly stderr: Promise<string>;
} {
  const child = Bun.spawn([process.execPath, '--no-env-file', FIXTURE, mode], {
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const [readinessStream, captureStream] = child.stdout.tee();
  return {
    child,
    ready: firstLine(readinessStream).then((line): Observation => JSON.parse(line) as Observation),
    stdout: new Response(captureStream).text(),
    stderr: new Response(child.stderr).text(),
  };
}

describe('P4 BunRuntime root-cutover characterization', () => {
  test('a successful root releases owner and runtime keep-alives and exits naturally', async () => {
    const fixture = spawnFixture('natural-exit');
    try {
      assert.deepEqual(await fixture.ready, { event: 'ready', mode: 'natural-exit' });
      assert.equal(await within(fixture.child.exited, 'natural P4 root exit'), 0);
      const [stdout, stderr] = await within(
        Promise.all([fixture.stdout, fixture.stderr]),
        'natural P4 root pipe drain',
      );
      const items = observations(stdout);

      assert.deepEqual(eventNames(items), [
        'ready',
        'cleanup-started',
        'cleanup-complete',
        'teardown',
        'host-idle',
      ]);
      assert.deepEqual(items.at(-2), {
        event: 'teardown',
        mode: 'natural-exit',
        defaultCode: 0,
        exitCode: 0,
        interruptionOnly: false,
        sigintListeners: 0,
        sigtermListeners: 0,
      });
      assert.deepEqual(items.at(-1), { event: 'host-idle', mode: 'natural-exit', code: 0 });
      assert.equal(stderr, '');
    } finally {
      await forceRetire(fixture.child, 'natural P4 root');
      await Promise.allSettled([fixture.stdout, fixture.stderr]);
    }
  });

  test('first SIGTERM interrupts one root, awaits cleanup, and maps the clean exit to zero', async () => {
    const fixture = spawnFixture('first-sigterm');
    try {
      assert.deepEqual(await fixture.ready, { event: 'ready', mode: 'first-sigterm' });
      const state = await Promise.race([
        fixture.child.exited.then((exitCode) => ({ _tag: 'Exited' as const, exitCode })),
        delay(scaleMs(100)).then(() => ({ _tag: 'Alive' as const })),
      ]);
      assert.deepEqual(state, { _tag: 'Alive' }, 'Effect.never keeps the root alive before signal');

      fixture.child.kill('SIGTERM');
      assert.equal(await within(fixture.child.exited, 'SIGTERM P4 root exit'), 0);
      const [stdout, stderr] = await within(
        Promise.all([fixture.stdout, fixture.stderr]),
        'SIGTERM P4 root pipe drain',
      );
      const items = observations(stdout);

      assert.deepEqual(eventNames(items), [
        'ready',
        'cleanup-started',
        'cleanup-complete',
        'teardown',
      ]);
      assert.deepEqual(items.at(-1), {
        event: 'teardown',
        mode: 'first-sigterm',
        defaultCode: 130,
        exitCode: 0,
        interruptionOnly: true,
        sigintListeners: 0,
        sigtermListeners: 0,
      });
      assert.equal(stderr, '');
    } finally {
      await forceRetire(fixture.child, 'SIGTERM P4 root');
      await Promise.allSettled([fixture.stdout, fixture.stderr]);
    }
  });
});
