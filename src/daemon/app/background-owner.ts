import * as Cause from 'effect/Cause';
import * as Data from 'effect/Data';
import * as Deferred from 'effect/Deferred';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import type * as Fiber from 'effect/Fiber';
import * as Ref from 'effect/Ref';
import * as Scope from 'effect/Scope';
import {
  type BackgroundService,
  BackgroundUnexpectedExitError,
  BackgroundUnexpectedInterruptionError,
  type BackgroundOperationalError,
  type ReconciliationStatus,
} from './services/background.ts';

const DEFAULT_JOIN_TIMEOUT_MS = 1_000;

export class BackgroundJoinTimeoutError extends Data.TaggedError('BackgroundJoinTimeoutError')<{
  readonly owner: string;
  readonly timeoutMs: number;
  readonly message: string;
}> {}

/** Close-channel representation of one or more named Effect failures. */
export class BackgroundOperationalExitError extends Data.TaggedError(
  'BackgroundOperationalExitError',
)<{
  readonly owner: string;
  readonly errors: readonly BackgroundOperationalError[];
  readonly cause: Cause.Cause<BackgroundOperationalError>;
  readonly message: string;
}> {}

/** Close-channel representation of an Effect defect; the full Cause is retained. */
export class BackgroundDefectExitError extends Data.TaggedError('BackgroundDefectExitError')<{
  readonly owner: string;
  readonly cause: Cause.Cause<BackgroundOperationalError>;
  readonly message: string;
}> {}

export interface BackgroundController {
  /** Idempotent; publishes the synchronous status before waking awaitReady. */
  readonly markReconciliationReady: Effect.Effect<void>;
}

export interface BackgroundOwner {
  readonly state: 'running' | 'interrupting' | 'closed';
  /** Callback-safe, synchronous, and idempotent. */
  readonly interrupt: () => void;
  /** Exact memoized Promise: interrupt first, then join within one configured bound. */
  readonly close: () => Promise<void>;
}

export interface BackgroundRegistration {
  readonly service: BackgroundService;
  readonly controller: BackgroundController;
  readonly owner: BackgroundOwner;
}

/**
 * Cold background registration state.
 *
 * Readiness is available to native acquisition before the daemon-long fiber is
 * admitted. Evaluating `start` later starts that fiber exactly once and binds
 * its fallback finalizer to the Scope of the first evaluation.
 */
export interface PreparedBackgroundOwner<R> {
  readonly service: BackgroundService;
  readonly controller: BackgroundController;
  readonly start: Effect.Effect<BackgroundOwner, never, R | Scope.Scope>;
}

export interface BackgroundOwnerOptions<R> {
  readonly name: string;
  readonly run: (
    controller: BackgroundController,
  ) => Effect.Effect<unknown, BackgroundOperationalError, R>;
  readonly joinTimeoutMs?: number;
  /** Injectable host timer for deterministic owner-boundary tests. */
  readonly armJoinTimeout?: (callback: () => void, milliseconds: number) => () => void;
}

export interface OwnedLegacyPromiseOptions<A, E> {
  /** Construct the already-owned Promise. A synchronous throw is a defect. */
  readonly try: () => PromiseLike<A>;
  /** Only Promise rejection is mapped to the named operational channel. */
  readonly catch: (cause: unknown) => E;
}

/**
 * Lift a legacy Promise without allowing fiber interruption to detach it.
 *
 * Effect.callback runs the returned canceler when the awaiting fiber is
 * interrupted. That canceler deliberately does not pretend the Promise is
 * cancellable: it joins the same settlement before interruption completes, so
 * a late continuation cannot reach SQLite after the owner has reported closed.
 */
export function ownedLegacyPromise<A, E>(
  options: OwnedLegacyPromiseOptions<A, E>,
): Effect.Effect<A, E> {
  return Effect.callback<A, E>((resume) => {
    let promise: PromiseLike<A>;
    try {
      promise = options.try();
    } catch (defect) {
      resume(Effect.die(defect));
      return;
    }

    const completion = Promise.resolve(promise).then(
      (value) => {
        resume(Effect.succeed(value));
      },
      (cause: unknown) => {
        try {
          resume(Effect.fail(options.catch(cause)));
        } catch (defect) {
          resume(Effect.die(defect));
        }
      },
    );

    // The canceler itself is awaited by Effect. It is non-rejecting even when
    // the source rejected, because the rejection branch above consumes it.
    return Effect.promise(() => completion);
  });
}

