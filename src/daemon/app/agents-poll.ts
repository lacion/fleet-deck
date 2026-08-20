import * as Clock from 'effect/Clock';
import * as Data from 'effect/Data';
import * as Duration from 'effect/Duration';
import * as Effect from 'effect/Effect';
import type * as Exit from 'effect/Exit';
import type * as Fiber from 'effect/Fiber';
import * as Ref from 'effect/Ref';
import * as Schedule from 'effect/Schedule';

import { pidOwnedBy } from '../helpers.ts';
import { ProcessRunner, type ProcessRunnerService } from './services/process-runner.ts';

const DEFAULT_POLL_INTERVAL_MS = 10_000;
const DEFAULT_IDLE_POLL_INTERVAL_MS = 60_000;
const MINIMUM_ENV_POLL_INTERVAL_MS = 100;
const MAXIMUM_FIRST_RUN_DELAY_MS = 1_000;
const DEFAULT_ARGV = ['claude', 'agents', '--json'] as const;

export const AGENTS_POLL_EXEC_TIMEOUT_MS = 5_000;

export interface AgentsPollCore {
  readonly ingestAgentsPoll: (records: unknown) => void;
  readonly spawnLivenessTick?: () => void | Promise<void>;
}

export interface AgentsPollOptions {
  /** Captured when makeAgentsPollProgram is called; later environment changes are ignored. */
  readonly env?: Readonly<Record<string, string | undefined>>;
  readonly argv?: readonly string[] | null;
  readonly firstRunDelayMs?: number;
  readonly idlePollIntervalMs?: number;
  readonly pollIntervalMs?: number;
  /** Test seam for the shared agents-registry process ownership predicate. */
  readonly processOwnedBy?: (pid: number, startedAt: number) => boolean;
}

export interface ResolvedAgentsPollOptions {
  readonly argv: readonly [executable: string, ...arguments_: string[]] | null;
  readonly firstRunDelayMs: number;
  readonly idlePollIntervalMs: number;
  readonly pollIntervalMs: number;
  readonly processOwnedBy: (pid: number, startedAt: number) => boolean;
}

export class AgentsPollDecodeError extends Data.TaggedError('AgentsPollDecodeError')<{
  readonly cause: unknown;
}> {}

export class AgentsPollIngestError extends Data.TaggedError('AgentsPollIngestError')<{
  readonly cause: unknown;
}> {}

export class AgentsPollLivenessError extends Data.TaggedError('AgentsPollLivenessError')<{
  readonly cause: unknown;
}> {}

export type AgentsPollProgram = Effect.Effect<never, never, ProcessRunner>;

export interface AgentsPollOwner {
  /** Full fiber outcome remains observable so defects are not laundered into successful close. */
  readonly exit: Promise<Exit.Exit<never, never>>;
  /** Interrupts and joins the scheduler fiber. Repeated calls return one shared Promise. */
  readonly close: () => Promise<void>;
}

interface AgentsPollState {
  readonly agentsWereActive: boolean;
  readonly nextAgentsPollAt: number;
}

interface ValidPoll {
  readonly _tag: 'ValidPoll';
  readonly records: unknown;
}

interface SkippedPoll {
  readonly _tag: 'SkippedPoll';
}

type PollResult = ValidPoll | SkippedPoll;

const SKIPPED_POLL: SkippedPoll = { _tag: 'SkippedPoll' };

function resolveEnvArgv(
  env: Readonly<Record<string, string | undefined>>,
): readonly [executable: string, ...arguments_: string[]] | null {
  const override = env['FLEETDECK_AGENTS_CMD'];
  if (override === undefined) return DEFAULT_ARGV;
  const trimmed = override.trim();
  if (trimmed === '' || trimmed === 'false') return null;
  const [executable, ...arguments_] = trimmed.split(/\s+/);
  return executable === undefined ? null : [executable, ...arguments_];
}

function copyArgv(
  argv: readonly string[] | null,
): readonly [executable: string, ...arguments_: string[]] | null {
  if (argv === null) return null;
  const [executable, ...arguments_] = argv;
  return executable === undefined ? null : [executable, ...arguments_];
}

/** Resolve every environment/option decision once, before the returned Effect starts. */
export function resolveAgentsPollOptions(
  options: AgentsPollOptions = {},
): ResolvedAgentsPollOptions {
  const env = options.env ?? process.env;
  const envPollIntervalMs = Math.max(
    MINIMUM_ENV_POLL_INTERVAL_MS,
    Number(env['FLEETDECK_AGENTS_POLL_MS']) || DEFAULT_POLL_INTERVAL_MS,
  );
  const envIdlePollIntervalMs = Math.max(
    envPollIntervalMs,
    Number(env['FLEETDECK_AGENTS_IDLE_POLL_MS']) ||
      (env['FLEETDECK_AGENTS_POLL_MS'] ? envPollIntervalMs : DEFAULT_IDLE_POLL_INTERVAL_MS),
  );
  const envFirstRunDelayMs = Math.min(MAXIMUM_FIRST_RUN_DELAY_MS, envPollIntervalMs);
  const argv = options.argv === undefined ? resolveEnvArgv(env) : copyArgv(options.argv);

  return {
    argv,
    firstRunDelayMs: options.firstRunDelayMs ?? envFirstRunDelayMs,
    idlePollIntervalMs: options.idlePollIntervalMs ?? envIdlePollIntervalMs,
    pollIntervalMs: options.pollIntervalMs ?? envPollIntervalMs,
    processOwnedBy: options.processOwnedBy ?? pidOwnedBy,
  };
}

