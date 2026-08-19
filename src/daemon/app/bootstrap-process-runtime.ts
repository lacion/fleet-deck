import * as Cause from 'effect/Cause';
import type * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import type * as Layer from 'effect/Layer';
import * as ManagedRuntime from 'effect/ManagedRuntime';
import type {
  ProcessError,
  ProcessRunnerStartupError,
  ProcessRunnerUnavailableError,
} from './errors.ts';
import { ProcessRunnerLive } from './live-layer.ts';
import {
  type BoundedProcessRequest,
  type BoundedProcessResult,
  execBoundedEffect,
  execEffect,
  type ProcessRequest,
  type ProcessResult,
  type ProcessSuccess,
  ProcessRunner,
} from './services/process-runner.ts';

const CANCELLED_RESULT: ProcessResult = {
  ok: false,
  code: 'ECANCELED',
  err: 'cancelled',
};

type BootstrapProcessError = ProcessRunnerStartupError | ProcessError;
type BootstrapBoundedError = ProcessRunnerStartupError | ProcessRunnerUnavailableError;
type BootstrapLayer = Layer.Layer<ProcessRunner, ProcessRunnerStartupError>;
type BootstrapRuntime = ManagedRuntime.ManagedRuntime<ProcessRunner, ProcessRunnerStartupError>;

interface TrackedRun {
  readonly controller: AbortController;
  readonly exit: Promise<unknown>;
}

export interface BootstrapProcessRuntimeBridge {
  /** Submit one compatibility request. Expected failures resolve; defects reject unchanged. */
  readonly run: (request: ProcessRequest) => Promise<ProcessResult>;
  readonly runBounded: (request: BoundedProcessRequest) => Promise<BoundedProcessResult>;
  /** Synchronously refuse new submissions and interrupt every admitted Effect. */
  readonly quiesce: () => void;
  /** Join submitted Effects, then dispose the sole ProcessRunner ManagedRuntime. */
  readonly close: () => Promise<void>;
}

function expectedFailure(
  error: ProcessRunnerStartupError | ProcessRunnerUnavailableError,
): ProcessResult {
  return {
    ok: false,
    err: error.message || error._tag,
  };
}

function exitResult(exit: Exit.Exit<ProcessSuccess, BootstrapProcessError>): ProcessResult {
  if (Exit.isSuccess(exit)) return exit.value;

  // A defect is a programming error, not a compatibility-shaped process
  // failure. Preserve its original identity for the existing unhandled-error
  // policy instead of laundering it through an `err` string.
  const defect = exit.cause.reasons.find(Cause.isDieReason);
  if (defect) throw defect.defect;

  const failure = exit.cause.reasons.find(Cause.isFailReason);
  if (failure) {
    return failure.error._tag === 'ProcessRunnerStartupError'
      ? expectedFailure(failure.error)
      : failure.error.result;
  }

  if (Cause.hasInterruptsOnly(exit.cause)) return CANCELLED_RESULT;

  // Empty or otherwise structurally impossible causes indicate an invariant
  // break in this bridge/RC pairing. Keep it visible and non-lossy.
  throw Cause.squash(exit.cause);
}

function boundedExitResult(
  exit: Exit.Exit<BoundedProcessResult, BootstrapBoundedError>,
): BoundedProcessResult {
  if (Exit.isSuccess(exit)) return exit.value;

  const defect = exit.cause.reasons.find(Cause.isDieReason);
  if (defect) throw defect.defect;

  const failure = exit.cause.reasons.find(Cause.isFailReason);
  if (failure) {
    return {
      code: null,
      stdout: Buffer.alloc(0),
      stderr: failure.error.message || failure.error._tag,
      truncated: false,
      timedOut: false,
    };
  }

  if (Cause.hasInterruptsOnly(exit.cause)) {
    return {
      code: null,
      stdout: Buffer.alloc(0),
      stderr: 'cancelled',
      truncated: true,
      timedOut: false,
    };
  }

  throw Cause.squash(exit.cause);
}

/**
 * The one pre-root runtime crossing permitted by P3. Construction is lazy:
 * creating and closing an unused bridge never builds ProcessRunnerLive. The
 * optional Layer exists only for isolated tests of typed startup/finalization;
 * production calls this with no argument and therefore always uses exactly
 * `ManagedRuntime.make(ProcessRunnerLive)`.
 */
export function createBootstrapProcessRuntimeBridge(
  layer: BootstrapLayer = ProcessRunnerLive,
): BootstrapProcessRuntimeBridge {
  let phase: 'open' | 'quiescing' | 'closed' = 'open';
  let runtime: BootstrapRuntime | null = null;
  let closePromise: Promise<void> | null = null;
  const active = new Set<TrackedRun>();

  const getRuntime = (): BootstrapRuntime => {
    runtime ??= ManagedRuntime.make(layer);
    return runtime;
  };

  const quiesce = (): void => {
    if (phase !== 'open') return;
    phase = 'quiescing';
    for (const entry of [...active]) entry.controller.abort();
  };

  const submit = <Success, Failure, Output>(
    effect: Effect.Effect<Success, Failure, ProcessRunner>,
    fromExit: (exit: Exit.Exit<Success, Failure | ProcessRunnerStartupError>) => Output,
    cancelled: Output,
  ): Promise<Output> => {
    if (phase !== 'open') return Promise.resolve(cancelled);

    const controller = new AbortController();
    let exit: Promise<Exit.Exit<Success, Failure | ProcessRunnerStartupError>>;
    try {
      exit = getRuntime().runPromiseExit(effect, { signal: controller.signal });
    } catch (defect) {
      return Promise.reject(defect);
    }

    const tracked: TrackedRun = { controller, exit };
    active.add(tracked);
    const forget = () => {
      active.delete(tracked);
    };
    void exit.then(forget, forget);
    return exit.then(fromExit);
  };

  const run = (request: ProcessRequest): Promise<ProcessResult> =>
    submit(execEffect(request), exitResult, CANCELLED_RESULT);

  const runBounded = (request: BoundedProcessRequest): Promise<BoundedProcessResult> =>
    submit(execBoundedEffect(request), boundedExitResult, {
      code: null,
      stdout: Buffer.alloc(0),
      stderr: 'cancelled',
      truncated: true,
      timedOut: false,
    });

  const closeAfterQuiesce = async (): Promise<void> => {
    await Promise.allSettled([...active].map((entry) => entry.exit));
    if (runtime) await runtime.dispose();
    phase = 'closed';
  };

  const close = (): Promise<void> => {
    if (closePromise) return closePromise;

    let resolveClose: () => void = () => undefined;
    let rejectClose: (error: unknown) => void = () => undefined;
    // Publish before quiesce aborts fibers: interruption finalizers may call
    // back into owner shutdown and must observe this exact Promise. Admission
    // itself closes synchronously, before close() returns to its caller.
    closePromise = new Promise<void>((resolve, reject) => {
      resolveClose = resolve;
      rejectClose = reject;
    });
    quiesce();
    void closeAfterQuiesce().then(resolveClose, rejectClose);
    return closePromise;
  };

  return { run, runBounded, quiesce, close };
}
