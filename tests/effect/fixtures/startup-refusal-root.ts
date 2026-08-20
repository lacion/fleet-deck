import { existsSync, writeSync } from 'node:fs';
import path from 'node:path';
import * as BunRuntime from '@effect/platform-bun/BunRuntime';
import { DaemonApp, daemonHostControl } from '../../../src/daemon/app/root-program.ts';

const home = process.env['FLEETDECK_HOME'];
if (!home) throw new Error('startup-refusal root fixture requires FLEETDECK_HOME');

process.once('exit', (code) => {
  writeSync(
    1,
    `STARTUP_EXIT_OBSERVATION ${JSON.stringify({
      code,
      homeExists: existsSync(home),
      pidExists: existsSync(path.join(home, 'fleetd.pid')),
      listeners: {
        sigint: process.listenerCount('SIGINT'),
        sigterm: process.listenerCount('SIGTERM'),
        unhandledRejection: process.listenerCount('unhandledRejection'),
      },
    })}\n`,
  );
});

BunRuntime.runMain(DaemonApp, {
  disableErrorReporting: true,
  teardown: daemonHostControl.teardown,
});
