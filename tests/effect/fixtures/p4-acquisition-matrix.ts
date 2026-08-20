import { existsSync, writeSync } from 'node:fs';
import path from 'node:path';
import * as BunRuntime from '@effect/platform-bun/BunRuntime';
import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import { DaemonHostControl } from '../../../src/daemon/app/host-control.ts';
import {
  composeDaemonRootLayer,
  makeDaemonLifecycleCoordinator,
  makeDaemonLifecycleLayer,
} from '../../../src/daemon/app/live-layer.ts';
import {
  acquireDaemonResources,
  type DaemonAcquisitionCheckpoint,
  type DaemonAcquisitionOwner,
} from '../../../src/daemon/app/program.ts';
import { withDaemonRootExitPolicy } from '../../../src/daemon/app/root-program.ts';
import { AppConfig } from '../../../src/daemon/app/services/app-config.ts';
import { DaemonLifecycle } from '../../../src/daemon/app/services/daemon-lifecycle.ts';
import type { RootIngressSupervisorService } from '../../../src/daemon/app/services/ingress-supervisor.ts';
import {
  ProcessRunner,
  type ProcessRunnerService,
} from '../../../src/daemon/app/services/process-runner.ts';
import {
  ProcessRuntimeControl,
  type ProcessRuntimeControlService,
} from '../../../src/daemon/app/services/process-runtime-control.ts';
import { execFileP } from '../../../src/daemon/exec.ts';

type Mode = 'failure' | 'interruption' | 'bind-conflict';
type Target = DaemonAcquisitionCheckpoint | 'process-driver-attached' | 'bind-conflict';

const rawMode = process.argv[2];
const rawTarget = process.argv[3];
if (
  (rawMode !== 'failure' && rawMode !== 'interruption' && rawMode !== 'bind-conflict') ||
  !rawTarget
) {
  throw new Error(
    'usage: p4-acquisition-matrix.ts <failure|interruption|bind-conflict> <checkpoint>',
  );
}
const mode: Mode = rawMode;
const target = rawTarget as Target;
const rawHome = process.env['FLEETDECK_HOME'];
const port = Number(process.env['FLEETDECK_PORT']);
if (!rawHome || !Number.isInteger(port) || port < 1 || port > 65_535) {
  throw new Error('P4 acquisition matrix requires FLEETDECK_HOME and FLEETDECK_PORT');
}
const home = rawHome;

const listenerBaseline = {
  sigint: process.listenerCount('SIGINT'),
  sigterm: process.listenerCount('SIGTERM'),
  unhandledRejection: process.listenerCount('unhandledRejection'),
};
const checkpoints: DaemonAcquisitionCheckpoint[] = [];
const releaseEvents: string[] = [];
let driverForceCalls = 0;
let driverCloseCalls = 0;
let capturedIngress: RootIngressSupervisorService | null = null;
let cleanupObservation: Readonly<Record<string, unknown>> | null = null;

const injectedFailure = new Error(`injected acquisition failure after ${target}`);
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
const processControl: ProcessRuntimeControlService = {
  force() {
    driverForceCalls++;
  },
  async close() {
    driverCloseCalls++;
    releaseEvents.push('process-driver');
  },
};

const applicationLayer = Layer.merge(
  Layer.succeed(AppConfig, {
    home,
    port,
    version: 'p4-acquisition-matrix',
  }),
  Layer.merge(
    Layer.succeed(ProcessRunner, processRunner),
    Layer.succeed(ProcessRuntimeControl, processControl),
  ),
);

const blocker =
  mode === 'bind-conflict'
    ? Bun.serve({
        hostname: '0.0.0.0',
        port,
        fetch: () => new Response('occupied'),
      })
    : null;

