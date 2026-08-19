import * as Data from 'effect/Data';
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

export type StartupError = StartupConfigurationError | ProcessRunnerStartupError;

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
