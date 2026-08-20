import { existsSync } from 'node:fs';
import path from 'node:path';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Runtime from 'effect/Runtime';
import * as Scope from 'effect/Scope';
import {
  acquireDaemonResources,
  type AcquiredDaemonResources,
} from '../../../src/daemon/app/program.ts';
import type { RootIngressSupervisorService } from '../../../src/daemon/app/services/ingress-supervisor.ts';
import {
  ProcessRunner,
  type ProcessRunnerService,
} from '../../../src/daemon/app/services/process-runner.ts';
import { execFileP } from '../../../src/daemon/exec.ts';
import { makeIngressSupervisor } from '../../../src/daemon/platform/bun/ingress-supervisor-live.ts';

type Mode = 'success' | 'abort' | 'bind-failure';

const mode = process.argv[2] as Mode | undefined;
if (mode !== 'success' && mode !== 'abort' && mode !== 'bind-failure') {
  throw new Error('daemon acquisition fixture requires success, abort, or bind-failure');
}

const home = process.env['FLEETDECK_HOME'];
const port = Number(process.env['FLEETDECK_PORT']);
if (!home || !Number.isInteger(port) || port <= 0) {
  throw new Error('daemon acquisition fixture requires FLEETDECK_HOME and FLEETDECK_PORT');
}

const listenerCounts = () => ({
  sigint: process.listenerCount('SIGINT'),
  sigterm: process.listenerCount('SIGTERM'),
  unhandledRejection: process.listenerCount('unhandledRejection'),
});

let processRuns = 0;
const processRunner: ProcessRunnerService = {
  run: () => {
    processRuns++;
    return Effect.succeed({ ok: true, out: '' });
  },
  runBounded: () => {
    processRuns++;
    return Effect.succeed({
      code: 0,
      stdout: Buffer.alloc(0),
      stderr: '',
      truncated: false,
      timedOut: false,
    });
  },
};

const rootScope = Scope.makeUnsafe('sequential');
const context = Context.make(ProcessRunner, processRunner);
const runTestPromise = Effect.runPromiseWith(Context.empty());
const ingress = (await runTestPromise(
  makeIngressSupervisor(context, rootScope),
)) as RootIngressSupervisorService;
const controller = new AbortController();
const listenersBefore = listenerCounts();
const blocker =
  mode === 'bind-failure'
    ? Bun.serve({
        hostname: '127.0.0.1',
        port,
        fetch: () => new Response('occupied'),
      })
    : null;

let acquired: AcquiredDaemonResources | null = null;
let healthStatus: number | null = null;
let startupError: unknown = null;
try {
  const acquiring = acquireDaemonResources(controller.signal, ingress);
  if (mode === 'abort') controller.abort();
  acquired = await acquiring;
  healthStatus = (await fetch(`http://127.0.0.1:${port}/health`)).status;
  await acquired.readiness;
} catch (error) {
  startupError = error;
} finally {
  await acquired?.resources.close();
  if (blocker) await blocker.stop(true);
  await runTestPromise(Scope.close(rootScope, Exit.void));
}

const facadeUnbound = await execFileP(process.execPath, ['-e', '0']).then(
  () => false,
  (error: unknown) =>
    error instanceof Error && error.message === 'execFileP process runtime is not bound',
);
const listenersAfter = listenerCounts();
const errorRecord =
  startupError !== null && typeof startupError === 'object'
    ? (startupError as Record<string, unknown>)
    : null;
const causeRecord =
  errorRecord?.['cause'] !== null && typeof errorRecord?.['cause'] === 'object'
    ? (errorRecord['cause'] as Record<string, unknown>)
    : null;

console.log(
  `ACQUISITION_OBSERVATION ${JSON.stringify({
    mode,
    acquired: acquired !== null,
    healthStatus,
    processRuns,
    ingressState: ingress.state,
    shutdownExitCode: acquired?.shutdownExitCode() ?? null,
    facadeUnbound,
    homeExists: existsSync(home),
    pidExists: existsSync(path.join(home, 'fleetd.pid')),
    databaseExists: existsSync(path.join(home, 'fleetd.db')),
    listenerDelta: {
      sigint: listenersAfter.sigint - listenersBefore.sigint,
      sigterm: listenersAfter.sigterm - listenersBefore.sigterm,
      unhandledRejection: listenersAfter.unhandledRejection - listenersBefore.unhandledRejection,
    },
    errorTag: typeof errorRecord?.['_tag'] === 'string' ? errorRecord['_tag'] : null,
    errorName: startupError instanceof Error ? startupError.name : null,
    errorMessage: startupError instanceof Error ? startupError.message : null,
    errorReason: typeof errorRecord?.['reason'] === 'string' ? errorRecord['reason'] : null,
    errorCode: typeof errorRecord?.['code'] === 'string' ? errorRecord['code'] : null,
    causeCode: typeof causeRecord?.['code'] === 'string' ? causeRecord['code'] : null,
    runtimeExitCode: startupError === null ? null : Runtime.getErrorExitCode(startupError),
  })}`,
);