/** The active cadence trusts only owned, live interactive records. */
export function hasLiveInteractiveAgent(
  records: unknown,
  processOwnedBy: (pid: number, startedAt: number) => boolean = pidOwnedBy,
): boolean {
  if (!Array.isArray(records)) return false;
  return records.some((record) => {
    if (typeof record !== 'object' || record === null) return false;
    const candidate = record as { kind?: unknown; pid?: unknown; startedAt?: unknown };
    return (
      candidate.kind === 'interactive' &&
      typeof candidate.pid === 'number' &&
      processOwnedBy(
        candidate.pid,
        typeof candidate.startedAt === 'number' ? candidate.startedAt : Number.NaN,
      )
    );
  });
}

function decodePoll(out: string): Effect.Effect<ValidPoll, AgentsPollDecodeError> {
  return Effect.try({
    try: () => ({ _tag: 'ValidPoll' as const, records: JSON.parse(out) as unknown }),
    catch: (cause) => new AgentsPollDecodeError({ cause }),
  });
}

function runPoll(
  runner: ProcessRunnerService,
  argv: readonly [executable: string, ...arguments_: string[]],
): Effect.Effect<PollResult, never> {
  return runner.run({ argv, timeoutMs: AGENTS_POLL_EXEC_TIMEOUT_MS }).pipe(
    Effect.flatMap(({ out }) => decodePoll(out)),
    Effect.catchTags({
      AgentsPollDecodeError: () => Effect.succeed(SKIPPED_POLL),
      ProcessNonZeroExitError: () => Effect.succeed(SKIPPED_POLL),
      ProcessOutputLimitError: () => Effect.succeed(SKIPPED_POLL),
      ProcessSpawnError: () => Effect.succeed(SKIPPED_POLL),
      ProcessTimeoutError: () => Effect.succeed(SKIPPED_POLL),
    }),
  );
}

function ingestPoll(core: AgentsPollCore, records: unknown): Effect.Effect<void, never> {
  return Effect.try({
    try: () => core.ingestAgentsPoll(records),
    catch: (cause) => new AgentsPollIngestError({ cause }),
  }).pipe(Effect.catchTag('AgentsPollIngestError', () => Effect.void));
}

/**
 * Promise callback bridge whose interruption finalizer joins the admitted callback. The callback
 * itself has no cancellation protocol, but close cannot retire downstream resources while it is
 * still running, and a rejected operational callback remains a named fail-open skip.
 */
function runLiveness(core: AgentsPollCore): Effect.Effect<void, never> {
  const callback = core.spawnLivenessTick?.bind(core);
  if (callback === undefined) return Effect.void;

  const owned = Effect.callback<void, AgentsPollLivenessError>((resume) => {
    let completion: Promise<void>;
    try {
      completion = Promise.resolve(callback()).then(() => undefined);
    } catch (cause) {
      resume(Effect.fail(new AgentsPollLivenessError({ cause })));
      return;
    }

    const settled = completion.then(
      () => {
        resume(Effect.void);
      },
      (cause: unknown) => {
        resume(Effect.fail(new AgentsPollLivenessError({ cause })));
      },
    );
    return Effect.promise(() => settled);
  });

  return owned.pipe(Effect.catchTag('AgentsPollLivenessError', () => Effect.void));
}

function makeTick(
  core: AgentsPollCore,
  settings: ResolvedAgentsPollOptions,
  runner: ProcessRunnerService | null,
  state: Ref.Ref<AgentsPollState>,
): Effect.Effect<void> {
  return Effect.gen(function* () {
    if (settings.argv !== null && runner !== null) {
      const current = yield* Ref.get(state);
      const now = yield* Clock.currentTimeMillis;
      if (now >= current.nextAgentsPollAt) {
        const result = yield* runPoll(runner, settings.argv);
        let agentsWereActive = current.agentsWereActive;
        if (result._tag === 'ValidPoll') {
          yield* ingestPoll(core, result.records);
          // A verifier exception is an unexpected defect, not an operational polling error.
          agentsWereActive = yield* Effect.sync(() =>
            hasLiveInteractiveAgent(result.records, settings.processOwnedBy),
          );
        }
        const completedAt = yield* Clock.currentTimeMillis;
        yield* Ref.set(state, {
          agentsWereActive,
          nextAgentsPollAt:
            completedAt +
            (agentsWereActive ? settings.pollIntervalMs : settings.idlePollIntervalMs),
        });
      }
    }

    yield* runLiveness(core);
  });
}

/**
 * Infinite, scoped agents scheduler. It starts after the legacy first-run delay, polls serially,
 * then spaces every next liveness tick from completion of the prior tick.
 */
export function makeAgentsPollProgram(
  core: AgentsPollCore,
  options: AgentsPollOptions = {},
): AgentsPollProgram {
  const settings = resolveAgentsPollOptions(options);

  return Effect.gen(function* () {
    let runner: ProcessRunnerService | null = null;
    if (settings.argv !== null) runner = yield* ProcessRunner;
    const state = yield* Ref.make<AgentsPollState>({
      agentsWereActive: false,
      nextAgentsPollAt: 0,
    });
    const tick = makeTick(core, settings, runner, state);

    yield* Effect.sleep(Duration.millis(settings.firstRunDelayMs));
    yield* Effect.repeat(tick, Schedule.spaced(Duration.millis(settings.pollIntervalMs)));
    return yield* Effect.never;
  });
}

/** Wrap an already-forked scheduler before registering it in DaemonResources. */
export function makeAgentsPollOwner(fiber: Fiber.Fiber<never, never>): AgentsPollOwner {
  const exit = new Promise<Exit.Exit<never, never>>((resolve) => {
    fiber.addObserver(resolve);
  });
  let closePromise: Promise<void> | null = null;

  return {
    exit,
    close(): Promise<void> {
      if (closePromise) return closePromise;
      closePromise = exit.then(() => undefined);
      fiber.interruptUnsafe();
      return closePromise;
    },
  };
}
