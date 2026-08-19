import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import {
  type ProcessError,
  ProcessNonZeroExitError,
  ProcessOutputLimitError,
  ProcessSpawnError,
  ProcessTimeoutError,
} from '../errors.ts';
import {
  type BoundedProcessRequest,
  type BoundedProcessResult,
  type ProcessFailure,
  type ProcessRequest,
  type ProcessResult,
  type ProcessSuccess,
  ProcessRunner,
  type ProcessRunnerService,
} from './process-runner.ts';

export const PROCESS_DRIVER_DEFAULT_TIMEOUT_MS = 30_000;
export const PROCESS_DRIVER_KILL_GRACE_MS = 1_000;
export const PROCESS_DRIVER_MAX_OUTPUT_BYTES = 1024 * 1024;

/**
 * One published subprocess attempt has two completion channels by design.
 * `decision` preserves execFileP's immediate wall-clock result, while `cleanup`
 * is the ownership proof that listeners, streams, timers, and the child have
 * been released. Both promises are non-rejecting for a conforming driver.
 */
export interface ProcessExecution<Decision = ProcessResult> {
  readonly decision: Promise<Decision>;
  readonly cleanup: Promise<void>;
  /** Idempotently choose cancellation (if undecided) and begin TERM/KILL cleanup. */
  cancel(): void;
}

/**
 * Low-level injectable boundary shared by the temporary Node reference and the
 * Bun-native implementation. `start` publishes its handle synchronously, even
 * when spawning fails synchronously. `close` is mandatory and idempotent: it
 * closes admission, cancels active executions, and joins every cleanup.
 */
export interface ProcessDriver {
  start(request: ProcessRequest): ProcessExecution;
  startBounded(request: BoundedProcessRequest): ProcessExecution<BoundedProcessResult>;
  close(): Promise<void>;
}

function effectFromExecution<Decision, Success, Error>(
  start: () => ProcessExecution<Decision>,
  interpret: (decision: Decision) => Effect.Effect<Success, Error>,
): Effect.Effect<Success, Error> {
  return Effect.callback<Success, Error>((resume) => {
    const execution = start();
    void execution.decision.then(
      (decision) => resume(interpret(decision)),
      (defect) => resume(Effect.die(defect)),
    );

    return Effect.promise(() => {
      execution.cancel();
      return execution.cleanup;
    });
  });
}

function processFailureEffect(
  request: ProcessRequest,
  result: ProcessFailure,
): Effect.Effect<never, ProcessError> {
  const details = { message: result.err, result };
  switch (result.code) {
    case 'ECANCELED':
      // Cancellation is ownership, not an expected application failure.
      return Effect.interrupt;
    case 'ETIMEDOUT':
      return Effect.fail(
        new ProcessTimeoutError({
          ...details,
          timeoutMs: request.timeoutMs ?? PROCESS_DRIVER_DEFAULT_TIMEOUT_MS,
        }),
      );
    case 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER':
      return Effect.fail(
        new ProcessOutputLimitError({
          ...details,
          maxOutputBytes: PROCESS_DRIVER_MAX_OUTPUT_BYTES,
        }),
      );
    default:
      return Effect.fail(
        typeof result.code === 'number' || result.code === null
          ? new ProcessNonZeroExitError({ ...details, exitCode: result.code })
          : new ProcessSpawnError(details),
      );
  }
}

function interpretProcessResult(
  request: ProcessRequest,
  result: ProcessResult,
): Effect.Effect<ProcessSuccess, ProcessError> {
  return result.ok ? Effect.succeed(result) : processFailureEffect(request, result);
}

/**
 * Lift one already-acquired driver into the application service. Interruption
 * waits for the driver's ownership channel instead of merely abandoning the
 * public decision Promise.
 */
export function makeProcessRunnerServiceFromDriver(driver: ProcessDriver): ProcessRunnerService {
  return {
    run(request) {
      return effectFromExecution(
        () => driver.start(request),
        (result) => interpretProcessResult(request, result),
      );
    },
    runBounded(request) {
      return effectFromExecution(
        () => driver.startBounded(request),
        (result) => Effect.succeed(result),
      );
    },
  };
}

/**
 * Rebuildable, definition-only Layer constructor. Driver acquisition happens
 * inside each Layer scope and its mandatory close is the release action.
 */
export function makeProcessRunnerLayerFromDriver<E, R>(
  acquireDriver: Effect.Effect<ProcessDriver, E, R>,
): Layer.Layer<ProcessRunner, E, R> {
  const acquireService = Effect.acquireRelease(acquireDriver, (driver) =>
    Effect.promise(() => driver.close()),
  ).pipe(Effect.map(makeProcessRunnerServiceFromDriver));

  return Layer.effect(ProcessRunner, acquireService);
}
