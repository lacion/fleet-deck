import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Deferred from 'effect/Deferred';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import * as Scope from 'effect/Scope';
import { prepareBackgroundOwner } from './background-owner.ts';
import { makeUnboundHttpServer } from './http-server-owner.ts';
import {
  DaemonStartupError,
  DaemonStartupRefusalError,
  HttpBindStartupError,
  type StartupError,
} from './errors.ts';
import {
  LifecycleCoordinator,
  type LifecycleCoordinatorOptions,
  type ShutdownTrigger,
} from './lifecycle-coordinator.ts';
import { makeDaemonResourceLifecycleOwner } from './daemon-resource-lifecycle.ts';
import { DaemonLifecycle } from './services/daemon-lifecycle.ts';
import { HttpServer } from './services/http-server.ts';
import { AppConfig, type AppConfigService } from './services/app-config.ts';
import {
  IngressSupervisor,
  type RootIngressSupervisorService,
} from './services/ingress-supervisor.ts';
import { ProcessRunner, type ProcessRunnerService } from './services/process-runner.ts';
import {
  ProcessRuntimeControl,
  type ProcessRuntimeControlService,
} from './services/process-runtime-control.ts';
import type { AcquiredDaemonResources, DaemonAcquisitionInputs } from './program.ts';
import { Background } from './services/background.ts';
import { makeIngressSupervisorLayer } from '../platform/bun/ingress-supervisor-live.ts';
import { ProcessRunnerLive } from '../platform/bun/process-runner-live.ts';

// Native selection belongs in this module. The one P4 daemon root provides
// this Layer; application services never reach into platform/** themselves.
export { ProcessRunnerLive };

export interface LiveLayerOptions<E extends StartupError> {
  readonly config: AppConfigService;
  /**
   * Adapter construction stays injectable until P3 supplies the Bun implementation.
   * Requiring AppConfig here also proves that Layer dependencies compose in RC.110.
   */
  readonly acquireProcessRunner: Effect.Effect<ProcessRunnerService, E, AppConfig>;
}

/**
 * Definition-only production composition seam. Importing this module acquires nothing and does
 * not create a runtime; the caller decides when and in which Scope the returned Layer is built.
 */
export function makeLiveLayer<E extends StartupError>(
  options: LiveLayerOptions<E>,
): Layer.Layer<AppConfig | ProcessRunner, E> {
  const configLayer = Layer.succeed(AppConfig, options.config);
  const processRunnerLayer = Layer.effect(ProcessRunner, options.acquireProcessRunner).pipe(
    Layer.provide(configLayer),
  );

  return Layer.merge(configLayer, processRunnerLayer);
}

export interface DaemonLifecycleLayerOptions {
  /**
   * Cold, prefix-safe acquisition callback. P4.4 supplies the production
   * function only after its bind/error seam is typed; P4.3 injects fixtures.
   */
  readonly acquireDaemonResources: (
    signal: AbortSignal,
    ingress: RootIngressSupervisorService,
    inputs: DaemonAcquisitionInputs,
  ) => Promise<AcquiredDaemonResources>;
  /**
   * The root takeover contract's complete signal-to-exit budget. Cancellation
   * starts one absolute deadline when Effect aborts acquisition; neither a
   * stuck prefix cleanup nor a late-success retirement may keep BunRuntime's
   * teardown waiting beyond it.
   */
  readonly acquisitionShutdownTimeoutMs: number;
  /**
   * Time inside the absolute deadline reserved for Layer finalizers and host
   * teardown after acquisition-prefix waiting stops.
   */
  readonly acquisitionShutdownReserveMs: number;
  /**
   * Callback-safe host policy hook. Production monotonically upgrades the
   * custom teardown status to 1; tests may retain the structured diagnostic.
   */
  readonly onAcquisitionShutdownFailure: (failure: DaemonAcquisitionShutdownFailure) => void;
  readonly makeLifecycleCoordinator: (acquired: AcquiredDaemonResources) => LifecycleCoordinator;
}

