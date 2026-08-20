import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'bun:test';
import type {
  DaemonAcquisitionCheckpoint,
  DaemonAcquisitionOwner,
} from '../../src/daemon/app/program.ts';
import { randomPort, REPO_ROOT } from '../helpers/daemon.ts';
import { scaleMs } from '../helpers/wait.ts';

type Mode = 'failure' | 'interruption' | 'bind-conflict';
type Target = DaemonAcquisitionCheckpoint | 'process-driver-attached' | 'bind-conflict';

interface Observation {
  readonly mode: Mode;
  readonly target: Target;
  readonly code: number;
  readonly exitFailure: boolean;
  readonly fails: boolean;
  readonly dies: boolean;
  readonly interruptsOnly: boolean;
  readonly errorTag: string | null;
  readonly causeMessage: string | null;
  readonly checkpoints: readonly DaemonAcquisitionCheckpoint[];
  readonly releaseEvents: readonly (DaemonAcquisitionOwner | 'process-driver')[];
  readonly releaseStableAfterCleanup: boolean;
  readonly driverForceCalls: number;
  readonly driverCloseCalls: number;
  readonly homeExists: boolean;
  readonly pidExists: boolean;
  readonly databaseExists: boolean;
  readonly tokenExists: boolean;
  readonly portCanRebind: boolean;
  readonly facadeUnbound: boolean;
  readonly ingressState: 'open' | 'quiescing' | 'closed' | null;
  readonly ingressActiveCount: number | null;
  readonly listenerDelta: {
    readonly sigint: number;
    readonly sigterm: number;
    readonly unhandledRejection: number;
  };
  readonly activeResources: readonly string[];
}

const CHECKPOINTS = [
  'pid-claim',
  'durable-config',
  'process-runtime',
  'database',
  'core',
  'http-owner',
  'background-owners',
  'listener',
  'discovery-network',
  'pollers-boot',
] as const satisfies readonly DaemonAcquisitionCheckpoint[];

const FIXTURE = fileURLToPath(new URL('./fixtures/p4-acquisition-matrix.ts', import.meta.url));
const OBSERVATION_PREFIX = 'ACQUISITION_MATRIX_OBSERVATION ';

