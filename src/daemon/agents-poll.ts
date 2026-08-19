// agents-poll.ts — secondary session source (handoff F1): polls
// `claude agents --json` to catch sessions that predate plugin install (no
// hooks ever fired for them — e.g. a session started before the fleetdeck
// plugin was installed, or a background/interactive session Claude Code
// itself is tracking that this daemon never got hook telemetry for).
//
// Hook-derived state always wins — that precedence rule lives entirely in
// derive.mjs (the source flip in applyEvent, the merge logic in
// ingestAgentsPoll). This module only owns the polling cadence and process
// execution:
//   - runs every ~10s while agents are reported, backs off while the fleet is
//     idle, and first runs shortly after the daemon starts listening
//   - 5s exec timeout; ANY failure (CLI absent, timeout, non-zero exit,
//     unparseable output) is a silent skip — it must never crash the daemon
//   - overridable via FLEETDECK_AGENTS_CMD (tests inject fixture output this
//     way): the value is split on whitespace and run WITHOUT a shell, so
//     quotes, pipes, $(), and redirection are never interpreted — every token
//     arrives as a literal argv byte; wrap any shell pipeline in an executable
//     script and point the var at that. A 'false' or blank value disables the
//     CLI half of the poll entirely.

import { execFileP } from './exec.ts';
import { pidOwnedBy } from './helpers.ts';

// The slice of a derive.mjs createCore() instance this poller drives: the
// agents-cli merge step and the owned-pane liveness sweep (the latter optional
// so a core without spawns still type-checks). Structural on purpose.
interface AgentsPollCore {
  ingestAgentsPoll: (records: unknown) => void;
  spawnLivenessTick?: () => void | Promise<void>;
}

// FLEETDECK_AGENTS_POLL_MS: test hook to shrink the cadence (floor 100 ms);
// production default ~10 s.
const POLL_INTERVAL_MS = Math.max(100, Number(process.env['FLEETDECK_AGENTS_POLL_MS']) || 10_000);
// Keep an explicit cadence override authoritative for test/dev workflows. In
// production, an empty agents registry is checked once a minute rather than
// paying to launch a CLI every ten seconds forever.
const IDLE_POLL_INTERVAL_MS = Math.max(
  POLL_INTERVAL_MS,
  Number(process.env['FLEETDECK_AGENTS_IDLE_POLL_MS']) ||
    (process.env['FLEETDECK_AGENTS_POLL_MS'] ? POLL_INTERVAL_MS : 60_000),
);
const FIRST_RUN_DELAY_MS = Math.min(1_000, POLL_INTERVAL_MS); // "shortly after listen"
const EXEC_TIMEOUT_MS = 5_000;
const DEFAULT_ARGV = ['claude', 'agents', '--json'];

// Resolve FLEETDECK_AGENTS_CMD to an argv array (never a shell string). Unset →
// the default CLI; a blank or 'false' value → null, which disables the CLI half
// of the poll; anything else is tokenized on runs of whitespace into argv.
function resolveArgv(): string[] | null {
  const override = process.env['FLEETDECK_AGENTS_CMD'];
  if (override === undefined) return DEFAULT_ARGV;
  const trimmed = override.trim();
  if (trimmed === '' || trimmed === 'false') return null;
  return trimmed.split(/\s+/);
}

async function runOnce(argv: readonly string[]): Promise<string | null> {
  // execFileP runs by argv (no shell), absorbs synchronous throws, and applies
  // windowsHide itself — so a bad command yields { ok: false } rather than
  // escaping. ANY failure (absent CLI / timeout / non-zero exit) → null skip.
  const cmd = argv[0];
  if (cmd === undefined) return null; // resolveArgv never yields an empty argv, but the type allows it
  const res = await execFileP(cmd, argv.slice(1), { timeout: EXEC_TIMEOUT_MS });
  return res.ok ? res.out : null;
}

export interface AgentsPollOwner {
  stop: () => Promise<void>;
}

export interface AgentsPollOptions {
  argv?: readonly string[] | null;
  firstRunDelayMs?: number;
  idlePollIntervalMs?: number;
  pollIntervalMs?: number;
  runAgents?: (argv: readonly string[]) => Promise<string | null>;
}