export type DaemonRootServices =
  | AppConfig
  | Background
  | HttpServer
  | ProcessRunner
  | ProcessRuntimeControl
  | DaemonLifecycle
  | IngressSupervisor;

export type DaemonAcquisitionShutdownFailure =
  | {
      readonly _tag: 'ProcessForceFailed';
      readonly cause: unknown;
    }
  | {
      readonly _tag: 'ProcessCloseFailed';
      readonly cause: unknown;
    }
  | {
      readonly _tag: 'AcquisitionCleanupFailed';
      readonly cause: unknown;
    }
  | {
      readonly _tag: 'LateRetirementFailed';
      readonly operation:
        | 'attach-process-driver'
        | 'resources.close'
        | 'shutdown-exit-code'
        | 'release-process';
      readonly cause: unknown;
    }
  | {
      readonly _tag: 'TimedOut';
      readonly timeoutMs: number;
    };

function errorMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

export type DaemonRootStartupError =
  | DaemonStartupError
  | DaemonStartupRefusalError
  | HttpBindStartupError;

function mapDaemonStartupError(cause: unknown): DaemonRootStartupError {
  return cause instanceof DaemonStartupError ||
    cause instanceof DaemonStartupRefusalError ||
    cause instanceof HttpBindStartupError
    ? cause
    : new DaemonStartupError({
        message: `daemon resource acquisition failed: ${errorMessage(cause)}`,
        cause,
      });
}

function isAbortFailure(cause: unknown, signal: AbortSignal): boolean {
  if (cause === signal.reason) return true;
  return (
    typeof cause === 'object' &&
    cause !== null &&
    'name' in cause &&
    (cause as { readonly name?: unknown }).name === 'AbortError'
  );
}

function reportAcquisitionShutdownFailure(
  options: DaemonLifecycleLayerOptions,
  failure: DaemonAcquisitionShutdownFailure,
): void {
  try {
    options.onAcquisitionShutdownFailure(failure);
  } catch {
    // Host accounting is callback-safe and must never replace or delay cleanup.
  }
}

function attachProcessDriver(
  acquired: AcquiredDaemonResources,
  processControl: ProcessRuntimeControlService,
): void {
  acquired.resources.setProcessDriver('bun-process-driver', processControl);
}

function acquisitionShutdownTiming(options: DaemonLifecycleLayerOptions): {
  readonly timeoutMs: number;
  readonly reserveMs: number;
} {
  const timeoutMs = options.acquisitionShutdownTimeoutMs;
  const reserveMs = options.acquisitionShutdownReserveMs;
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw new RangeError('acquisitionShutdownTimeoutMs must be a finite positive number');
  }
  if (!Number.isFinite(reserveMs) || reserveMs < 0 || reserveMs > timeoutMs) {
    throw new RangeError(
      'acquisitionShutdownReserveMs must be finite and between 0 and acquisitionShutdownTimeoutMs',
    );
  }
  return { timeoutMs, reserveMs };
}

/**
 * Interruptible acquisition with one bounded cancellation finalizer.
 *
 * Effect.callback aborts its private signal before evaluating the returned
 * finalizer. The signal observer therefore starts process escalation
 * synchronously, then the finalizer joins both driver close and the underlying
 * prefix cleanup only until the root's one absolute deadline. A Promise
 * continuation remains attached after timeout so a late successful acquisition
 * is still retired exactly once rather than becoming an unowned daemon.
 */
