import * as Cause from 'effect/Cause';
import type * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import type {
  ExecBoundedRequest,
  ExecBoundedResult,
  ExecFileDelegate,
  ExecRequest,
  ExecResult,
} from '../exec.ts';
import {
  ApplicationQuiescingError,
  type ProcessError,
  type ProcessRunnerUnavailableError,
} from './errors.ts';
import type { IngressSupervisorService } from './services/ingress-supervisor.ts';
import {
  type BoundedProcessResult,
  execBoundedEffect,
  execEffect,
  type ProcessSuccess,
} from './services/process-runner.ts';

const CANCELLED_RESULT: ExecResult = {
  ok: false,
  code: 'ECANCELED',
  err: 'cancelled',
};

const BOUNDED_CANCELLED_RESULT: ExecBoundedResult = {
  code: null,
  stdout: Buffer.alloc(0),
  stderr: 'cancelled',
  truncated: true,
  timedOut: false,
};

function submitExit<A, E, Services>(
  ingress: IngressSupervisorService<Services>,
  operation: string,
  effect: Effect.Effect<A, E, Services>,
): Promise<Exit.Exit<A, E | ApplicationQuiescingError>> {
  return new Promise((resolve, reject) => {
    try {
      // RC.110's Promise runner can reject outside Effect.exit when the
      // supervisor interrupts its fiber directly. The callback runner delivers
      // that same interruption as an Exit after its canceler has settled, which
      // lets this compatibility boundary retain canonical ECANCELED semantics.
      ingress.runCallback(operation, effect, { onExit: resolve });
    } catch (defect) {
      reject(defect);
    }
  });
}

function processExitResult(
  exit: Exit.Exit<ProcessSuccess, ProcessError | ApplicationQuiescingError>,
): ExecResult {
  if (Exit.isSuccess(exit)) return exit.value;

  // Defects are programming errors, not compatibility-shaped subprocess
  // failures. Keep their original identity for the daemon's unhandled-error
  // policy instead of laundering them through an `err` string.
  const defect = exit.cause.reasons.find(Cause.isDieReason);
  if (defect) throw defect.defect;

  const failure = exit.cause.reasons.find(Cause.isFailReason);
  if (failure) {
    return failure.error instanceof ApplicationQuiescingError
      ? CANCELLED_RESULT
      : failure.error.result;
  }

  if (Cause.hasInterruptsOnly(exit.cause)) return CANCELLED_RESULT;
  throw Cause.squash(exit.cause);
}

function boundedExitResult(
  exit: Exit.Exit<BoundedProcessResult, ProcessRunnerUnavailableError | ApplicationQuiescingError>,
): ExecBoundedResult {
  if (Exit.isSuccess(exit)) return exit.value;

  const defect = exit.cause.reasons.find(Cause.isDieReason);
  if (defect) throw defect.defect;

  const failure = exit.cause.reasons.find(Cause.isFailReason);
  if (failure) {
    if (failure.error instanceof ApplicationQuiescingError) return BOUNDED_CANCELLED_RESULT;
    return {
      code: null,
      stdout: Buffer.alloc(0),
      stderr: failure.error.message || failure.error._tag,
      truncated: false,
      timedOut: false,
    };
  }

  if (Cause.hasInterruptsOnly(exit.cause)) return BOUNDED_CANCELLED_RESULT;
  throw Cause.squash(exit.cause);
}

/**
 * Adapt the temporary Promise subprocess API to the one already-built root
 * Context. This value owns no runtime, Layer, Fiber, or listener: admission and
 * every submitted Effect belong to the root IngressSupervisor.
 */
export function makeIngressExecFileDelegate<Services>(
  ingress: IngressSupervisorService<Services>,
): ExecFileDelegate {
  const run = (request: ExecRequest): Promise<ExecResult> =>
    submitExit(
      ingress,
      'legacy-process.run',
      execEffect(request) as Effect.Effect<ProcessSuccess, ProcessError, Services>,
    ).then(processExitResult);

  const runBounded = (request: ExecBoundedRequest): Promise<ExecBoundedResult> =>
    submitExit(
      ingress,
      'legacy-process.run-bounded',
      execBoundedEffect(request) as Effect.Effect<
        BoundedProcessResult,
        ProcessRunnerUnavailableError,
        Services
      >,
    ).then(boundedExitResult);

  return { run, runBounded };
}
