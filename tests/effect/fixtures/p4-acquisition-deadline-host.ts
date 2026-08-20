import { appendFileSync, existsSync, unlinkSync, writeFileSync, writeSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
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
  ProcessRunnerLive,
} from '../../../src/daemon/app/live-layer.ts';
import type { AcquiredDaemonResources } from '../../../src/daemon/app/program.ts';
import { AppConfig } from '../../../src/daemon/app/services/app-config.ts';
import { execEffect } from '../../../src/daemon/app/services/process-runner.ts';

const rawPort = Number(process.argv[2]);
const rawPidFile = process.argv[3];
const rawChildPidFile = process.argv[4];
const rawEventFile = process.argv[5];
if (
  !Number.isInteger(rawPort) ||
  rawPort < 1 ||
  rawPort > 65_535 ||
  !rawPidFile ||
  !rawChildPidFile ||
  !rawEventFile
) {
  throw new Error(
    'usage: p4-acquisition-deadline-host.ts <port> <pid-file> <child-pid-file> <event-file>',
  );
}
const port = rawPort;
const pidFile = rawPidFile;
const childPidFile = rawChildPidFile;
const eventFile = rawEventFile;
const acquisitionShutdownTimeoutMs = 1_750;
const childFixture = fileURLToPath(new URL('./p4-acquisition-deadline-child.ts', import.meta.url));

function record(event: string, details: Readonly<Record<string, unknown>> = {}): void {
  appendFileSync(eventFile, `${JSON.stringify({ event, at: performance.now(), ...details })}\n`);
}

let processOwnershipReleased = false;
const releaseProcessOwnershipAtExit = (): void => {
  if (processOwnershipReleased) return;
  processOwnershipReleased = true;
  if (existsSync(pidFile)) unlinkSync(pidFile);
  record('host-process-release');
};

// This is the acquisition-prefix counterpart of program.ts's early fallback:
// no AcquiredDaemonResources value exists yet for HostControl to retain, so the
// pid marker remains owned until the actual process-exit event.
process.once('exit', releaseProcessOwnershipAtExit);
process.once('exit', () => {
  record('host-exit-observation', {
    pidExists: existsSync(pidFile),
    listeners: {
      sigint: process.listenerCount('SIGINT'),
      sigterm: process.listenerCount('SIGTERM'),
      unhandledRejection: process.listenerCount('unhandledRejection'),
    },
  });
});

const hostControl = new DaemonHostControl();
const applicationLayer = Layer.merge(
  Layer.succeed(AppConfig, {
    home: '',
    port,
    version: 'p4-acquisition-deadline-fixture',
  }),
  ProcessRunnerLive,
);

const daemonLayer = makeDaemonLifecycleLayer({
  acquisitionShutdownTimeoutMs,
  acquisitionShutdownReserveMs: 250,
  onAcquisitionShutdownFailure: (failure) => {
    record('acquisition-shutdown-failure', { tag: failure._tag });
    hostControl.recordExitCode(1);
  },
  acquireDaemonResources: async (signal, ingress) => {
    writeFileSync(pidFile, JSON.stringify({ pid: process.pid, port }));
    record('pid-acquired');

    const server = Bun.serve({
      hostname: '127.0.0.1',
      port,
      fetch: () => new Response('acquiring'),
    });
    record('listener-bound');

    signal.addEventListener(
      'abort',
      () => {
        record('acquisition-abort');
        void Promise.resolve(server.stop(true)).then(
          () => record('listener-closed'),
          (cause: unknown) =>
            record('listener-close-failed', {
              message: cause instanceof Error ? cause.message : String(cause),
            }),
        );
      },
      { once: true },
    );

    const childRun = ingress.runPromise(
      'acquisition-deadline-child',
      execEffect({
        argv: [process.execPath, '--no-env-file', childFixture, childPidFile],
        timeoutMs: 60_000,
        killTree: true,
      }),
    );
    void childRun.then(
      () => record('child-effect-settled'),
      () => record('child-effect-settled'),
    );

    const childDeadline = performance.now() + 2_000;
    while (!existsSync(childPidFile) && performance.now() < childDeadline) {
      signal.throwIfAborted();
      await Bun.sleep(5);
    }
    if (!existsSync(childPidFile)) throw new Error('fixture child did not publish its pid record');
    record('child-active');

    writeSync(
      process.stdout.fd,
      `${JSON.stringify({
        event: 'ready',
        pid: process.pid,
        port,
        acquisitionShutdownTimeoutMs,
      })}\n`,
    );

    // Deliberately ignore the abort after closing the listener. The live-layer
    // cancellation owner must stop waiting at the absolute deadline while its
    // late-retirement continuation remains attached to this never-settling
    // acquisition.
    return await new Promise<AcquiredDaemonResources>(() => undefined);
  },
  makeLifecycleCoordinator: (acquired) =>
    makeDaemonLifecycleCoordinator(acquired, { timeoutMs: acquisitionShutdownTimeoutMs }),
});
const rootLayer = composeDaemonRootLayer(applicationLayer, daemonLayer);

const provided = Effect.never.pipe(
  Effect.provide(rootLayer),
  Effect.onExit((exit) =>
    Effect.sync(() => {
      record('root-exit');
      if (
        hostControl.signalObserved &&
        Exit.isFailure(exit) &&
        Cause.hasInterruptsOnly(exit.cause)
      ) {
        // Mirrors production: this must not downgrade the timeout's prior 1.
        hostControl.recordExitCode(0);
      }
    }),
  ),
);
const app = Effect.acquireUseRelease(
  Effect.sync(() => hostControl.installSignalObserver()),
  () => provided,
  (removeSignalObserver) => Effect.sync(removeSignalObserver),
);

BunRuntime.runMain(app, {
  disableErrorReporting: true,
  teardown: (exit, onExit) =>
    hostControl.teardown(exit, (code) => {
      record('host-teardown', { code });
      onExit(code);
    }),
});