function acquireDaemonResourcesOwned(
  options: DaemonLifecycleLayerOptions,
  ingress: RootIngressSupervisorService,
  processControl: ProcessRuntimeControlService,
  inputs: DaemonAcquisitionInputs,
): Effect.Effect<AcquiredDaemonResources, DaemonRootStartupError> {
  return Effect.callback<AcquiredDaemonResources, DaemonRootStartupError>((resume, signal) => {
    let timing: ReturnType<typeof acquisitionShutdownTiming>;
    try {
      timing = acquisitionShutdownTiming(options);
    } catch (cause) {
      resume(Effect.fail(mapDaemonStartupError(cause)));
      return;
    }

    let acquisition: Promise<AcquiredDaemonResources>;
    try {
      acquisition = Promise.resolve(options.acquireDaemonResources(signal, ingress, inputs));
    } catch (cause) {
      resume(Effect.fail(mapDaemonStartupError(cause)));
      return;
    }

    let delivered = false;
    let cancellationStarted = false;
    let cancellationCompletion: Promise<void> | null = null;
    let lateRetirement: Promise<void> | null = null;
    let processDriverAttachmentAttempted = false;

    const attachProcessDriverOnce = (value: AcquiredDaemonResources): void => {
      if (processDriverAttachmentAttempted) return;
      processDriverAttachmentAttempted = true;
      attachProcessDriver(value, processControl);
    };

    const retireLateAcquisition = (late: AcquiredDaemonResources): Promise<void> => {
      if (lateRetirement) return lateRetirement;

      lateRetirement = (async () => {
        try {
          attachProcessDriverOnce(late);
        } catch (cause) {
          reportAcquisitionShutdownFailure(options, {
            _tag: 'LateRetirementFailed',
            operation: 'attach-process-driver',
            cause,
          });
        }

        try {
          await late.resources.close();
        } catch (cause) {
          // Production DaemonResources.close is non-rejecting, but retain a
          // structural fixture failure and continue to the host-owner fallback.
          reportAcquisitionShutdownFailure(options, {
            _tag: 'LateRetirementFailed',
            operation: 'resources.close',
            cause,
          });
        }

        try {
          if (late.shutdownExitCode() !== 0) {
            reportAcquisitionShutdownFailure(options, {
              _tag: 'LateRetirementFailed',
              operation: 'shutdown-exit-code',
              cause: new Error('late daemon resource retirement reported a cleanup failure'),
            });
          }
        } catch (cause) {
          reportAcquisitionShutdownFailure(options, {
            _tag: 'LateRetirementFailed',
            operation: 'shutdown-exit-code',
            cause,
          });
        }

        try {
          late.releaseProcessAtHostExit();
        } catch (cause) {
          reportAcquisitionShutdownFailure(options, {
            _tag: 'LateRetirementFailed',
            operation: 'release-process',
            cause,
          });
        }
      })();
      // The bounded finalizer may stop awaiting this continuation. It must stay
      // observed until it retires a late value or reports its own diagnostics.
      void lateRetirement.catch(() => undefined);
      return lateRetirement;
    };

    const startCancellation = (): Promise<void> => {
      if (cancellationCompletion) return cancellationCompletion;
      cancellationStarted = true;
      const cancellationStartedAt = performance.now();
      const absoluteDeadline = cancellationStartedAt + timing.timeoutMs;
      const cleanupDeadline = absoluteDeadline - timing.reserveMs;

      try {
        processControl.force();
      } catch (cause) {
        reportAcquisitionShutdownFailure(options, { _tag: 'ProcessForceFailed', cause });
      }

      let processClose: Promise<void>;
      try {
        processClose = Promise.resolve(processControl.close()).then(
          () => undefined,
          (cause: unknown) => {
            reportAcquisitionShutdownFailure(options, { _tag: 'ProcessCloseFailed', cause });
          },
        );
      } catch (cause) {
        reportAcquisitionShutdownFailure(options, { _tag: 'ProcessCloseFailed', cause });
        processClose = Promise.resolve();
      }

      const prefixCleanup = acquisition.then(
        (late) => retireLateAcquisition(late),
        (cause: unknown) => {
          if (!isAbortFailure(cause, signal)) {
            reportAcquisitionShutdownFailure(options, {
              _tag: 'AcquisitionCleanupFailed',
              cause,
            });
          }
        },
      );
      const completeCleanup = Promise.all([processClose, prefixCleanup]).then(() => undefined);

      cancellationCompletion = new Promise<void>((resolve) => {
        const remainingMs = Math.max(0, cleanupDeadline - performance.now());
        let settled = false;
        const finish = (): void => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          resolve();
        };
        const timer = setTimeout(() => {
          if (settled) return;
          reportAcquisitionShutdownFailure(options, {
            _tag: 'TimedOut',
            timeoutMs: timing.timeoutMs,
          });
          finish();
        }, remainingMs);
        void completeCleanup.then(finish);
      });
      return cancellationCompletion;
    };

    const onAbort = (): void => {
      if (delivered || cancellationStarted) return;
      void startCancellation();
    };
    signal.addEventListener('abort', onAbort, { once: true });
    if (signal.aborted) onAbort();

    void acquisition.then(
      (value) => {
        if (cancellationStarted || signal.aborted) {
          void startCancellation();
          void retireLateAcquisition(value);
          return;
        }
        try {
          // Publish driver ownership before callback success. Effect can observe
          // interruption immediately after resume; delaying this registration
          // until the next generator instruction would recreate an unowned gap.
          attachProcessDriverOnce(value);
        } catch (cause) {
          void retireLateAcquisition(value).then(() => {
            if (cancellationStarted || signal.aborted) return;
            delivered = true;
            signal.removeEventListener('abort', onAbort);
            resume(Effect.fail(mapDaemonStartupError(cause)));
          });
          return;
        }
        delivered = true;
        signal.removeEventListener('abort', onAbort);
        resume(Effect.succeed(value));
      },
      (cause: unknown) => {
        if (cancellationStarted || signal.aborted) {
          void startCancellation();
          return;
        }
        delivered = true;
        signal.removeEventListener('abort', onAbort);
        resume(Effect.fail(mapDaemonStartupError(cause)));
      },
    );

    return Effect.promise(() => startCancellation());
  });
}