function validateJoinTimeout(value: number): number {
  if (!Number.isFinite(value) || value <= 0) {
    throw new RangeError('background join timeout must be a finite positive number');
  }
  return value;
}

function armLiveJoinTimeout(callback: () => void, milliseconds: number): () => void {
  const timer = setTimeout(callback, milliseconds);
  timer.unref();
  return () => {
    clearTimeout(timer);
  };
}

function unexpectedExit(owner: string): BackgroundUnexpectedExitError {
  return new BackgroundUnexpectedExitError({
    owner,
    message: `background owner ${owner} returned unexpectedly`,
  });
}

function unexpectedInterruption(owner: string): BackgroundUnexpectedInterruptionError {
  return new BackgroundUnexpectedInterruptionError({
    owner,
    message: `background owner ${owner} interrupted without a stop request`,
  });
}

function closeExitError(
  owner: string,
  interruptionRequested: boolean,
  exit: Exit.Exit<unknown, BackgroundOperationalError>,
):
  | BackgroundUnexpectedExitError
  | BackgroundUnexpectedInterruptionError
  | BackgroundOperationalExitError
  | BackgroundDefectExitError
  | null {
  if (Exit.isSuccess(exit)) return unexpectedExit(owner);
  if (Cause.hasInterruptsOnly(exit.cause)) {
    return interruptionRequested ? null : unexpectedInterruption(owner);
  }
  if (Cause.hasDies(exit.cause)) {
    return new BackgroundDefectExitError({
      owner,
      cause: exit.cause,
      message: `background owner ${owner} exited with a defect`,
    });
  }
  const errors = exit.cause.reasons.filter(Cause.isFailReason).map((reason) => reason.error);
  if (errors.length > 0) {
    return new BackgroundOperationalExitError({
      owner,
      errors,
      cause: exit.cause,
      message: `background owner ${owner} exited with an operational failure`,
    });
  }
  return new BackgroundDefectExitError({
    owner,
    cause: exit.cause,
    message: `background owner ${owner} exited with an unknown cause`,
  });
}

/**
 * Start one daemon-long fiber and bridge it to the imperative producer owner.
 *
 * The fiber is deliberately detached from Scope's implicit join machinery.
 * The normal producer phase calls the bounded owner.close(); the Scope
 * finalizer is only a synchronous interruption fallback, so teardown can never
 * start a second unbounded join after the policy deadline has already expired.
 */
