import { writeSync } from 'node:fs';
import * as BunRuntime from '@effect/platform-bun/BunRuntime';
import * as Effect from 'effect/Effect';
import type { DaemonRootStartupError } from '../../../src/daemon/app/live-layer.ts';
import { DaemonStartupRefusalError, HttpBindStartupError } from '../../../src/daemon/app/errors.ts';
import { DaemonHostControl } from '../../../src/daemon/app/host-control.ts';
import { withDaemonRootExitPolicy } from '../../../src/daemon/app/root-program.ts';

type Mode = 'startup-refusal' | 'bind-address-in-use' | 'defect' | 'signal' | 'finite';

const rawMode = process.argv[2];
if (
  rawMode !== 'startup-refusal' &&
  rawMode !== 'bind-address-in-use' &&
  rawMode !== 'defect' &&
  rawMode !== 'signal' &&
  rawMode !== 'finite'
) {
  process.stderr.write(`unknown P4 root exit-path mode: ${String(rawMode)}\n`);
  process.exit(2);
  throw new Error('unreachable after usage failure');
}
const mode: Mode = rawMode;
const hostControl = new DaemonHostControl();

process.once('exit', (code) => {
  writeSync(
    process.stdout.fd,
    `ROOT_EXIT_OBSERVATION ${JSON.stringify({
      mode,
      code,
      firstSignal: hostControl.firstSignal,
      listeners: {
        sigint: process.listenerCount('SIGINT'),
        sigterm: process.listenerCount('SIGTERM'),
      },
    })}\n`,
  );
});

function rootProgram(): Effect.Effect<void, DaemonRootStartupError> {
  switch (mode) {
    case 'startup-refusal':
      return Effect.fail(
        new DaemonStartupRefusalError({
          reason: 'root exit fixture refusal',
          message: 'fleetd refused to start: root exit fixture refusal',
          cleanupCause: null,
        }),
      );
    case 'bind-address-in-use': {
      const cause = Object.assign(new Error('fixture port is occupied'), {
        code: 'EADDRINUSE',
      });
      return Effect.fail(
        new HttpBindStartupError({
          reason: 'address-in-use',
          origin: 'bun-serve-throw',
          code: cause.code,
          errno: null,
          message: 'fleetd already running (root exit fixture)',
          cause,
        }),
      );
    }
    case 'defect': {
      // Build the marker at runtime so Bun's source-code excerpt cannot create
      // a false second occurrence beside the one reported Error message.
      const marker = ['P4', 'ROOT', 'DEFECT', 'SENTINEL'].join('_');
      return Effect.die(new Error(marker));
    }
    case 'signal':
      return Effect.sync(() => writeSync(process.stdout.fd, 'ROOT_EXIT_READY\n')).pipe(
        Effect.andThen(Effect.never),
      );
    case 'finite':
      // Let the platform runner finish installing its signal observers before
      // the main fiber completes, matching a real finite asynchronous root.
      return Effect.sleep('20 millis');
  }
}

BunRuntime.runMain(withDaemonRootExitPolicy(hostControl, rootProgram()), {
  disableErrorReporting: true,
  teardown: hostControl.teardown,
});