const daemonLayer = makeDaemonLifecycleLayer({
  acquisitionShutdownTimeoutMs: 1_750,
  acquisitionShutdownReserveMs: 250,
  onAcquisitionShutdownFailure: () => undefined,
  acquireDaemonResources: (signal, ingress) => {
    capturedIngress = ingress;
    return acquireDaemonResources(signal, ingress, {
      afterAcquire(checkpoint) {
        checkpoints.push(checkpoint);
        if (checkpoint !== target) return;
        if (mode === 'failure') throw injectedFailure;
        if (mode !== 'interruption') return;

        return new Promise<void>((_resolve, reject) => {
          const onAbort = (): void => reject(signal.reason);
          signal.addEventListener('abort', onAbort, { once: true });
          if (signal.aborted) {
            onAbort();
            return;
          }
          setImmediate(() => process.kill(process.pid, 'SIGTERM'));
        });
      },
      afterRelease(owner: DaemonAcquisitionOwner) {
        releaseEvents.push(owner);
      },
    });
  },
  makeLifecycleCoordinator: (acquired) => {
    if (target === 'process-driver-attached' && mode === 'failure') throw injectedFailure;
    return makeDaemonLifecycleCoordinator(acquired, {
      timeoutMs: 1_750,
      forceReserveMs: 250,
    });
  },
});
const rootLayer = composeDaemonRootLayer(applicationLayer, daemonLayer);
const hostControl = new DaemonHostControl();

async function portCanRebind(): Promise<boolean> {
  let server: ReturnType<typeof Bun.serve> | null = null;
  try {
    server = Bun.serve({
      hostname: '0.0.0.0',
      port,
      fetch: () => new Response('rebound'),
    });
    return true;
  } catch {
    return false;
  } finally {
    await server?.stop(true);
  }
}

async function observeCleanup(exit: Exit.Exit<unknown, unknown>): Promise<void> {
  await blocker?.stop(true);
  const releaseSnapshot = [...releaseEvents];
  await Bun.sleep(25);
  const facadeUnbound = await execFileP(process.execPath, ['-e', '0']).then(
    () => false,
    (error: unknown) =>
      error instanceof Error && error.message === 'execFileP process runtime is not bound',
  );
  const failure = Exit.isFailure(exit)
    ? exit.cause.reasons.find(Cause.isFailReason)?.error
    : undefined;
  const failureRecord =
    failure !== null && typeof failure === 'object'
      ? (failure as Readonly<Record<string, unknown>>)
      : null;
  const cause = failureRecord?.['cause'];

  cleanupObservation = {
    exitFailure: Exit.isFailure(exit),
    fails: Exit.isFailure(exit) && Cause.hasFails(exit.cause),
    dies: Exit.isFailure(exit) && Cause.hasDies(exit.cause),
    interruptsOnly: Exit.isFailure(exit) && Cause.hasInterruptsOnly(exit.cause),
    errorTag: typeof failureRecord?.['_tag'] === 'string' ? failureRecord['_tag'] : null,
    causeMessage: cause instanceof Error ? cause.message : null,
    checkpoints,
    releaseEvents,
    releaseStableAfterCleanup: releaseEvents.length === releaseSnapshot.length,
    driverForceCalls,
    driverCloseCalls,
    homeExists: existsSync(home),
    pidExists: existsSync(path.join(home, 'fleetd.pid')),
    databaseExists: existsSync(path.join(home, 'fleetd.db')),
    tokenExists: existsSync(path.join(home, 'token')),
    portCanRebind: await portCanRebind(),
    facadeUnbound,
    ingressState: capturedIngress?.state ?? null,
    ingressActiveCount: capturedIngress?.activeCount ?? null,
  };
}

process.once('exit', (code) => {
  const activeResources = process.getActiveResourcesInfo?.() ?? [];
  writeSync(
    process.stdout.fd,
    `ACQUISITION_MATRIX_OBSERVATION ${JSON.stringify({
      mode,
      target,
      code,
      ...cleanupObservation,
      listenerDelta: {
        sigint: process.listenerCount('SIGINT') - listenerBaseline.sigint,
        sigterm: process.listenerCount('SIGTERM') - listenerBaseline.sigterm,
        unhandledRejection:
          process.listenerCount('unhandledRejection') - listenerBaseline.unhandledRejection,
      },
      activeResources,
    })}\n`,
  );
});

const program = Effect.provide(Effect.as(DaemonLifecycle, undefined), rootLayer).pipe(
  Effect.onExit((exit) => Effect.promise(() => observeCleanup(exit))),
);

BunRuntime.runMain(withDaemonRootExitPolicy(hostControl, program), {
  disableErrorReporting: true,
  teardown: hostControl.teardown,
});
