import { writeSync } from 'node:fs';
import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import { AppConfigLive } from './app-config-live.ts';
import { DaemonStartupRefusalError, HttpBindStartupError } from './errors.ts';
import { DaemonHostControl } from './host-control.ts';
import {
  composeDaemonRootLayer,
  type DaemonRootStartupError,
  makeDaemonLifecycleCoordinator,
  makeDaemonLifecycleLayer,
  ProcessRunnerLive,
  shutdownTriggerFromExit,
} from './live-layer.ts';
import type { ShutdownTrigger } from './lifecycle-coordinator.ts';
import { acquireDaemonResources } from './program.ts';
import { Background, BackgroundOperationalError } from './services/background.ts';
import { DaemonLifecycle, type DaemonLifecycleService } from './services/daemon-lifecycle.ts';

/** The 2s takeover window leaves this root 250ms to publish process exit. */
export const DAEMON_SHUTDOWN_TIMEOUT_MS = 1_750;

const ProductionApplicationLayer = Layer.merge(AppConfigLive, ProcessRunnerLive);

export type DaemonRootError = DaemonRootStartupError | BackgroundOperationalError;

function triggerForExit(
  hostControl: DaemonHostControl,
  exit: Exit.Exit<unknown, unknown>,
): ShutdownTrigger {
  if (hostControl.firstSignal && Exit.isFailure(exit) && Cause.hasInterruptsOnly(exit.cause)) {
    return { _tag: 'Interruption', signal: hostControl.firstSignal };
  }
  return shutdownTriggerFromExit(exit);
}

function closeLifecycle(
  hostControl: DaemonHostControl,
  lifecycle: DaemonLifecycleService,
  exit: Exit.Exit<unknown, unknown>,
): Effect.Effect<void> {
  return lifecycle.coordinator.close(triggerForExit(hostControl, exit)).pipe(
    Effect.tap((outcome) =>
      Effect.sync(() => {
        hostControl.recordLifecycleOutcome(outcome, lifecycle.acquired.shutdownExitCode());
      }),
    ),
    Effect.ensuring(
      Effect.sync(() => {
        hostControl.detachLifecycle(lifecycle.coordinator);
      }),
    ),
    Effect.asVoid,
  );
}

function writeStderrLine(message: string): void {
  try {
    // BunRuntime exits immediately after teardown, and stderr is commonly a
    // launcher/test pipe. Keep the historical synchronous one-line contract.
    writeSync(2, `${message}\n`);
  } catch {
    // A closed stderr must not replace the startup policy or its exit code.
  }
}

function reportRootFailure(error: DaemonRootError): void {
  if (error instanceof DaemonStartupRefusalError) {
    writeStderrLine(error.message);
    return;
  }
  if (error instanceof HttpBindStartupError) {
    if (error.reason === 'address-in-use') writeStderrLine(error.message);
    else console.error(error.cause);
    return;
  }
  if (error instanceof BackgroundOperationalError) {
    console.error(error);
    return;
  }
  console.error(error.cause);
}

function reportRootDefect(defect: unknown): void {
  try {
    console.error(defect);
  } catch {
    // A closed stderr must not replace the defect Cause or its exit code.
  }
}

/**
 * Interpret the root Cause exactly once before BunRuntime's teardown maps it
 * to a process status. Expected startup failures keep their stable messages,
 * defects remain visible even though the host runner's automatic reporter is
 * disabled, and a signal-only interruption is recorded as a clean daemon exit.
 */
export function withDaemonRootExitPolicy<A, R>(
  hostControl: DaemonHostControl,
  effect: Effect.Effect<A, DaemonRootError, R>,
): Effect.Effect<A, DaemonRootError, R> {
  const interpreted = effect.pipe(
    Effect.tapError((error) => Effect.sync(() => reportRootFailure(error))),
    // A signal during interruptible Layer acquisition has no coordinator yet.
    // If fallback finalization completed with interruption only, it is the same
    // expected clean host shutdown; any cleanup failure changes the Exit Cause
    // and therefore cannot be accidentally mapped to zero here.
    Effect.onExit((exit) =>
      Effect.sync(() => {
        if (
          hostControl.signalObserved &&
          Exit.isFailure(exit) &&
          Cause.hasInterruptsOnly(exit.cause)
        ) {
          hostControl.recordExitCode(0);
        }
      }),
    ),
  );

  return Effect.acquireUseRelease(
    Effect.sync(() => hostControl.installSignalObserver()),
    () => interpreted,
    (removeSignalObserver) => Effect.sync(removeSignalObserver),
  ).pipe(
    // Keep this outside the observer resource so acquisition, application, and
    // release defects all reach the same single owning-boundary reporter.
    Effect.tapDefect((defect) => Effect.sync(() => reportRootDefect(defect))),
  );
}

/** Build one cold, resource-sized root Layer; no runtime is constructed here. */
export function makeProductionDaemonRootLayer(hostControl: DaemonHostControl) {
  const daemonLayer = makeDaemonLifecycleLayer({
    acquireDaemonResources,
    acquisitionShutdownTimeoutMs: DAEMON_SHUTDOWN_TIMEOUT_MS,
    acquisitionShutdownReserveMs: 250,
    onAcquisitionShutdownFailure: () => hostControl.recordExitCode(1),
    makeLifecycleCoordinator: (acquired) =>
      makeDaemonLifecycleCoordinator(acquired, {
        timeoutMs: DAEMON_SHUTDOWN_TIMEOUT_MS,
        forceReserveMs: 250,
      }),
  });
  return composeDaemonRootLayer(ProductionApplicationLayer, daemonLayer);
}

/**
 * One daemon Effect kept alive until BunRuntime interruption. The coordinator
 * onExit is inside the provided Layer, so every service remains live through
 * policy cleanup and Layer finalizers are only idempotent fallbacks.
 */
export function makeDaemonApp(
  hostControl: DaemonHostControl,
): Effect.Effect<never, DaemonRootError> {
  const rootLayer = makeProductionDaemonRootLayer(hostControl);
  const operational = Effect.gen(function* () {
    const lifecycle = yield* DaemonLifecycle;
    const background = yield* Background;
    hostControl.attachProcessExitFallback(lifecycle.acquired.releaseProcessAtHostExit);
    hostControl.attachLifecycle(lifecycle.coordinator);
    return yield* background.awaitFailure.pipe(
      Effect.onExit((exit) => closeLifecycle(hostControl, lifecycle, exit)),
    );
  });

  return withDaemonRootExitPolicy(hostControl, operational.pipe(Effect.provide(rootLayer)));
}

export const daemonHostControl = new DaemonHostControl();
export const DaemonApp = makeDaemonApp(daemonHostControl);
