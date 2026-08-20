import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import * as Runtime from 'effect/Runtime';
import { DaemonStartupError, DaemonStartupRefusalError } from '../../src/daemon/app/errors.ts';
import {
  composeDaemonRootLayer,
  makeDaemonLifecycleLayer,
} from '../../src/daemon/app/live-layer.ts';
import { AppConfig } from '../../src/daemon/app/services/app-config.ts';
import {
  ProcessRunner,
  type ProcessRunnerService,
} from '../../src/daemon/app/services/process-runner.ts';
import { ProcessRuntimeControl } from '../../src/daemon/app/services/process-runtime-control.ts';
import { randomPort, startDaemon } from '../helpers/daemon.ts';
import { scaleMs } from '../helpers/wait.ts';
import { runEffectExit } from './helpers.ts';

const ROOT_FIXTURE = fileURLToPath(new URL('./fixtures/startup-refusal-root.ts', import.meta.url));
const PID_RELEASE_FIXTURE = fileURLToPath(
  new URL('./fixtures/pid-release-retry.ts', import.meta.url),
);
const OBSERVATION_PREFIX = 'STARTUP_EXIT_OBSERVATION ';
const PID_OBSERVATION_PREFIX = 'PID_RELEASE_OBSERVATION ';

interface StartupObservation {
  readonly code: number;
  readonly homeExists: boolean;
  readonly pidExists: boolean;
  readonly listeners: {
    readonly sigint: number;
    readonly sigterm: number;
    readonly unhandledRejection: number;
  };
}

interface FixtureResult {
  readonly code: number;
  readonly stdout: string;
  readonly stderr: string;
}

const processRunner: ProcessRunnerService = {
  run: () => Effect.succeed({ ok: true, out: '' }),
  runBounded: () =>
    Effect.succeed({
      code: 0,
      stdout: Buffer.alloc(0),
      stderr: '',
      truncated: false,
      timedOut: false,
    }),
};

function cleanDaemonEnvironment(overrides: Record<string, string>): NodeJS.ProcessEnv {
  const env = { ...process.env };
  for (const key of [
    'FLEETDECK_BIND',
    'FLEETDECK_HOME',
    'FLEETDECK_MANAGED',
    'FLEETDECK_MDNS',
    'FLEETDECK_PORT',
    'FLEETDECK_PROXY_AUTH',
    'FLEETDECK_REQUIRE_TOKEN',
    'FLEETDECK_TOKEN',
    'FLEETDECK_TRUSTED_ORIGINS',
    'FLEETDECK_TRUST_LOOPBACK',
    'FLEETDECK_VERSION_OVERRIDE',
  ]) {
    delete env[key];
  }
  return {
    ...env,
    FLEETDECK_AGENTS_CMD: 'false',
    FLEETDECK_BIND: '127.0.0.1',
    FLEETDECK_MDNS: 'off',
    FLEETDECK_PROXY_AUTH: 'token',
    FLEETDECK_REQUIRE_TOKEN: 'off',
    FLEETDECK_TOKEN: 'typed-startup-fixture-token',
    FLEETDECK_TRUSTED_ORIGINS: '',
    FLEETDECK_TRUST_LOOPBACK: 'off',
    ...overrides,
  };
}

async function runFixture(
  fixture: string,
  overrides: Record<string, string>,
): Promise<FixtureResult> {
  const child = Bun.spawn([process.execPath, '--no-env-file', fixture], {
    cwd: path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..'),
    env: cleanDaemonEnvironment(overrides),
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stdout = new Response(child.stdout).text();
  const stderr = new Response(child.stderr).text();
  let timer: ReturnType<typeof setTimeout> | null = null;

  try {
    const code = await Promise.race([
      child.exited,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`startup fixture ${path.basename(fixture)} timed out`)),
          scaleMs(15_000),
        );
      }),
    ]);
    const [output, errorOutput] = await Promise.all([stdout, stderr]);
    return { code, stdout: output, stderr: errorOutput };
  } finally {
    if (timer) clearTimeout(timer);
    if (child.exitCode === null) child.kill('SIGKILL');
    await child.exited;
    await Promise.allSettled([stdout, stderr]);
  }
}

function startupObservation(stdout: string): StartupObservation {
  const line = stdout.split('\n').find((candidate) => candidate.startsWith(OBSERVATION_PREFIX));
  assert.ok(line, `missing startup observation in stdout:\n${stdout}`);
  return JSON.parse(line.slice(OBSERVATION_PREFIX.length)) as StartupObservation;
}

