import * as Data from 'effect/Data';

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

/** New application work was refused because the application is quiescing. */
export class ApplicationQuiescingError extends Data.TaggedError('ApplicationQuiescingError')<{
  readonly operation: string;
  readonly message: string;
}> {}

export type ApplicationError = ProcessRunnerUnavailableError | ApplicationQuiescingError;
