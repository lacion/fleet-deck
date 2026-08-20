import { startAgentsPoll } from '../../src/daemon/agents-poll.ts';
import { startNetworkWatch } from '../../src/daemon/network-watch.ts';

type Mode = 'agents' | 'network';

const CLOSE_AFTER_MS = 80;
const OBSERVE_AFTER_CLOSE_MS = 40;

const mode = process.argv[2] as Mode | undefined;
if (mode !== 'agents' && mode !== 'network') {
  throw new Error('usage: poll-owner-natural-exit.ts agents|network');
}

let callbacks = 0;
const owner =
  mode === 'agents'
    ? startAgentsPoll(
        {
          ingestAgentsPoll() {
            throw new Error('the fixture disables the agents CLI');
          },
          spawnLivenessTick() {
            callbacks++;
          },
        },
        {
          argv: null,
          firstRunDelayMs: 5,
          idlePollIntervalMs: 5,
          pollIntervalMs: 5,
        },
      )
    : startNetworkWatch({
        intervalMs: 5,
        previousAddresses: () => ['192.0.2.10'],
        readAddresses: () => ['192.0.2.20'],
        onChange() {
          callbacks++;
        },
      });

const startedAt = performance.now();
process.stdout.write(`${JSON.stringify({ type: 'started', mode })}\n`);

// The control timer must not be the reason this fixture stays alive. With an
// unreferenced owner cadence, Bun exits before this callback and no report is
// emitted; a referenced owner cadence keeps the fixture alive until stop().
const closeControl = setTimeout(() => {
  void owner.stop().then(
    () => {
      const callbacksAtStop = callbacks;
      const closedAtMs = performance.now() - startedAt;

      // Keep the process around briefly after close solely to detect a late
      // owner callback. Once this observation fires, natural exit proves that
      // stop() cleared the owner's referenced timer.
      setTimeout(() => {
        process.stdout.write(
          `${JSON.stringify({
            type: 'closed',
            mode,
            callbacksAtStop,
            callbacksAfterObservation: callbacks,
            closedAtMs,
          })}\n`,
        );
      }, OBSERVE_AFTER_CLOSE_MS);
    },
    (error: unknown) => {
      const detail = error instanceof Error ? (error.stack ?? error.message) : String(error);
      process.stderr.write(`${detail}\n`);
      process.exitCode = 1;
    },
  );
}, CLOSE_AFTER_MS);
closeControl.unref();
