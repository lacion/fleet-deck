import { readFileSync, writeSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import * as BunRuntime from '@effect/platform-bun/BunRuntime';
import * as Effect from 'effect/Effect';
import { DaemonResources } from '../../../src/daemon/daemon-resources.ts';
import { makeDaemonResourceLifecycleOwner } from '../../../src/daemon/app/daemon-resource-lifecycle.ts';
import { LifecycleCoordinator } from '../../../src/daemon/app/lifecycle-coordinator.ts';
import { ProcessRunner } from '../../../src/daemon/app/services/process-runner.ts';
import { ProcessRuntimeControl } from '../../../src/daemon/app/services/process-runtime-control.ts';
import { ProcessRunnerLive } from '../../../src/daemon/platform/bun/process-runner-live.ts';

const ROOT_TIMEOUT_MS = 1_750;
const FORCE_RESERVE_MS = 250;

interface Observation {
  readonly event: string;
  readonly [key: string]: string | number | boolean;
}

function observe(event: string, fields: Record<string, string | number | boolean> = {}): void {
  const observation: Observation = { event, ...fields };
  writeSync(process.stdout.fd, `${JSON.stringify(observation)}\n`);
}

async function waitForPid(pidFile: string): Promise<number> {
  const deadline = Date.now() + 2_000;
  while (Date.now() < deadline) {
    try {
      const pid = Number(readFileSync(pidFile, 'utf8'));
      if (Number.isSafeInteger(pid) && pid > 1) return pid;
    } catch {
      // The TERM-immune descendant has not published its pid yet.
    }
    await Bun.sleep(10);
  }
  throw new Error('timed out waiting for TERM-immune process-group descendant');
}

const childFixture = fileURLToPath(new URL('./bun-process-child.ts', import.meta.url));
const pidFile = process.argv[2];
if (!pidFile) throw new Error('missing descendant pid file');

let startedAt = 0;
let driverSettledAt = -1;

const root = Effect.gen(function* () {
  const runner = yield* ProcessRunner;
  const processControl = yield* ProcessRuntimeControl;
  yield* Effect.forkChild(
    runner.run({
      argv: [process.execPath, childFixture, 'term-resistant-group-parent', pidFile],
      timeoutMs: 30_000,
      killTree: true,
    }),
  );
  const descendantPid = yield* Effect.promise(() => waitForPid(pidFile));
  observe('ready', { descendantPid, timeoutMs: ROOT_TIMEOUT_MS });

  const stuckProducer = new Promise<void>(() => undefined);
  const resources = new DaemonResources({
    producers: [{ name: 'stuck-producer', owner: { close: () => stuckProducer } }],
    processRuntime: {
      name: 'root-ingress-fixture',
      owner: {
        quiesce() {},
        interrupt() {},
        join() {},
        close() {},
      },
    },
    processDriver: {
      name: 'bun-process-driver',
      owner: processControl,
    },
  });
  const coordinator = new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(resources), {
    timeoutMs: ROOT_TIMEOUT_MS,
    forceReserveMs: FORCE_RESERVE_MS,
  });

  startedAt = Date.now();
  const watchForce = setInterval(() => {
    if (!coordinator.forced || driverSettledAt >= 0) return;
    void processControl.close().then(() => {
      driverSettledAt = Date.now() - startedAt;
      observe('driver-settled', { elapsedMs: driverSettledAt });
    });
    clearInterval(watchForce);
  }, 1);
  try {
    const outcome = yield* coordinator.close({ _tag: 'Interruption', signal: 'SIGTERM' });
    const elapsedMs = Date.now() - startedAt;
    const joinStartedAt = Date.now();
    yield* Effect.promise(() => processControl.close());
    observe('outcome', {
      deadlineExpired: outcome.deadlineExpired,
      elapsedMs,
      driverSettledAt,
      postDeadlineJoinMs: Date.now() - joinStartedAt,
    });
  } finally {
    clearInterval(watchForce);
  }
}).pipe(Effect.provide(ProcessRunnerLive));

BunRuntime.runMain(
  root.pipe(
    Effect.onExit(() =>
      Effect.sync(() => {
        observe('root-finalized', { elapsedMs: Date.now() - startedAt });
      }),
    ),
  ),
  { disableErrorReporting: true },
);
