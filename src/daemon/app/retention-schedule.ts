import * as Clock from 'effect/Clock';
import * as Deferred from 'effect/Deferred';
import * as Duration from 'effect/Duration';
import * as Effect from 'effect/Effect';

import { ownedLegacyPromise } from './background-owner.ts';
import { fixedGridNoCatchUp, nextFixedGridDelayMs } from './fixed-grid-schedule.ts';
import { BackgroundOperationalError } from './services/background.ts';

export const RETENTION_SCHEDULE_INTERVAL_MS = 10 * 60 * 1_000;
export const RETENTION_EVENT_WINDOW_MS = 24 * 60 * 60 * 1_000;

export type RetentionPhase = 'boot' | 'periodic';
export type RetentionOperation = 'prune-events' | 'retention-sweep';

export interface RetentionOperationalFailure {
  readonly phase: RetentionPhase;
  readonly operation: RetentionOperation;
  readonly error: BackgroundOperationalError;
}

export interface RetentionWork<Environment> {
  readonly pruneEvents: (
    cutoffMs: number,
  ) => Effect.Effect<void, BackgroundOperationalError, Environment>;
  readonly retentionSweep: (
    nowMs: number,
  ) => Effect.Effect<unknown, BackgroundOperationalError, Environment>;
}

export interface RetentionScheduleOptions<Environment> extends RetentionWork<Environment> {
  readonly interval?: Duration.Input;
  readonly eventWindowMs?: number;
  /** The integration keeps boot failures logged and periodic hygiene failures silent. */
  readonly onOperationalFailure?: (
    failure: RetentionOperationalFailure,
  ) => Effect.Effect<void, never, Environment>;
}

export interface RetentionSchedule<Environment> {
  /** Settles after the immediate sweep, including its named degraded-failure path. */
  readonly awaitFirstRun: Effect.Effect<void>;
  /** Daemon-long program; fork it under the shared background owner/root Scope. */
  readonly program: Effect.Effect<never, never, Environment>;
}

export interface LegacyRetentionCallbacks {
  readonly pruneEvents: (cutoffMs: number) => void;
  readonly retentionSweep: (nowMs: number) => PromiseLike<unknown>;
}

interface ResolvedRetentionScheduleOptions<Environment> extends RetentionWork<Environment> {
  readonly interval: Duration.Duration;
  readonly intervalMs: number;
  readonly eventWindowMs: number;
  readonly onOperationalFailure:
    | ((failure: RetentionOperationalFailure) => Effect.Effect<void, never, Environment>)
    | undefined;
}

function operationalError(
  operation: RetentionOperation,
  cause: unknown,
): BackgroundOperationalError {
  return new BackgroundOperationalError({
    operation: `retention-${operation}`,
    cause,
    message: `fleetd retention ${operation} failed`,
  });
}

/**
 * Temporary adapter for createCore's existing synchronous prune and Promise sweep seams. Promise
 * interruption joins the admitted sweep; a synchronous Promise-factory throw remains a defect.
 */
export function legacyRetentionWork(callbacks: LegacyRetentionCallbacks): RetentionWork<never> {
  const pruneEvents = callbacks.pruneEvents.bind(callbacks);
  const retentionSweep = callbacks.retentionSweep.bind(callbacks);
  return {
    pruneEvents: (cutoffMs) =>
      Effect.try({
        try: () => pruneEvents(cutoffMs),
        catch: (cause) => operationalError('prune-events', cause),
      }),
    retentionSweep: (nowMs) =>
      ownedLegacyPromise({
        try: () => retentionSweep(nowMs),
        catch: (cause) => operationalError('retention-sweep', cause),
      }),
  };
}

