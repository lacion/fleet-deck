import * as Context from 'effect/Context';
import * as Data from 'effect/Data';
import type * as Effect from 'effect/Effect';

/** Existing /health vocabulary; the migration must not change the wire value. */
export type ReconciliationStatus = 'reconciling' | 'settled';

/**
 * A named, expected failure from one supervised background operation.
 *
 * Schedulers that are intentionally fail-open catch this error inside their
 * repeated action. An error that escapes the background program is therefore a
 * root-observable operational failure, while thrown values and Effect defects
 * remain defects.
 */
export class BackgroundOperationalError extends Data.TaggedError('BackgroundOperationalError')<{
  readonly operation: string;
  readonly message: string;
  readonly cause: unknown;
}> {}

/** A daemon-long background program returned normally instead of staying owned. */
export class BackgroundUnexpectedExitError extends Data.TaggedError(
  'BackgroundUnexpectedExitError',
)<{
  readonly owner: string;
  readonly message: string;
}> {}

/** A background fiber interrupted itself without its imperative owner asking it to stop. */
export class BackgroundUnexpectedInterruptionError extends Data.TaggedError(
  'BackgroundUnexpectedInterruptionError',
)<{
  readonly owner: string;
  readonly message: string;
}> {}

export interface BackgroundService {
  /** Cheap synchronous status used by the existing /health adapter. */
  readonly reconciliationStatus: () => ReconciliationStatus;
  /** Completes only after reconciliation, retention, and the trailing broadcast flush. */
  readonly awaitReady: Effect.Effect<void, BackgroundOperationalError>;
  /**
   * Never succeeds. Named failures stay typed; defects and an unexpected
   * daemon-long success remain in the Cause's defect channel.
   */
  readonly awaitFailure: Effect.Effect<never, BackgroundOperationalError>;
}

/** Definition-only value service; the P5 root integration supplies its one live instance. */
export class Background extends Context.Service<Background, BackgroundService>()(
  'fleetdeck/daemon/app/Background',
) {}