function fixtureEnvironment(home: string, port: number): NodeJS.ProcessEnv {
  const env = { ...process.env };
  for (const key of [
    'FLEETDECK_AGENTS_CMD',
    'FLEETDECK_BIND',
    'FLEETDECK_HOME',
    'FLEETDECK_LAN_REFRESH_MS',
    'FLEETDECK_MANAGED',
    'FLEETDECK_MDNS',
    'FLEETDECK_MDNS_SEND_DELAY_MS',
    'FLEETDECK_PORT',
    'FLEETDECK_PROXY_AUTH',
    'FLEETDECK_REQUIRE_TOKEN',
    'FLEETDECK_TEST_CONSOLE_RECORD',
    'FLEETDECK_TEST_NETIFS',
    'FLEETDECK_TEST_NET_FILE',
    'FLEETDECK_TEST_NET_MOCK',
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
    FLEETDECK_BIND: '0.0.0.0',
    FLEETDECK_HOME: home,
    FLEETDECK_LAN_REFRESH_MS: '60000',
    FLEETDECK_MDNS_SEND_DELAY_MS: '1',
    FLEETDECK_PORT: String(port),
    FLEETDECK_PROXY_AUTH: 'token',
    FLEETDECK_REQUIRE_TOKEN: 'off',
    FLEETDECK_TEST_NET_MOCK: '1',
    FLEETDECK_TOKEN: 'effect-acquisition-matrix-token',
    FLEETDECK_TRUSTED_ORIGINS: '',
    FLEETDECK_TRUST_LOOPBACK: 'off',
  };
}

async function runFixture(
  mode: Mode,
  target: Target,
): Promise<{
  readonly observation: Observation;
  readonly stderr: string;
  readonly stdout: string;
}> {
  const scratch = mkdtempSync(path.join(tmpdir(), `fleetdeck-p4-matrix-${mode}-`));
  const home = path.join(scratch, 'home');
  const port = randomPort();
  const child = Bun.spawn([process.execPath, '--no-env-file', FIXTURE, mode, target], {
    cwd: REPO_ROOT,
    env: fixtureEnvironment(home, port),
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stdoutPromise = new Response(child.stdout).text();
  const stderrPromise = new Response(child.stderr).text();
  let timer: ReturnType<typeof setTimeout> | null = null;

  try {
    const exitCode = await Promise.race([
      child.exited,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${mode}/${target} acquisition matrix timed out`)),
          scaleMs(15_000),
        );
      }),
    ]);
    const [stdout, stderr] = await Promise.all([stdoutPromise, stderrPromise]);
    const line = stdout.split('\n').find((candidate) => candidate.startsWith(OBSERVATION_PREFIX));
    assert.ok(line, `missing ${mode}/${target} observation:\n${stdout}\n${stderr}`);
    const observation = JSON.parse(line.slice(OBSERVATION_PREFIX.length)) as Observation;
    assert.equal(exitCode, observation.code, `${mode}/${target}: child/observation exit code`);
    return { observation, stderr, stdout };
  } finally {
    if (timer) clearTimeout(timer);
    if (child.exitCode === null) child.kill('SIGKILL');
    await child.exited;
    await Promise.allSettled([stdoutPromise, stderrPromise]);
    rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
}

function expectedProgramReleases(
  target: DaemonAcquisitionCheckpoint | 'bind-conflict',
): readonly DaemonAcquisitionOwner[] {
  switch (target) {
    case 'pid-claim':
    case 'durable-config':
      return ['pid-claim'];
    case 'process-runtime':
      return ['process-runtime', 'pid-claim'];
    case 'database':
      return ['process-runtime', 'database', 'pid-claim'];
    case 'core':
      return ['core', 'process-runtime', 'database', 'pid-claim'];
    case 'http-owner':
      return ['http-owner', 'core', 'process-runtime', 'database', 'pid-claim'];
    case 'background-owners':
    case 'listener':
    case 'discovery-network':
    case 'pollers-boot':
    case 'bind-conflict':
      return ['mdns', 'http-owner', 'core', 'process-runtime', 'database', 'pid-claim'];
  }
}

function assertNoResidue(observation: Observation, label: string): void {
  assert.equal(observation.homeExists, true, `${label}: HOME remains durable`);
  assert.equal(observation.pidExists, false, `${label}: owned pidfile released`);
  assert.equal(observation.portCanRebind, true, `${label}: listener released before process exit`);
  assert.equal(observation.facadeUnbound, true, `${label}: Promise facade unbound`);
  assert.notEqual(observation.ingressState, 'open', `${label}: ingress admission closed`);
  assert.equal(observation.ingressActiveCount, 0, `${label}: no ingress work remains`);
  assert.equal(observation.releaseStableAfterCleanup, true, `${label}: no late owner callback`);
  assert.deepEqual(
    observation.listenerDelta,
    { sigint: 0, sigterm: 0, unhandledRejection: 0 },
    `${label}: process listeners restored`,
  );
  assert.deepEqual(
    observation.activeResources.filter((resource) =>
      /child|process|tcp|timer|timeout|udp/i.test(resource),
    ),
    [],
    `${label}: no child, listener, socket, timer, or root keep-alive remains`,
  );
}

function assertDurablePrefix(
  observation: Observation,
  target: DaemonAcquisitionCheckpoint,
  label: string,
): void {
  const targetIndex = CHECKPOINTS.indexOf(target);
  assert.deepEqual(
    observation.checkpoints,
    CHECKPOINTS.slice(0, targetIndex + 1),
    `${label}: exact acquired prefix`,
  );
  assert.equal(observation.tokenExists, targetIndex >= CHECKPOINTS.indexOf('durable-config'));
  assert.equal(observation.databaseExists, targetIndex >= CHECKPOINTS.indexOf('database'));
}

describe('P4.7 production acquisition checkpoint matrix', () => {
  test('failure after every acquisition step closes the exact prefix and exits 1', async () => {
    for (const target of CHECKPOINTS) {
      const label = `failure/${target}`;
      const { observation, stderr } = await runFixture('failure', target);

      assert.equal(observation.code, 1, `${label}: stable generic startup exit`);
      assert.equal(observation.exitFailure, true);
      assert.equal(observation.fails, true);
      assert.equal(observation.dies, false);
      assert.equal(observation.interruptsOnly, false);
      assert.equal(observation.errorTag, 'DaemonStartupError');
      assert.equal(observation.causeMessage, `injected acquisition failure after ${target}`);
      assertDurablePrefix(observation, target, label);
      assert.deepEqual(observation.releaseEvents, expectedProgramReleases(target), label);
      assert.equal(observation.driverForceCalls, 0, label);
      assert.equal(observation.driverCloseCalls, 0, label);
      assertNoResidue(observation, label);
      assert.doesNotMatch(stderr, /unhandled|SQLITE_MISUSE|shutdown error/i, label);
    }
  });

  test('interruption after every interruptible step joins the exact prefix and exits 0', async () => {
    for (const target of CHECKPOINTS) {
      const label = `interruption/${target}`;
      const { observation, stderr } = await runFixture('interruption', target);

      assert.equal(observation.code, 0, `${label}: signal-only acquisition interruption is clean`);
      assert.equal(observation.exitFailure, true);
      assert.equal(observation.fails, false);
      assert.equal(observation.dies, false);
      assert.equal(observation.interruptsOnly, true);
      assert.equal(observation.errorTag, null);
      assert.equal(observation.causeMessage, null);
      assertDurablePrefix(observation, target, label);
      assert.deepEqual(
        observation.releaseEvents.filter((owner) => owner !== 'process-driver'),
        expectedProgramReleases(target),
        label,
      );
      assert.equal(observation.driverForceCalls, 1, `${label}: driver force begins synchronously`);
      assert.equal(observation.driverCloseCalls, 1, `${label}: driver cleanup joins once`);
      assertNoResidue(observation, label);
      assert.doesNotMatch(stderr, /unhandled|SQLITE_MISUSE|shutdown error/i, label);
    }
  });

  test('the uninterruptible driver-attachment boundary retires in dependency order', async () => {
    const target = 'process-driver-attached' as const;
    const { observation, stderr } = await runFixture('failure', target);

    assert.equal(observation.code, 1);
    assert.equal(observation.errorTag, 'DaemonStartupError');
    assert.equal(observation.causeMessage, `injected acquisition failure after ${target}`);
    assert.deepEqual(observation.checkpoints, CHECKPOINTS);
    assert.deepEqual(observation.releaseEvents, [
      'mdns',
      'http-owner',
      'core',
      'process-runtime',
      'process-driver',
      'database',
      'pid-claim',
    ]);
    assert.equal(observation.driverForceCalls, 0);
    assert.equal(observation.driverCloseCalls, 1);
    assert.equal(observation.tokenExists, true);
    assert.equal(observation.databaseExists, true);
    assertNoResidue(observation, target);
    assert.doesNotMatch(stderr, /unhandled|SQLITE_MISUSE|shutdown error/i);
  });

  test('a real bind conflict closes the same prefix but retains exit 3', async () => {
    const { observation, stderr } = await runFixture('bind-conflict', 'bind-conflict');

    assert.equal(observation.code, 3);
    assert.equal(observation.errorTag, 'HttpBindStartupError');
    assert.equal(observation.fails, true);
    assert.equal(observation.dies, false);
    assert.deepEqual(
      observation.checkpoints,
      CHECKPOINTS.slice(0, CHECKPOINTS.indexOf('listener')),
    );
    assert.deepEqual(observation.releaseEvents, expectedProgramReleases('bind-conflict'));
    assert.equal(observation.driverForceCalls, 0);
    assert.equal(observation.driverCloseCalls, 0);
    assert.equal(observation.tokenExists, true);
    assert.equal(observation.databaseExists, true);
    assertNoResidue(observation, 'bind-conflict');
    assert.deepEqual(stderr.split('\n').filter(Boolean), [
      'fleetd already running (port bind lost the election)',
    ]);
  });
});