function resolveOptions<Environment>(
  options: RetentionScheduleOptions<Environment>,
): ResolvedRetentionScheduleOptions<Environment> {
  const interval = Duration.fromInputUnsafe(
    options.interval ?? Duration.millis(RETENTION_SCHEDULE_INTERVAL_MS),
  );
  const intervalMs = Duration.toMillis(interval);
  if (!Number.isFinite(intervalMs) || intervalMs <= 0) {
    throw new RangeError('retention interval must be a positive finite duration');
  }

  const eventWindowMs = options.eventWindowMs ?? RETENTION_EVENT_WINDOW_MS;
  if (!Number.isFinite(eventWindowMs) || eventWindowMs <= 0) {
    throw new RangeError('retention event window must be a positive finite number');
  }

  return {
    interval,
    intervalMs,
    eventWindowMs,
    pruneEvents: options.pruneEvents,
    retentionSweep: options.retentionSweep,
    onOperationalFailure: options.onOperationalFailure,
  };
}

function reportOperationalFailure<Environment>(
  options: ResolvedRetentionScheduleOptions<Environment>,
  phase: RetentionPhase,
  operation: RetentionOperation,
  error: BackgroundOperationalError,
): Effect.Effect<void, never, Environment> {
  const observer = options.onOperationalFailure;
  return observer ? Effect.suspend(() => observer({ phase, operation, error })) : Effect.void;
}

function runOperation<Environment>(
  options: ResolvedRetentionScheduleOptions<Environment>,
  phase: RetentionPhase,
  operation: RetentionOperation,
  work: () => Effect.Effect<unknown, BackgroundOperationalError, Environment>,
): Effect.Effect<void, never, Environment> {
  return Effect.suspend(work).pipe(
    Effect.asVoid,
    Effect.catchTag('BackgroundOperationalError', (error) =>
      reportOperationalFailure(options, phase, operation, error),
    ),
  );
}

function bootAttempt<Environment>(
  options: ResolvedRetentionScheduleOptions<Environment>,
): Effect.Effect<void, never, Environment> {
  return Effect.flatMap(Clock.currentTimeMillis, (nowMs) =>
    runOperation(options, 'boot', 'retention-sweep', () => options.retentionSweep(nowMs)),
  );
}

function periodicAttempt<Environment>(
  options: ResolvedRetentionScheduleOptions<Environment>,
): Effect.Effect<void, never, Environment> {
  return Effect.gen(function* () {
    const nowMs = yield* Clock.currentTimeMillis;
    yield* runOperation(options, 'periodic', 'prune-events', () =>
      options.pruneEvents(nowMs - options.eventWindowMs),
    );
    yield* runOperation(options, 'periodic', 'retention-sweep', () =>
      options.retentionSweep(nowMs),
    );
  });
}

/**
 * Construct one immediate-boot/fixed-grid retention program. All options are copied and validated
 * before this allocation Effect is returned, so later mutation cannot alter a running scheduler.
 */
export function makeRetentionSchedule<Environment>(
  options: RetentionScheduleOptions<Environment>,
): Effect.Effect<RetentionSchedule<Environment>> {
  const resolved = resolveOptions(options);

  return Effect.gen(function* () {
    const firstRun = yield* Deferred.make<void>();
    const program: Effect.Effect<never, never, Environment> = Effect.gen(function* () {
      const anchorMs = yield* Clock.currentTimeMillis;
      yield* bootAttempt(resolved);
      yield* Deferred.succeed(firstRun, undefined);

      const bootCompletedAt = yield* Clock.currentTimeMillis;
      const firstPeriodicDelayMs = nextFixedGridDelayMs(
        anchorMs,
        bootCompletedAt,
        resolved.intervalMs,
      );
      yield* Effect.sleep(Duration.millis(firstPeriodicDelayMs));
      yield* Effect.repeat(
        periodicAttempt(resolved),
        fixedGridNoCatchUp(resolved.interval, anchorMs),
      );
      return yield* Effect.never;
    }).pipe(
      // If a defect/interruption wins before the boot attempt settles, readiness observes the
      // same Cause instead of hanging. After success this is an idempotent no-op.
      Effect.onExit((exit) => Deferred.done(firstRun, exit).pipe(Effect.asVoid)),
    );

    return {
      awaitFirstRun: Deferred.await(firstRun),
      program,
    };
  });
}
