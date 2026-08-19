import * as BunRuntime from '@effect/platform-bun/BunRuntime';
import * as Effect from 'effect/Effect';
import { kernelProbe } from '../../../src/daemon/app/kernel.ts';
import { makeLiveLayer } from '../../../src/daemon/app/live-layer.ts';
import { AppConfig } from '../../../src/daemon/app/services/app-config.ts';
import type { ProcessRunnerService } from '../../../src/daemon/app/services/process-runner.ts';

const layer = makeLiveLayer({
  config: {
    home: '/kernel-probe',
    port: 4711,
    version: 'p2-kernel-probe',
  },
  acquireProcessRunner: Effect.gen(function* () {
    const config = yield* AppConfig;
    return {
      run(request) {
        return Effect.succeed({
          ok: true as const,
          out: `${config.version}:${request.argv.join('\u0000')}`,
        });
      },
      runBounded: () =>
        Effect.succeed({
          code: 0,
          stdout: Buffer.alloc(0),
          stderr: '',
          truncated: false,
          timedOut: false,
        }),
    } satisfies ProcessRunnerService;
  }),
});

const program = kernelProbe({
  argv: ['fleetdeck-probe', '--direct-argv', 'value with spaces'],
}).pipe(
  Effect.provide(layer),
  Effect.tap((result) =>
    Effect.sync(() => {
      process.stdout.write(`${JSON.stringify(result)}\n`);
    }),
  ),
);

BunRuntime.runMain(program, { disableErrorReporting: true });