function startBackgroundOwner<R>(
  options: BackgroundOwnerOptions<R>,
  controller: BackgroundController,
  ready: Deferred.Deferred<void, BackgroundOperationalError>,
  failure: Deferred.Deferred<never, BackgroundOperationalError>,
  joinTimeoutMs: number,
): Effect.Effect<BackgroundOwner, never, R | Scope.Scope> {
  return Effect.uninterruptible(
    Effect.gen(function* () {
      const scope = yield* Effect.scope;

      let phase: 'running' | 'interrupting' | 'closed' = 'running';
      let interruptionRequested = false;
      let closePromise: Promise<void> | null = null;
      let resolveCompletion: (exit: Exit.Exit<unknown, BackgroundOperationalError>) => void = () =>
        undefined;
      const completion = new Promise<Exit.Exit<unknown, BackgroundOperationalError>>((resolve) => {
        resolveCompletion = resolve;
      });
      const fiber: Fiber.Fiber<unknown, BackgroundOperationalError> = yield* Effect.forkDetach(
        Effect.suspend(() => options.run(controller)),
      );

      fiber.addObserver((exit) => {
        phase = 'closed';
        resolveCompletion(exit);

        if (Exit.isSuccess(exit)) {
          const defect = unexpectedExit(options.name);
          Deferred.doneUnsafe(ready, Effect.die(defect));
          Deferred.doneUnsafe(failure, Effect.die(defect));
          return;
        }
        if (Cause.hasInterruptsOnly(exit.cause) && !interruptionRequested) {
          const defect = unexpectedInterruption(options.name);
          Deferred.doneUnsafe(ready, Effect.die(defect));
          Deferred.doneUnsafe(failure, Effect.die(defect));
          return;
        }
        Deferred.doneUnsafe(ready, Effect.failCause(exit.cause));
        Deferred.doneUnsafe(failure, Effect.failCause(exit.cause));
      });

      const interrupt = (): void => {
        if (interruptionRequested || phase === 'closed') return;
        interruptionRequested = true;
        phase = 'interrupting';
        fiber.interruptUnsafe();
      };

      const close = (): Promise<void> => {
        if (closePromise) return closePromise;

        let resolveClose: () => void = () => undefined;
        let rejectClose: (error: unknown) => void = () => undefined;
        // Publish before interruptUnsafe: an Effect finalizer may synchronously
        // re-enter close and must observe this exact Promise.
        closePromise = new Promise<void>((resolve, reject) => {
          resolveClose = resolve;
          rejectClose = reject;
        });

        interrupt();
        let cancelTimeout: () => void = () => undefined;
        void completion.then((exit) => {
          cancelTimeout();
          const error = closeExitError(options.name, interruptionRequested, exit);
          if (error) rejectClose(error);
          else resolveClose();
        });
        try {
          cancelTimeout = (options.armJoinTimeout ?? armLiveJoinTimeout)(() => {
            rejectClose(
              new BackgroundJoinTimeoutError({
                owner: options.name,
                timeoutMs: joinTimeoutMs,
                message: `background owner ${options.name} did not join within ${String(joinTimeoutMs)}ms`,
              }),
            );
          }, joinTimeoutMs);
        } catch (defect) {
          rejectClose(defect);
        }
        return closePromise;
      };

      const owner: BackgroundOwner = {
        get state() {
          return phase;
        },
        interrupt,
        close,
      };

      // Fallback only: normal lifecycle ownership calls close() and gets its one
      // bounded join. Never await close here or Scope teardown could start a new
      // unbounded wait after that join timed out.
      yield* Scope.addFinalizer(scope, Effect.sync(interrupt));
      return owner;
    }),
  );
}

/**
 * Allocate Background readiness without starting its daemon-long fiber.
 *
 * `start` is a cold, concurrent-safe cached Effect. Its first evaluation owns
 * the one detached fiber and installs the Scope fallback; later evaluations
 * return the exact same owner without invoking `options.run` again.
 */
export function prepareBackgroundOwner<R>(
  options: BackgroundOwnerOptions<R>,
): Effect.Effect<PreparedBackgroundOwner<R>> {
  return Effect.uninterruptible(
    Effect.gen(function* () {
      const joinTimeoutMs = validateJoinTimeout(options.joinTimeoutMs ?? DEFAULT_JOIN_TIMEOUT_MS);
      const status = yield* Ref.make<ReconciliationStatus>('reconciling');
      const ready = yield* Deferred.make<void, BackgroundOperationalError>();
      const failure = yield* Deferred.make<never, BackgroundOperationalError>();

      const markReconciliationReady = Ref.set(status, 'settled').pipe(
        Effect.andThen(Deferred.succeed(ready, undefined)),
        Effect.asVoid,
      );
      const controller: BackgroundController = { markReconciliationReady };
      const service: BackgroundService = {
        reconciliationStatus: () => Ref.getUnsafe(status),
        awaitReady: Deferred.await(ready),
        awaitFailure: Deferred.await(failure),
      };
      const start = yield* Effect.cached(
        startBackgroundOwner(options, controller, ready, failure, joinTimeoutMs),
      );

      return { service, controller, start };
    }),
  );
}

/**
 * Start one daemon-long fiber and bridge it to the imperative producer owner.
 *
 * Compatibility wrapper for callers that do not need to expose Background
 * readiness before native acquisition.
 */
export function makeBackgroundOwner<R>(
  options: BackgroundOwnerOptions<R>,
): Effect.Effect<BackgroundRegistration, never, R | Scope.Scope> {
  return Effect.uninterruptible(
    Effect.gen(function* () {
      const prepared = yield* prepareBackgroundOwner(options);
      const owner = yield* prepared.start;
      return { service: prepared.service, controller: prepared.controller, owner };
    }),
  );
}
