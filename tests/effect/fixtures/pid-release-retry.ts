import fs, { existsSync } from 'node:fs';
import path from 'node:path';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Scope from 'effect/Scope';
import { prepareBackgroundOwner } from '../../../src/daemon/app/background-owner.ts';
import { acquireDaemonResources } from '../../../src/daemon/app/program.ts';
import type { RootIngressSupervisorService } from '../../../src/daemon/app/services/ingress-supervisor.ts';
import {
  ProcessRunner,
  type ProcessRunnerService,
} from '../../../src/daemon/app/services/process-runner.ts';
import { makeIngressSupervisor } from '../../../src/daemon/platform/bun/ingress-supervisor-live.ts';

const home = process.env['FLEETDECK_HOME'];
if (!home) throw new Error('pid-release retry fixture requires FLEETDECK_HOME');
const pidFile = path.join(home, 'fleetd.pid');

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

const listenerCounts = () => ({
  exit: process.listenerCount('exit'),
  sigint: process.listenerCount('SIGINT'),
  sigterm: process.listenerCount('SIGTERM'),
  unhandledRejection: process.listenerCount('unhandledRejection'),
});

const listenersBefore = listenerCounts();
const rootScope = Scope.makeUnsafe('sequential');
const runTestPromise = Effect.runPromiseWith(Context.empty());
const ingress = (await runTestPromise(
  makeIngressSupervisor(Context.make(ProcessRunner, processRunner), rootScope),
)) as RootIngressSupervisorService;
let acquired: Awaited<ReturnType<typeof acquireDaemonResources>> | null = null;
const prepared = await runTestPromise(
  prepareBackgroundOwner({
    name: 'pid-release-retry-fixture',
    run: () =>
      acquired?.backgroundProgram ??
      Effect.die(new Error('background program started before daemon acquisition')),
  }),
);
acquired = await acquireDaemonResources(new AbortController().signal, ingress, {
  config: {
    home,
    port: Number(process.env['FLEETDECK_PORT']),
    version: 'pid-release-retry-fixture',
  },
  background: prepared.service,
  backgroundController: prepared.controller,
});
const backgroundOwner = await runTestPromise(
  prepared.start.pipe(
    Effect.provideService(ProcessRunner, processRunner),
    Effect.provideService(Scope.Scope, rootScope),
  ),
);
acquired.resources.addProducer('effect-background', { close: backgroundOwner.close });
await runTestPromise(prepared.service.awaitReady);

const originalUnlinkSync = fs.unlinkSync;
const injected = Object.assign(new Error('injected pid unlink failure'), { code: 'EACCES' });
const failingUnlinkSync = ((target: Parameters<typeof fs.unlinkSync>[0]) => {
  if (path.resolve(String(target)) === path.resolve(pidFile)) throw injected;
  return originalUnlinkSync(target);
}) as typeof fs.unlinkSync;

Object.defineProperty(fs, 'unlinkSync', {
  configurable: true,
  writable: true,
  value: failingUnlinkSync,
});

let fallbackError: unknown = null;
try {
  await acquired.resources.close();
  try {
    acquired.releaseProcessAtHostExit();
  } catch (error) {
    fallbackError = error;
  }
} finally {
  Object.defineProperty(fs, 'unlinkSync', {
    configurable: true,
    writable: true,
    value: originalUnlinkSync,
  });
}

const pidRetainedAfterFailure = existsSync(pidFile);
const exitListenerDeltaAfterFailure = process.listenerCount('exit') - listenersBefore.exit;
acquired.releaseProcessAtHostExit();
const pidRemovedAfterRetry = !existsSync(pidFile);
await runTestPromise(Scope.close(rootScope, Exit.void));

const listenersAfter = listenerCounts();
console.log(
  `PID_RELEASE_OBSERVATION ${JSON.stringify({
    closeErrors: acquired.resources.closeErrors.map(({ name }) => name),
    fallbackErrorIsInjected: fallbackError === injected,
    pidRetainedAfterFailure,
    exitListenerDeltaAfterFailure,
    pidRemovedAfterRetry,
    ingressState: ingress.state,
    listenerDelta: {
      exit: listenersAfter.exit - listenersBefore.exit,
      sigint: listenersAfter.sigint - listenersBefore.sigint,
      sigterm: listenersAfter.sigterm - listenersBefore.sigterm,
      unhandledRejection: listenersAfter.unhandledRejection - listenersBefore.unhandledRejection,
    },
  })}`,
);
