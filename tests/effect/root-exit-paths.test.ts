import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'bun:test';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import { shutdownTriggerFromExit } from '../../src/daemon/app/live-layer.ts';
import { scaleMs } from '../helpers/wait.ts';

type Mode = 'startup-refusal' | 'bind-address-in-use' | 'defect' | 'signal' | 'finite';

interface RootExitObservation {
  readonly mode: Mode;
  readonly code: number;
  readonly firstSignal: 'SIGINT' | 'SIGTERM' | null;
  readonly listeners: {
    readonly sigint: number;
    readonly sigterm: number;
  };
}

interface FixtureResult {
  readonly code: number;
  readonly stdout: string;
  readonly stderr: string;
  readonly observation: RootExitObservation;
}

const FIXTURE = fileURLToPath(new URL('./fixtures/p4-root-exit-path.ts', import.meta.url));
const OBSERVATION_PREFIX = 'ROOT_EXIT_OBSERVATION ';

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 5_000): Promise<T> {
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

async function firstLine(stream: ReadableStream<Uint8Array>): Promise<string> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let buffered = '';
  try {
    while (!buffered.includes('\n')) {
      const chunk = await within(reader.read(), 'root exit fixture readiness');
      if (chunk.done) throw new Error(`root exit fixture closed before readiness: ${buffered}`);
      buffered += decoder.decode(chunk.value, { stream: true });
    }
    return buffered.slice(0, buffered.indexOf('\n'));
  } finally {
    void reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}

function parseObservation(stdout: string): RootExitObservation {
  const line = stdout.split('\n').find((candidate) => candidate.startsWith(OBSERVATION_PREFIX));
  assert.ok(line, `missing root exit observation in stdout:\n${stdout}`);
  return JSON.parse(line.slice(OBSERVATION_PREFIX.length)) as RootExitObservation;
}

async function runFixture(mode: Mode): Promise<FixtureResult> {
  const child = Bun.spawn([process.execPath, '--no-env-file', FIXTURE, mode], {
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  let stdout: Promise<string>;
  let ready: Promise<string> | null = null;
  if (mode === 'signal') {
    const [readinessStream, captureStream] = child.stdout.tee();
    ready = firstLine(readinessStream);
    stdout = new Response(captureStream).text();
  } else {
    stdout = new Response(child.stdout).text();
  }
  const stderr = new Response(child.stderr).text();

  try {
    if (ready) {
      assert.equal(await ready, 'ROOT_EXIT_READY');
      child.kill('SIGTERM');
    }
    const code = await within(child.exited, `${mode} root exit`);
    const [output, errorOutput] = await within(
      Promise.all([stdout, stderr]),
      `${mode} root pipe drain`,
    );
    return {
      code,
      stdout: output,
      stderr: errorOutput,
      observation: parseObservation(output),
    };
  } finally {
    if (child.exitCode === null) child.kill('SIGKILL');
    await Promise.allSettled([child.exited, stdout, stderr, ...(ready ? [ready] : [])]);
  }
}

function assertHostReleased(result: FixtureResult): void {
  assert.equal(result.observation.code, result.code);
  assert.deepEqual(result.observation.listeners, { sigint: 0, sigterm: 0 });
}

describe('P4 root exit vocabulary', () => {
  test('shutdownTriggerFromExit distinguishes success, typed failure, defect, and interruption', () => {
    const failure = new Error('expected typed failure');
    const defect = new Error('unexpected defect');

    assert.deepEqual(shutdownTriggerFromExit(Exit.succeed('done')), { _tag: 'Success' });
    assert.deepEqual(shutdownTriggerFromExit(Effect.runSyncExit(Effect.fail(failure))), {
      _tag: 'Failure',
      error: failure,
    });
    assert.deepEqual(shutdownTriggerFromExit(Effect.runSyncExit(Effect.die(defect))), {
      _tag: 'Defect',
      defect,
    });
    assert.deepEqual(shutdownTriggerFromExit(Effect.runSyncExit(Effect.interrupt)), {
      _tag: 'Interruption',
    });
  });
});

describe('P4 BunRuntime root exit policy', () => {
  test('tagged startup failures retain the established process codes 1 and 3', async () => {
    const refusal = await runFixture('startup-refusal');
    assert.equal(refusal.code, 1);
    assert.equal(refusal.stderr, 'fleetd refused to start: root exit fixture refusal\n');
    assert.equal(refusal.observation.firstSignal, null);
    assertHostReleased(refusal);

    const bind = await runFixture('bind-address-in-use');
    assert.equal(bind.code, 3);
    assert.equal(bind.stderr, 'fleetd already running (root exit fixture)\n');
    assert.equal(bind.observation.firstSignal, null);
    assertHostReleased(bind);
  });

  test('a root defect is reported exactly once and exits 1', async () => {
    const result = await runFixture('defect');

    assert.equal(result.code, 1);
    assert.equal(result.observation.firstSignal, null);
    assert.equal(result.stderr.match(/P4_ROOT_DEFECT_SENTINEL/g)?.length, 1, result.stderr);
    assertHostReleased(result);
  });

  test('an expected first SIGTERM interruption remains a clean exit', async () => {
    const result = await runFixture('signal');

    assert.equal(result.code, 0);
    assert.equal(result.stderr, '');
    assert.equal(result.observation.firstSignal, 'SIGTERM');
    assertHostReleased(result);
  });

  test('a finite successful root exits naturally without reporting an error', async () => {
    const result = await runFixture('finite');

    assert.equal(result.code, 0);
    assert.equal(result.stderr, '');
    assert.equal(result.observation.firstSignal, null);
    assertHostReleased(result);
  });
});
