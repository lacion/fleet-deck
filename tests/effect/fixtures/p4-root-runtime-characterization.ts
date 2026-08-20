import { writeSync } from 'node:fs';
import * as BunRuntime from '@effect/platform-bun/BunRuntime';
import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Runtime from 'effect/Runtime';

type Mode = 'natural-exit' | 'first-sigterm';

interface Observation {
  readonly event: string;
  readonly mode: Mode;
  readonly [key: string]: string | number | boolean;
}

const rawMode = process.argv[2];
if (rawMode !== 'natural-exit' && rawMode !== 'first-sigterm') {
  process.stderr.write(`unknown P4 root-runtime characterization mode: ${String(rawMode)}\n`);
  process.exit(2);
}
const mode: Mode = rawMode;

function observe(event: string, fields: Record<string, string | number | boolean> = {}): void {
  const observation: Observation = { event, mode, ...fields };
  // The SIGTERM path calls process.exit from the platform runner immediately
  // after teardown. A synchronous write keeps the characterization evidence
  // deterministic even when stdout is a pipe.
  writeSync(process.stdout.fd, `${JSON.stringify(observation)}\n`);
}

process.once('beforeExit', (code) => {
  observe('host-idle', { code });
});

const teardown: Runtime.Teardown = (exit, onExit) => {
  Runtime.defaultTeardown(exit, (defaultCode) => {
    const interruptionOnly = Exit.isFailure(exit) && Cause.hasInterruptsOnly(exit.cause);
    const exitCode = interruptionOnly ? 0 : defaultCode;
    observe('teardown', {
      defaultCode,
      exitCode,
      interruptionOnly,
      sigintListeners: process.listenerCount('SIGINT'),
      sigtermListeners: process.listenerCount('SIGTERM'),
    });
    onExit(exitCode);
  });
};

const program = Effect.acquireRelease(
  Effect.sync(() => {
    // This fixture-owned handle and Runtime.makeRunMain's internal keep-alive
    // must both be gone before a successful root can reach `beforeExit`.
    const ownerKeepAlive = setInterval(() => undefined, 2_147_483_647);
    observe('ready');
    return ownerKeepAlive;
  }),
  (ownerKeepAlive) =>
    Effect.gen(function* () {
      observe('cleanup-started');
      if (mode === 'first-sigterm') yield* Effect.sleep('40 millis');
      clearInterval(ownerKeepAlive);
      observe('cleanup-complete');
    }),
).pipe(
  Effect.andThen(mode === 'natural-exit' ? Effect.sleep('20 millis') : Effect.never),
  Effect.scoped,
);

BunRuntime.runMain(program, {
  disableErrorReporting: true,
  teardown,
});