/** Map a root Scope exit to the coordinator's stable application vocabulary. */
export function shutdownTriggerFromExit(exit: Exit.Exit<unknown, unknown>): ShutdownTrigger {
  if (Exit.isSuccess(exit)) return { _tag: 'Success' };

  const failure = exit.cause.reasons.find(Cause.isFailReason);
  if (failure) return { _tag: 'Failure', error: failure.error };

  const defect = exit.cause.reasons.find(Cause.isDieReason);
  if (defect) return { _tag: 'Defect', defect: defect.defect };

  return { _tag: 'Interruption' };
}

/** Build the root coordinator over the resource-sized P4 ownership adapter. */
export function makeDaemonLifecycleCoordinator(
  acquired: AcquiredDaemonResources,
  options: LifecycleCoordinatorOptions,
): LifecycleCoordinator {
  return new LifecycleCoordinator(makeDaemonResourceLifecycleOwner(acquired.resources), options);
}

/**
 * Scoped aggregate owner. AppConfig, ProcessRunner, ProcessRuntimeControl, and
 * IngressSupervisor are requirements to encode the P4.4 order: application
 * services build first, ingress captures that Context, then the legacy daemon
 * may bind its facades. Reverse finalization runs daemon policy close before
 * ingress/process fallbacks, so Scope closure cannot race ahead of the
 * coordinator.
 */
export function makeDaemonLifecycleLayer(
  options: DaemonLifecycleLayerOptions,
): Layer.Layer<
  DaemonLifecycle | Background | HttpServer,
  DaemonRootStartupError,
  AppConfig | ProcessRunner | ProcessRuntimeControl | IngressSupervisor
