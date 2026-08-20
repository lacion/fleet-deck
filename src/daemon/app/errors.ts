import * as Data from 'effect/Data';
import * as Runtime from 'effect/Runtime';
import type { ProcessFailure } from './services/process-runner.ts';

/** A daemon setting could not be validated before resource acquisition. */
export class StartupConfigurationError extends Data.TaggedError('StartupConfigurationError')<{
  readonly setting: string;
  readonly message: string;
}> {}

/** The process service could not be constructed during application startup. */
export class ProcessRunnerStartupError extends Data.TaggedError('ProcessRunnerStartupError')<{
  readonly message: string;
}> {}

/**
 * An expected daemon preflight refused startup after synchronously attempting
 * to release its owned process state. The root interpreter is the only place
 * that emits `message`, preserving the historical one-line stderr contract
 * without terminating from inside resource acquisition.
 */
export class DaemonStartupRefusalError extends Data.TaggedError('DaemonStartupRefusalError')<{
  /** Refusal detail without the stable fleetd prefix. */
  readonly reason: string;
  /** Exact operator-facing line, excluding only the trailing newline. */
  readonly message: string;
  /** A synchronous pid-release failure retained without replacing the refusal. */
  readonly cleanupCause: unknown | null;
}> {
  override readonly [Runtime.errorExitCode] = 1;
}

/** The aggregate daemon owner could not be acquired or assembled for the root Layer. */
export class DaemonStartupError extends Data.TaggedError('DaemonStartupError')<{
  readonly message: string;
  readonly cause: unknown;
}> {}

/** The native HTTP listener could not be acquired during daemon startup. */
export class HttpBindStartupError extends Data.TaggedError('HttpBindStartupError')<{
  readonly reason: 'address-in-use' | 'closed' | 'other';
  readonly origin: 'bun-serve-throw' | 'lifecycle-guard';
  readonly code: string | null;
  readonly errno: string | number | null;
  readonly message: string;
  readonly cause: unknown;
}> {
  /** Preserve the daemon's established election-loser status; all other bind failures stay 1. */
  override readonly [Runtime.errorExitCode]: 1 | 3 = this.reason === 'address-in-use' ? 3 : 1;
}

export type StartupError =
  | StartupConfigurationError
  | ProcessRunnerStartupError
  | DaemonStartupRefusalError
  | DaemonStartupError
  | HttpBindStartupError;

/** The process service was present but could not accept an application request. */
export class ProcessRunnerUnavailableError extends Data.TaggedError(
  'ProcessRunnerUnavailableError',
)<{
  readonly message: string;
}> {}

/** The native process could not be started or its streams failed before an exit result. */
export class ProcessSpawnError extends Data.TaggedError('ProcessSpawnError')<{
  readonly message: string;
  /** Exact compatibility failure retained for the temporary Promise interpreter. */
  readonly result: ProcessFailure;
}> {}

/** The process exited without success, including signal exits represented by a null code. */
export class ProcessNonZeroExitError extends Data.TaggedError('ProcessNonZeroExitError')<{
  readonly message: string;
  readonly exitCode: number | null;
  /** Exact compatibility failure retained for the temporary Promise interpreter. */
  readonly result: ProcessFailure;
}> {}

/** The process exceeded its request deadline. */
export class ProcessTimeoutError extends Data.TaggedError('ProcessTimeoutError')<{
  readonly message: string;
  readonly timeoutMs: number;
  /** Exact compatibility failure retained for the temporary Promise interpreter. */
  readonly result: ProcessFailure;
}> {}

/** Combined stdout/stderr exceeded the application process budget. */
export class ProcessOutputLimitError extends Data.TaggedError('ProcessOutputLimitError')<{
  readonly message: string;
  readonly maxOutputBytes: number;
  /** Exact compatibility failure retained for the temporary Promise interpreter. */
  readonly result: ProcessFailure;
}> {}

export type ProcessError =
  | ProcessSpawnError
  | ProcessNonZeroExitError
  | ProcessTimeoutError
  | ProcessOutputLimitError;

/** New application work was refused because the application is quiescing. */
export class ApplicationQuiescingError extends Data.TaggedError('ApplicationQuiescingError')<{
  readonly operation: string;
  readonly message: string;
}> {}

export type ApplicationError =
  | ProcessRunnerUnavailableError
  | ProcessError
  | ApplicationQuiescingError;
