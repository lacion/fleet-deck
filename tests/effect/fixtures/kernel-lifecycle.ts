import * as BunRuntime from '@effect/platform-bun/BunRuntime';
import * as Effect from 'effect/Effect';

const mode = process.argv[2];

if (mode === 'natural-exit') {
  BunRuntime.runMain(
    Effect.sync(() => {
      process.stdout.write('natural-complete\n');
    }),
    { disableErrorReporting: true },
  );
} else if (mode === 'root-keepalive') {
  BunRuntime.runMain(
    Effect.sync(() => {
      process.stdout.write('root-ready\n');
    }).pipe(Effect.andThen(Effect.never)),
    { disableErrorReporting: true },
  );
} else {
  process.stderr.write(`unknown kernel lifecycle mode: ${String(mode)}\n`);
  process.exitCode = 2;
}