> {
  const acquireDaemonResources = Effect.gen(function* () {
    yield* Effect.context<AppConfig | ProcessRunner | ProcessRuntimeControl | IngressSupervisor>();
    const config = yield* AppConfig;
    const ingress = yield* IngressSupervisor;
    const processControl = yield* ProcessRuntimeControl;
    const program = yield* Deferred.make<Effect.Effect<never, never, ProcessRunner>>();
    const prepared = yield* prepareBackgroundOwner<ProcessRunner>({
      name: 'daemon-background',
      run: () => Deferred.await(program).pipe(Effect.flatten),
    });
    const acquired = yield* acquireDaemonResourcesOwned(options, ingress, processControl, {
      config,
      background: prepared.service,
      backgroundController: prepared.controller,
    });

    // acquireRelease restores interruption only around this complete acquire
    // Effect. Re-enter the mask after the callback publishes a value so the
    // Background owner registration and coordinator sealing cannot be split by
    // a signal.
    return yield* Effect.uninterruptible(
      Effect.gen(function* () {
        yield* Deferred.succeed(program, acquired.backgroundProgram);
        const owner = yield* prepared.start;
        // Publish the scoped Bun listener under the root Scope. Production boot
        // returns a bound owner; the injected acquisition fixtures inject no
        // listener, so a truthful unbound owner (no-op fallback) keeps them
        // building without perturbing the frozen finalizer sequence.
        const httpServer = acquired.httpServer ?? makeUnboundHttpServer(ingress);
        const scope = yield* Effect.scope;
        // Root-Scope fallback for listener retirement, registered during acquire
        // so finalizer LIFO runs it AFTER the acquireRelease release
        // (coordinator.close). On the success path the coordinator retires the
        // listener through the phased beginGracefulStop/forceStop (the
        // closing-http phase) and never calls http.lifecycle.close(), so this
        // fallback genuinely starts closeHttpOnce — a safe second pass only
        // because the transport's own latches have already run (quiesce latched,
        // holds released, memoized closeClients, bunServer === null so forceStop
        // no-ops). The memoized closePromise makes this a true no-op only on the
        // acquisition-failure path, where resources.close() already ran
        // http.close first. Inverting this LIFO (fallback before
        // coordinator.close) would collapse the frozen 8-phase shutdown.
        yield* Scope.addFinalizer(
          scope,
          Effect.sync(() => httpServer.shutdownFallback()),
        );
        let registered = false;

        return yield* Effect.try({
          try: () => {
            acquired.resources.addProducer('effect-background', { close: owner.close });
            registered = true;
            return {
              acquired,
              background: prepared.service,
              coordinator: options.makeLifecycleCoordinator(acquired),
              httpServer,
            };
          },
          catch: mapDaemonStartupError,
        }).pipe(
          Effect.onError(() =>
            Effect.promise(() =>
              Promise.allSettled([
                ...(registered ? [] : [owner.close()]),
                acquired.resources.close(),
              ]).then(() => undefined),
            ),
          ),
        );
      }),
    );
  });

  const scopedLifecycle = Effect.acquireRelease(
    acquireDaemonResources,
    (service, exit) => service.coordinator.close(shutdownTriggerFromExit(exit)).pipe(Effect.asVoid),
    { interruptible: true },
  );

  return Layer.effectContext(
    scopedLifecycle.pipe(
      Effect.map(({ acquired, background, coordinator, httpServer }) =>
        Context.make(DaemonLifecycle, { acquired, coordinator }).pipe(
          Context.add(Background, background),
          Context.add(HttpServer, httpServer.service),
        ),
      ),
    ),
  );
}

/**
 * Definition-only root composition. The dependency graph is intentionally
 * application -> ingress -> daemon; fallback finalizers reverse that order.
 * Constructing this value does not build a Layer, create a runtime, install
 * listeners, or acquire a daemon.
 */
export function composeDaemonRootLayer<ApplicationError, DaemonError, Requirements>(
  applicationLayer: Layer.Layer<
    AppConfig | ProcessRunner | ProcessRuntimeControl,
    ApplicationError,
    Requirements
  >,
  daemonLifecycleLayer: Layer.Layer<
    DaemonLifecycle | Background | HttpServer,
    DaemonError,
    AppConfig | ProcessRunner | ProcessRuntimeControl | IngressSupervisor
  >,
): Layer.Layer<DaemonRootServices, ApplicationError | DaemonError, Requirements> {
  const ingress = makeIngressSupervisorLayer<AppConfig | ProcessRunner | ProcessRuntimeControl>();
  const applicationAndIngress = ingress.pipe(Layer.provideMerge(applicationLayer));
  return daemonLifecycleLayer.pipe(Layer.provideMerge(applicationAndIngress));
}
