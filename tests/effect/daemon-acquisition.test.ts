import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'bun:test';
import * as Runtime from 'effect/Runtime';
import { HttpBindStartupError } from '../../src/daemon/app/errors.ts';
import { randomPort, REPO_ROOT } from '../helpers/daemon.ts';
import { scaleMs } from '../helpers/wait.ts';

type Mode = 'success' | 'abort' | 'bind-failure';

interface Observation {
  readonly mode: Mode;
  readonly acquired: boolean;
  readonly healthStatus: number | null;
  readonly processRuns: number;
  readonly ingressState: 'open' | 'quiescing' | 'closed';
  readonly shutdownExitCode: number | null;
  readonly facadeUnbound: boolean;
  readonly homeExists: boolean;
  readonly pidExists: boolean;
  readonly databaseExists: boolean;
  readonly listenerDelta: {
    readonly sigint: number;
    readonly sigterm: number;
    readonly unhandledRejection: number;
  };
  readonly errorTag: string | null;
  readonly errorName: string | null;
  readonly errorMessage: string | null;
  readonly errorReason: string | null;
  readonly errorCode: string | null;
  readonly causeCode: string | null;
  readonly runtimeExitCode: number | null;
}

const FIXTURE = fileURLToPath(new URL('./fixtures/daemon-acquisition.ts', import.meta.url));
const OBSERVATION_PREFIX = 'ACQUISITION_OBSERVATION ';

async function runFixture(mode: Mode): Promise<{ observation: Observation; stderr: string }> {
  const scratch = mkdtempSync(path.join(tmpdir(), `fleetdeck-acquisition-${mode}-`));
  const home = path.join(scratch, 'home');
  const child = Bun.spawn([process.execPath, '--no-env-file', FIXTURE, mode], {
    cwd: REPO_ROOT,
    env: {
      ...process.env,
      FLEETDECK_HOME: home,
      FLEETDECK_PORT: String(randomPort()),
      FLEETDECK_BIND: '127.0.0.1',
      FLEETDECK_TOKEN: 'effect-acquisition-fixture-token',
      FLEETDECK_AGENTS_CMD: 'false',
      FLEETDECK_MDNS: 'off',
    },
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stdout = new Response(child.stdout).text();
  const stderr = new Response(child.stderr).text();
  let timer: ReturnType<typeof setTimeout> | null = null;

  try {
    const exitCode = await Promise.race([
      child.exited,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${mode} acquisition fixture timed out`)),
          scaleMs(15_000),
        );
      }),
    ]);
    const [output, errorOutput] = await Promise.all([stdout, stderr]);
    assert.equal(exitCode, 0, errorOutput || output);
    const line = output.split('\n').find((candidate) => candidate.startsWith(OBSERVATION_PREFIX));
    assert.ok(line, `missing acquisition observation in stdout:\n${output}`);
    return {
      observation: JSON.parse(line.slice(OBSERVATION_PREFIX.length)) as Observation,
      stderr: errorOutput,
    };
  } finally {
    if (timer) clearTimeout(timer);
    if (child.exitCode === null) child.kill('SIGKILL');
    await child.exited;
    await Promise.allSettled([stdout, stderr]);
    rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
}

function assertOwnersReleased(observation: Observation): void {
  assert.equal(observation.homeExists, true);
  assert.equal(observation.pidExists, false);
  assert.equal(observation.facadeUnbound, true);
  assert.deepEqual(observation.listenerDelta, {
    sigint: 0,
    sigterm: 0,
    unhandledRejection: 0,
  });
}

describe('root-owned daemon acquisition', () => {
  test('generic typed bind failures retain their cause and established exit 1 policy', () => {
    const cause = Object.assign(new Error('bind setup failed'), {
      code: 'EACCES',
      errno: -13,
    });
    const error = new HttpBindStartupError({
      reason: 'other',
      origin: 'bun-serve-throw',
      code: cause.code,
      errno: cause.errno,
      message: cause.message,
      cause,
    });

    assert.equal(Runtime.getErrorExitCode(error), 1);
    assert.equal(error.cause, cause);
    assert.equal(error.code, 'EACCES');
    assert.equal(error.errno, -13);
  });

  test('successful bind starts readiness work and closes every acquired owner', async () => {
    const { observation, stderr } = await runFixture('success');

    assert.equal(stderr, '');
    assert.equal(observation.acquired, true);
    assert.equal(observation.healthStatus, 200);
    assert.ok(observation.processRuns > 0, 'post-bind reconciliation uses the root ingress facade');
    assert.equal(observation.ingressState, 'closed');
    assert.equal(observation.shutdownExitCode, 0);
    assert.equal(observation.databaseExists, true);
    assert.equal(observation.errorTag, null);
    assert.equal(observation.runtimeExitCode, null);
    assertOwnersReleased(observation);
  });

  test('interrupting the first async boundary releases pid/listener ownership without opening DB', async () => {
    const { observation, stderr } = await runFixture('abort');

    assert.equal(stderr, '');
    assert.equal(observation.acquired, false);
    assert.equal(observation.healthStatus, null);
    assert.equal(observation.processRuns, 0);
    assert.equal(observation.ingressState, 'quiescing');
    assert.equal(observation.databaseExists, false);
    assert.equal(observation.errorTag, null);
    assert.equal(observation.errorName, 'AbortError');
    assert.equal(observation.runtimeExitCode, 1);
    assertOwnersReleased(observation);
  });

  test('EADDRINUSE is typed as exit 3 and cannot start reconciliation or leak a prefix', async () => {
    const { observation, stderr } = await runFixture('bind-failure');

    assert.equal(stderr, '');
    assert.equal(observation.acquired, false);
    assert.equal(observation.healthStatus, null);
    assert.equal(observation.processRuns, 0, 'reconciliation starts only after successful bind');
    assert.equal(observation.ingressState, 'closed');
    assert.equal(
      observation.databaseExists,
      true,
      'persistent SQLite state is closed, not deleted',
    );
    assert.equal(observation.errorTag, 'HttpBindStartupError');
    assert.equal(observation.errorReason, 'address-in-use');
    assert.equal(observation.errorCode, 'EADDRINUSE');
    assert.equal(observation.causeCode, 'EADDRINUSE');
    assert.equal(observation.errorMessage, 'fleetd already running (port bind lost the election)');
    assert.equal(observation.runtimeExitCode, 3);
    assertOwnersReleased(observation);
  });
});
