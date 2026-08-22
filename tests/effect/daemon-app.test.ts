import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { describe, test } from 'bun:test';
import { randomPort, REPO_ROOT, spawnRaw, startDaemon } from '../helpers/daemon.ts';

const PROGRAM = path.join(REPO_ROOT, 'src/daemon/app/program.ts');
const LIVE_LAYER = path.join(REPO_ROOT, 'src/daemon/app/live-layer.ts');
const ROOT_PROGRAM = path.join(REPO_ROOT, 'src/daemon/app/root-program.ts');
const ENTRYPOINT = path.join(REPO_ROOT, 'src/daemon/fleetd.ts');
const IMPORT_FIXTURE = path.join(REPO_ROOT, 'tests/effect/fixtures/daemon-app-import.ts');

interface ImportObservation {
  readonly exports: readonly string[];
  readonly homeExists: boolean;
  readonly homeEnvironment: string;
  readonly listenerDelta: {
    readonly sigint: number;
    readonly sigterm: number;
    readonly unhandledRejection: number;
  };
}

async function runImportFixture(home: string): Promise<ImportObservation> {
  const child = Bun.spawn([process.execPath, '--no-env-file', IMPORT_FIXTURE], {
    cwd: REPO_ROOT,
    env: {
      ...process.env,
      FLEETDECK_HOME: home,
      // If importing the seam accidentally boots it, the preflight exits 1.
      FLEETDECK_PORT: '0',
    },
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stdout = new Response(child.stdout).text();
  const stderr = new Response(child.stderr).text();
  try {
    const exitCode = await Promise.race([
      child.exited,
      Bun.sleep(5_000).then(() => {
        throw new Error('daemon app import fixture did not exit naturally');
      }),
    ]);
    assert.equal(exitCode, 0, await stderr);
    return JSON.parse((await stdout).trim()) as ImportObservation;
  } finally {
    if (child.exitCode === null) child.kill('SIGKILL');
    await child.exited;
  }
}

function assertSourceOrder(source: string, needles: readonly string[]): void {
  let previous = -1;
  for (const needle of needles) {
    const position = source.indexOf(needle, previous + 1);
    assert.notEqual(position, -1, `missing boot step: ${needle}`);
    assert.ok(position > previous, `boot step moved out of order: ${needle}`);
    previous = position;
  }
}

describe('DaemonApp extraction', () => {
  test('program import is acquisition-free and exposes only the typed acquisition seam', async () => {
    const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-daemon-app-import-'));
    const home = path.join(scratch, 'must-not-be-created');
    try {
      const observation = await runImportFixture(home);
      assert.deepEqual(observation.exports, ['acquireDaemonResources']);
      assert.equal(observation.homeExists, false);
      assert.equal(observation.homeEnvironment, home);
      assert.deepEqual(observation.listenerDelta, {
        sigint: 0,
        sigterm: 0,
        unhandledRejection: 0,
      });
    } finally {
      rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    }
  });

  test('thin root entrypoint and program retain the exact acquisition order', () => {
    const entrypoint = readFileSync(ENTRYPOINT, 'utf8');
    const program = readFileSync(PROGRAM, 'utf8');
    const liveLayer = readFileSync(LIVE_LAYER, 'utf8');
    const rootProgram = readFileSync(ROOT_PROGRAM, 'utf8');

    assert.match(
      entrypoint,
      /import \{ DaemonApp, daemonHostControl \} from '\.\/app\/root-program\.ts';/,
    );
    assert.match(entrypoint, /BunRuntime\.runMain\(DaemonApp,/);
    assert.doesNotMatch(entrypoint, /ManagedRuntime|DaemonResources|createHttp/);
    assert.match(program, /export function acquireDaemonResources\(/);
    assert.doesNotMatch(program, /DaemonApp|BunRuntime|runMain|ManagedRuntime\.make/);
    assert.match(rootProgram, /export const DaemonApp = makeDaemonApp\(daemonHostControl\);/);
    assert.match(rootProgram, /import \{ acquireDaemonResources \} from '\.\/program\.ts';/);
    assert.doesNotMatch(program, /createBootstrapProcessRuntimeBridge|bootstrap-process-runtime/);

    // AppConfig and Background preparation belong to the root Layer now. Pin
    // the P5 ownership transition: publish readiness before native acquisition,
    // start the one cold program only after acquisition succeeds, register its
    // producer before lifecycle sealing, and only then construct the coordinator.
    assertSourceOrder(liveLayer, [
      'const config = yield* AppConfig;',
      'const ingress = yield* IngressSupervisor;',
      'const processControl = yield* ProcessRuntimeControl;',
      'const program = yield* Deferred.make<Effect.Effect<never, never, ProcessRunner>>();',
      'const prepared = yield* prepareBackgroundOwner<ProcessRunner>({',
      'const acquired = yield* acquireDaemonResourcesOwned(options, ingress, processControl, {',
      'yield* Deferred.succeed(program, acquired.backgroundProgram);',
      'const owner = yield* prepared.start;',
      "acquired.resources.addProducer('effect-background', { close: owner.close });",
      'coordinator: options.makeLifecycleCoordinator(acquired),',
    ]);

    // The native acquisition order remains byte-for-byte observable, while the
    // final boot/scheduler Effect stays cold for the Layer-owned start above.
    assertSourceOrder(program, [
      'installConsoleRecorder();',
      'const PORT = inputs.config.port;',
      'const HOME = inputs.config.home;',
      'const version = inputs.config.version;',
      "process.on('unhandledRejection'",
      'const daemonResources = new DaemonResources(',
      'daemonResources.setProcess(',
      'await claimHome();',
      'TRUSTED_ORIGINS = parseTrustedOrigins(',
      'makeIngressExecFileDelegate(ingress)',
      'daemonResources.setProcessRuntime(',
      'db = openDb(DB_FILE);',
      "daemonResources.setStore('sqlite'",
      'core = createCore(db, { port: PORT, version });',
      'daemonResources.setCore(',
      'http = createHttp(core, {',
      'daemonResources.setHttp(',
      'const result = await http.bind(PORT, BIND);',
      'console.log(`fleetd up on http://${boundHost}:${PORT}',
      'const boot = legacyBootReconciliationWithoutRetentionWork({',
      'const retentionWork = legacyRetentionWork({',
      'const backgroundProgram: Effect.Effect<never, never, ProcessRunner> = Effect.gen(',
      'return yield* makeDaemonBackgroundProgram(inputs.backgroundController, {',
      'backgroundProgram,',
    ]);
  });

  test('a failure after pid and runtime ownership closes the acquired prefix', async () => {
    const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-daemon-app-fault-'));
    const home = path.join(scratch, 'home');
    mkdirSync(home);
    // openDb must reject this path after HOME/pid/runtime ownership is acquired.
    mkdirSync(path.join(home, 'fleetd.db'));
    const daemon = spawnRaw({ port: randomPort(), home });
    try {
      const exitCode = await daemon.waitForExit(10_000);
      assert.equal(exitCode, 1, `stderr: ${daemon.stderr}`);
      assert.equal(
        existsSync(path.join(home, 'fleetd.pid')),
        false,
        'partial acquisition must release its exact owned pid record',
      );
      assert.equal(
        existsSync(path.join(home, 'fleetd.db')),
        true,
        'startup cleanup must not delete persistent state paths',
      );
    } finally {
      await daemon.kill();
      rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    }
  });

  test('source boot retains package-version discovery and graceful signal exit', async () => {
    const daemon = await startDaemon();
    try {
      const response = await fetch(`${daemon.baseUrl}/health`);
      assert.equal(response.status, 200);
      const health = (await response.json()) as { version?: string };
      const packageJson = JSON.parse(
        readFileSync(path.join(REPO_ROOT, 'package.json'), 'utf8'),
      ) as { version: string };
      assert.equal(
        health.version,
        process.env['FLEETDECK_VERSION_OVERRIDE']?.trim() || packageJson.version,
      );
    } finally {
      await daemon.stop();
    }
    assert.equal(daemon.proc.exitCode, 0, `stderr: ${daemon.stderr}`);
  });
});
