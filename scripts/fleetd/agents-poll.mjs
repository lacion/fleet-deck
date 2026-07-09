// agents-poll.mjs — secondary session source (handoff F1): polls
// `claude agents --json` to catch sessions that predate plugin install (no
// hooks ever fired for them — e.g. a session started before the fleetdeck
// plugin was installed, or a background/interactive session Claude Code
// itself is tracking that this daemon never got hook telemetry for).
//
// Hook-derived state always wins — that precedence rule lives entirely in
// derive.mjs (the source flip in applyEvent, the merge logic in
// ingestAgentsPoll). This module only owns the polling cadence and process
// execution:
//   - runs every ~10s, first run shortly after the daemon starts listening
//   - 5s exec timeout; ANY failure (CLI absent, timeout, non-zero exit,
//     unparseable output) is a silent skip — it must never crash the daemon
//   - overridable via FLEETDECK_AGENTS_CMD (tests inject fixture output this
//     way; users can disable the poller entirely with FLEETDECK_AGENTS_CMD=false)

import { exec } from 'node:child_process';

// FLEETDECK_AGENTS_POLL_MS: test hook to shrink the cadence (floor 100 ms);
// production default ~10 s.
const POLL_INTERVAL_MS = Math.max(100, Number(process.env.FLEETDECK_AGENTS_POLL_MS) || 10_000);
const FIRST_RUN_DELAY_MS = Math.min(1_000, POLL_INTERVAL_MS); // "shortly after listen"
const EXEC_TIMEOUT_MS = 5_000;
const DEFAULT_CMD = 'claude agents --json';

function resolveCommand() {
  const override = process.env.FLEETDECK_AGENTS_CMD;
  return override === undefined ? DEFAULT_CMD : override;
}

function runOnce(cmd) {
  return new Promise((resolve) => {
    try {
      exec(cmd, { timeout: EXEC_TIMEOUT_MS, windowsHide: true }, (err, stdout) => {
        if (err) return resolve(null); // absent CLI / timeout / non-zero exit: silent skip
        resolve(stdout);
      });
    } catch {
      resolve(null); // e.g. exec throwing synchronously on a malformed command
    }
  });
}

/**
 * Start the agents-cli poller against a running core (a derive.mjs
 * createCore() instance). Returns { stop() } to clear the timers.
 *
 * v1.2: the owned-pane liveness sweep (CONTRACT "Owned-pane liveness", ~10 s)
 * rides this same cadence — so the timers now ALWAYS run; disabling the
 * agents CLI via FLEETDECK_AGENTS_CMD=false only skips the CLI half of the
 * tick, never the pane sweep (a fleet with spawned panes but no agents CLI
 * still needs crash detection). The sweep is a cheap no-op when there are no
 * active spawn rows.
 */
export function startAgentsPoll(core) {
  const cmd = resolveCommand();
  const agentsEnabled = !(cmd === 'false' || cmd.trim() === '');

  async function tick() {
    if (agentsEnabled) {
      const out = await runOnce(cmd);
      if (out != null) { // exec failed/timed out: silent skip
        let records;
        try {
          records = JSON.parse(out);
        } catch {
          records = undefined; // garbage output: silent skip
        }
        if (records !== undefined) {
          try {
            core.ingestAgentsPoll(records);
          } catch {
            /* a bad poll result must never take the daemon down */
          }
        }
      }
    }
    // v1.2 owned-pane liveness (spawns rows in spawning|live). Failures are
    // swallowed for the same reason as above: never take the daemon down.
    try {
      await core.spawnLivenessTick?.();
    } catch {
      /* tmux hiccups are a silent skip; next tick retries */
    }
  }

  const firstTimer = setTimeout(tick, FIRST_RUN_DELAY_MS);
  const interval = setInterval(tick, POLL_INTERVAL_MS);
  firstTimer.unref();
  interval.unref();

  return {
    stop() {
      clearTimeout(firstTimer);
      clearInterval(interval);
    },
  };
}