function stderrLines(stderr: string): string[] {
  return stderr.split('\n').filter(Boolean);
}

function assertListenersReleased(observation: StartupObservation): void {
  assert.deepEqual(observation.listeners, {
    sigint: 0,
    sigterm: 0,
    unhandledRejection: 0,
  });
}

function failureLayer(error: unknown) {
  const application = Layer.merge(
    Layer.succeed(AppConfig, { home: '/unused', port: 4711, version: 'startup-error-test' }),
    Layer.merge(
      Layer.succeed(ProcessRunner, processRunner),
      Layer.succeed(ProcessRuntimeControl, {
        force() {},
        close: () => Promise.resolve(),
      }),
    ),
  );
  const daemon = makeDaemonLifecycleLayer({
    acquireDaemonResources: async () => {
      throw error;
    },
    acquisitionShutdownTimeoutMs: 50,
    acquisitionShutdownReserveMs: 0,
    onAcquisitionShutdownFailure() {},
    makeLifecycleCoordinator: () => {
      throw new Error('unreachable coordinator fixture');
    },
  });
  return composeDaemonRootLayer(application, daemon);
}

describe('P4.7 typed startup errors', () => {
  test('the root Layer preserves expected refusals and wraps unknown acquisition failures', async () => {
    const refusal = new DaemonStartupRefusalError({
      reason: 'representative refusal',
      message: 'fleetd refused to start: representative refusal',
      cleanupCause: null,
    });
    const refusalExit = await runEffectExit(Effect.provide(Effect.void, failureLayer(refusal)));
    assert.ok(Exit.isFailure(refusalExit));
    const preserved = refusalExit.cause.reasons.find(Cause.isFailReason)?.error;
    assert.equal(preserved, refusal);
    assert.equal(Runtime.getErrorExitCode(preserved), 1);

    const unknown = new Error('unknown acquisition failure');
    const unknownExit = await runEffectExit(Effect.provide(Effect.void, failureLayer(unknown)));
    assert.ok(Exit.isFailure(unknownExit));
    const mapped = unknownExit.cause.reasons.find(Cause.isFailReason)?.error;
    assert.ok(mapped instanceof DaemonStartupError);
    assert.equal(mapped.cause, unknown);
    assert.equal(Runtime.getErrorExitCode(mapped), 1);
  });

  test('invalid port, config, token, and HOME path each report once and release ownership', async () => {
    const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-startup-errors-'));
    try {
      const invalidPortHome = path.join(scratch, 'invalid-port-home');
      const invalidPort = await runFixture(ROOT_FIXTURE, {
        FLEETDECK_HOME: invalidPortHome,
        FLEETDECK_PORT: '0',
      });
      assert.equal(invalidPort.code, 1);
      assert.deepEqual(stderrLines(invalidPort.stderr), [
        'fleetd refused to start: invalid FLEETDECK_PORT "0" — expected an integer port in 1..65535 (port 0 is not supported)',
      ]);
      const invalidPortObservation = startupObservation(invalidPort.stdout);
      assert.equal(invalidPortObservation.code, 1);
      assert.equal(invalidPortObservation.homeExists, false, 'port validation precedes HOME');
      assert.equal(invalidPortObservation.pidExists, false);
      assertListenersReleased(invalidPortObservation);

      const invalidConfigHome = path.join(scratch, 'invalid-config-home');
      const invalidConfig = await runFixture(ROOT_FIXTURE, {
        FLEETDECK_HOME: invalidConfigHome,
        FLEETDECK_PORT: String(randomPort()),
        FLEETDECK_PROXY_AUTH: 'bogus',
      });
      assert.equal(invalidConfig.code, 1);
      assert.deepEqual(stderrLines(invalidConfig.stderr), [
        "fleetd refused to start: FLEETDECK_PROXY_AUTH must be 'token' or 'trust' (got 'bogus')",
      ]);
      const invalidConfigObservation = startupObservation(invalidConfig.stdout);
      assert.equal(invalidConfigObservation.pidExists, false);
      assertListenersReleased(invalidConfigObservation);

      const invalidTokenHome = path.join(scratch, 'invalid-token-home');
      const invalidToken = await runFixture(ROOT_FIXTURE, {
        FLEETDECK_HOME: invalidTokenHome,
        FLEETDECK_PORT: String(randomPort()),
        FLEETDECK_TOKEN: 'too-short',
      });
      assert.equal(invalidToken.code, 1);
      assert.deepEqual(stderrLines(invalidToken.stderr), [
        'fleetd refused to start: FLEETDECK_TOKEN must be at least 16 characters after trimming',
      ]);
      assert.doesNotMatch(invalidToken.stderr, /too-short/);
      const invalidTokenObservation = startupObservation(invalidToken.stdout);
      assert.equal(invalidTokenObservation.pidExists, false);
      assertListenersReleased(invalidTokenObservation);

      const invalidPath = path.join(scratch, 'home-is-a-file');
      writeFileSync(invalidPath, 'not a directory');
      const pathFailure = await runFixture(ROOT_FIXTURE, {
        FLEETDECK_HOME: invalidPath,
        FLEETDECK_PORT: String(randomPort()),
      });
      assert.equal(pathFailure.code, 1);
      const pathLines = stderrLines(pathFailure.stderr);
      assert.equal(pathLines.length, 1);
      assert.match(pathLines[0] ?? '', /^fleetd refused to start: cannot create FLEETDECK_HOME \(/);
      const pathObservation = startupObservation(pathFailure.stdout);
      assert.equal(pathObservation.homeExists, true);
      assert.equal(pathObservation.pidExists, false);
      assertListenersReleased(pathObservation);
    } finally {
      rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    }
  });

  test('a live HOME owner produces one typed refusal without disturbing its pidfile', async () => {
    const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-startup-live-owner-'));
    const incumbentPort = randomPort();
    let challengerPort = randomPort();
    while (challengerPort === incumbentPort) challengerPort = randomPort();
    const incumbent = await startDaemon({ port: incumbentPort, home });

    try {
      const pidFile = path.join(home, 'fleetd.pid');
      const ownerBefore = readFileSync(pidFile, 'utf8');
      const challenger = await runFixture(ROOT_FIXTURE, {
        FLEETDECK_HOME: home,
        FLEETDECK_PORT: String(challengerPort),
      });

      assert.equal(challenger.code, 1);
      const lines = stderrLines(challenger.stderr);
      assert.equal(lines.length, 1);
      assert.match(
        lines[0] ?? '',
        new RegExp(`already used by live fleetd pid .* port ${incumbentPort}`),
      );
      const observation = startupObservation(challenger.stdout);
      assert.equal(observation.pidExists, true, 'the incumbent ownership record remains live');
      assertListenersReleased(observation);
      assert.equal(readFileSync(pidFile, 'utf8'), ownerBefore);
      assert.equal(incumbent.proc.exitCode, null);
    } finally {
      await incumbent.stop();
    }
  });

  test('pid release retains ownership across an injected unlink failure and retries exactly', async () => {
    const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-pid-release-'));
    const home = path.join(scratch, 'home');
    try {
      const result = await runFixture(PID_RELEASE_FIXTURE, {
        FLEETDECK_HOME: home,
        FLEETDECK_PORT: String(randomPort()),
      });
      assert.equal(result.code, 0, result.stderr || result.stdout);
      const line = result.stdout
        .split('\n')
        .find((candidate) => candidate.startsWith(PID_OBSERVATION_PREFIX));
      assert.ok(line, `missing pid release observation in stdout:\n${result.stdout}`);
      const observation = JSON.parse(line.slice(PID_OBSERVATION_PREFIX.length)) as {
        readonly closeErrors: readonly string[];
        readonly fallbackErrorIsInjected: boolean;
        readonly pidRetainedAfterFailure: boolean;
        readonly exitListenerDeltaAfterFailure: number;
        readonly pidRemovedAfterRetry: boolean;
        readonly ingressState: string;
        readonly listenerDelta: StartupObservation['listeners'] & { readonly exit: number };
      };

      assert.deepEqual(observation.closeErrors, ['host-process']);
      assert.equal(observation.fallbackErrorIsInjected, true);
      assert.equal(observation.pidRetainedAfterFailure, true);
      assert.equal(
        observation.exitListenerDeltaAfterFailure,
        1,
        'failed release retains one exact process-exit retry',
      );
      assert.equal(observation.pidRemovedAfterRetry, true);
      assert.equal(observation.ingressState, 'closed');
      assert.deepEqual(observation.listenerDelta, {
        exit: 0,
        sigint: 0,
        sigterm: 0,
        unhandledRejection: 0,
      });
    } finally {
      rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    }
  });
});
