import { existsSync } from 'node:fs';

const home = process.env['FLEETDECK_HOME'];
if (!home) throw new Error('fixture requires FLEETDECK_HOME');

const listenerCounts = () => ({
  sigint: process.listenerCount('SIGINT'),
  sigterm: process.listenerCount('SIGTERM'),
  unhandledRejection: process.listenerCount('unhandledRejection'),
});

const before = listenerCounts();
const program = await import('../../../src/daemon/app/program.ts');
const after = listenerCounts();

console.log(
  JSON.stringify({
    exports: Object.keys(program).sort(),
    homeExists: existsSync(home),
    homeEnvironment: process.env['FLEETDECK_HOME'],
    listenerDelta: {
      sigint: after.sigint - before.sigint,
      sigterm: after.sigterm - before.sigterm,
      unhandledRejection: after.unhandledRejection - before.unhandledRejection,
    },
  }),
);