function hasLiveInteractive(records: unknown): boolean {
  if (!Array.isArray(records)) return false;
  // Cadence must use the same trust boundary as derive.mjs ingestion — the
  // SAME ownership verifier, not a second pid-existence check: the CLI
  // registry retains background agents, dead processes, and pids the OS has
  // since handed to unrelated processes; treating any of those ghosts as
  // fleet activity would defeat idle backoff on precisely the machines where
  // the registry is noisiest.
  return records.some((rec) => {
    if (typeof rec !== 'object' || rec === null) return false;
    const r = rec as { kind?: unknown; pid?: unknown; startedAt?: unknown };
    return (
      r.kind === 'interactive' &&
      typeof r.pid === 'number' &&
      pidOwnedBy(r.pid, typeof r.startedAt === 'number' ? r.startedAt : NaN)
    );
  });
}

/**
 * Start the agents-cli poller against a running core (a derive.mjs
 * createCore() instance). stop() is idempotent: it clears the pending timer,
 * prevents post-quiesce callbacks, and joins the current tick. It deliberately
 * does not cancel an in-flight subprocess; that belongs to the P3 process seam.
 *
 * v1.2: the owned-pane liveness sweep (CONTRACT "Owned-pane liveness", ~10 s)
 * rides this same cadence — so the timers now ALWAYS run; disabling the
 * agents CLI via FLEETDECK_AGENTS_CMD=false only skips the CLI half of the
 * tick, never the pane sweep (a fleet with spawned panes but no agents CLI
 * still needs crash detection). The sweep is a cheap no-op when there are no
 * active spawn rows.
 */
export function startAgentsPoll(
  core: AgentsPollCore,
  options: AgentsPollOptions = {},
): AgentsPollOwner {
  const argv = options.argv === undefined ? resolveArgv() : options.argv;
  const agentsEnabled = argv !== null;
  const firstRunDelayMs = options.firstRunDelayMs ?? FIRST_RUN_DELAY_MS;
  const pollIntervalMs = options.pollIntervalMs ?? POLL_INTERVAL_MS;
  const idlePollIntervalMs = options.idlePollIntervalMs ?? IDLE_POLL_INTERVAL_MS;
  const runAgents = options.runAgents ?? runOnce;
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let currentTick: Promise<void> | null = null;
  let stopPromise: Promise<void> | null = null;
  let nextAgentsPollAt = 0;
  let agentsWereActive = false;

  async function tick(): Promise<void> {
    if (stopped) return;
    if (agentsEnabled && Date.now() >= nextAgentsPollAt) {
      const out = await runAgents(argv);
      // P1 can join but cannot cancel the current exec. Once stop() begins,
      // discard its late result so shutdown cannot mutate core state afterward.
      if (stopped) return;
      let validPoll = false;
      let records: unknown;
      if (out != null) {
        // exec failed/timed out: silent skip
        try {
          records = JSON.parse(out);
          validPoll = true;
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
      // A valid empty registry is the strongest cheap signal that the fleet
      // is idle. On a transient CLI failure, retain the prior cadence: an
      // active fleet retries promptly, while an absent CLI does not burn CPU.
      if (validPoll) agentsWereActive = hasLiveInteractive(records);
      nextAgentsPollAt = Date.now() + (agentsWereActive ? pollIntervalMs : idlePollIntervalMs);
    }
    if (stopped) return;
    // v1.2 owned-pane liveness keeps its ~10 s contract even while the much
    // heavier agents CLI is backed off. Failures remain a silent retry.
    try {
      await core.spawnLivenessTick?.();
    } catch {
      /* tmux hiccups are a silent skip; next tick retries */
    }
  }

  function schedule(delayMs: number): void {
    if (stopped) return;
    timer = setTimeout(() => {
      timer = null;
      if (stopped || currentTick) return;
      // Register ownership before tick() executes so stop() can never miss a
      // tick whose first synchronous callback itself initiates shutdown.
      const active = Promise.resolve()
        .then(tick)
        .catch(() => {
          /* polling is best effort; the next scheduled tick retries */
        });
      currentTick = active;
      void active.then(() => {
        if (currentTick === active) currentTick = null;
        if (!stopped) schedule(pollIntervalMs);
      });
    }, delayMs);
    timer.unref();
  }
  schedule(firstRunDelayMs);

  return {
    stop(): Promise<void> {
      if (stopPromise) return stopPromise;
      stopped = true;
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      const active = currentTick;
      stopPromise = active ? active.then(() => undefined) : Promise.resolve();
      return stopPromise;
    },
  };
}
