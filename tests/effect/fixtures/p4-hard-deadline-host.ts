import { appendFileSync, existsSync, unlinkSync, writeFileSync, writeSync } from 'node:fs';
import * as BunRuntime from '@effect/platform-bun/BunRuntime';
import * as Effect from 'effect/Effect';
import { makeDaemonResourceLifecycleOwner } from '../../../src/daemon/app/daemon-resource-lifecycle.ts';
import { DaemonHostControl } from '../../../src/daemon/app/host-control.ts';
import { LifecycleCoordinator } from '../../../src/daemon/app/lifecycle-coordinator.ts';
import { DaemonResources } from '../../../src/daemon/daemon-resources.ts';

const rawPort = Number(process.argv[2]);
const rawPidFile = process.argv[3];
const rawEventFile = process.argv[4];
if (!Number.isInteger(rawPort) || rawPort < 1 || rawPort > 65_535 || !rawPidFile || !rawEventFile) {
  process.stderr.write('usage: p4-hard-deadline-host.ts <port> <pid-file> <event-file>\n');
  process.exit(2);
  throw new Error('unreachable after usage failure');
}
const port = rawPort;
const pidFile = rawPidFile;
const eventFile = rawEventFile;

function record(event: string): void {
  appendFileSync(eventFile, `${JSON.stringify({ event, at: performance.now() })}\n`);
}

const server = Bun.serve({
  hostname: '127.0.0.1',
  port,
  fetch: () => new Response('ok'),
});
writeFileSync(pidFile, JSON.stringify({ pid: process.pid, port }));
record('pid-acquired');

const never = new Promise<void>(() => undefined);
let forceStopStarted = false;
let processReleased = false;
const releaseProcessAtHostExit = (): void => {
  if (processReleased) return;
  processReleased = true;
  if (existsSync(pidFile)) unlinkSync(pidFile);
  record('host-process-release');
};

const resources = new DaemonResources();
resources.setHttp({
  quiesce: () => {
    record('http-quiesce');
  },
  beginGracefulStop: () => {
    record('graceful-stop-start');
    return never;
  },
  releaseHolds: () => {
    record('release-holds');
  },
  closeClients: async () => {
    record('close-clients');
  },
  forceClients: () => {
    record('force-clients');
  },
  forceStop: () => {
    if (!forceStopStarted) {
      forceStopStarted = true;
      record('force-stop-start');
      // Release the native listener, but deliberately never settle the owner
      // Promise. The coordinator must stop awaiting at its absolute deadline,
      // and host teardown must retain pid ownership until process.exit itself.
      void server.stop(true);
    }
    return never;
  },
  close: async () => {
    record('unexpected-http-close');
    await server.stop(true);
  },
});
resources.setStore('store', {
  close: () => {
    record('unexpected-store-close');
  },
});
resources.setProcess('host-process', { close: releaseProcessAtHostExit });

const timeoutMs = 350;
const coordinator = new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(resources), {
  timeoutMs,
  forceReserveMs: 75,
});
const hostControl = new DaemonHostControl();
hostControl.attachLifecycle(coordinator);
hostControl.attachProcessExitFallback(releaseProcessAtHostExit);

writeSync(
  process.stdout.fd,
  `${JSON.stringify({ event: 'ready', pid: process.pid, port, timeoutMs })}\n`,
);

const program = coordinator.close({ _tag: 'Requested', reason: 'hard-deadline-fixture' }).pipe(
  Effect.tap((outcome) =>
    Effect.sync(() => {
      record('coordinator-complete');
      hostControl.recordLifecycleOutcome(outcome, 0);
      hostControl.detachLifecycle(coordinator);
      writeSync(
        process.stdout.fd,
        `${JSON.stringify({
          event: 'outcome',
          deadlineExpired: outcome.deadlineExpired,
          elapsedMs: outcome.finishedAtMs - outcome.startedAtMs,
          phases: outcome.phases.map((phase) => ({ phase: phase.phase, tag: phase._tag })),
        })}\n`,
      );
    }),
  ),
  Effect.asVoid,
);

BunRuntime.runMain(program, {
  disableErrorReporting: true,
  teardown: hostControl.teardown,
});
