import type * as Exit from 'effect/Exit';
import * as Runtime from 'effect/Runtime';
import type {
  LifecycleCloseOutcome,
  LifecycleCoordinator,
  ShutdownForceReason,
} from './lifecycle-coordinator.ts';

export type DaemonSignal = 'SIGINT' | 'SIGTERM';

interface SignalHost {
  on(signal: DaemonSignal, listener: () => void): unknown;
  removeListener(signal: DaemonSignal, listener: () => void): unknown;
}

/**
 * Convert the complete policy outcome to Fleet Deck's existing process status.
 * Forced phases are safe when their owner actually joined; failures, deadline
 * skips, and resource-owner diagnostics are not.
 */
export function coordinatedShutdownExitCode(
  outcome: LifecycleCloseOutcome,
  resourceExitCode: 0 | 1,
): 0 | 1 {
  if (resourceExitCode !== 0) return 1;
  if (outcome.deadlineExpired || outcome.failures.length || outcome.forceFailures.length) return 1;
  return outcome.phases.some(
    (phase) => phase._tag === 'Failed' || phase._tag === 'TimedOut' || phase._tag === 'Skipped',
  )
    ? 1
    : 0;
}

/**
 * Callback-safe host state shared by DaemonApp and BunRuntime's teardown.
 *
 * Signal callbacks only update local state and synchronously trip the existing
 * coordinator force latch. They never construct/run an Effect or invoke an
 * asynchronous finalizer. The root Effect remains the sole cleanup driver.
 */
export class DaemonHostControl {
  private coordinator: LifecycleCoordinator | null = null;
  private observedFirstSignal: DaemonSignal | null = null;
  private observedSignalCount = 0;
  private pendingForce: ShutdownForceReason | null = null;
  private coordinatedExitCode: 0 | 1 | null = null;
  private processExitFallback: (() => void) | null = null;
  private installedHost: SignalHost | null = null;
  private removeObserver: (() => void) | null = null;

  get firstSignal(): DaemonSignal | null {
    return this.observedFirstSignal;
  }

  get signalCount(): number {
    return this.observedSignalCount;
  }

  get signalObserved(): boolean {
    return this.observedFirstSignal !== null;
  }

  /** Attach the one policy coordinator after aggregate acquisition completes. */
  attachLifecycle(coordinator: LifecycleCoordinator): void {
    if (this.coordinator && this.coordinator !== coordinator) {
      throw new Error('daemon host control already owns another lifecycle coordinator');
    }
    this.coordinator = coordinator;
    if (this.pendingForce) coordinator.force(this.pendingForce);
  }

  detachLifecycle(coordinator: LifecycleCoordinator): void {
    if (this.coordinator === coordinator) this.coordinator = null;
  }

  /**
   * Retain the verified process-owner release through the exact host-exit
   * boundary. Policy close retains the same idempotent function through every
   * Layer fallback; teardown invokes it immediately before BunRuntime calls
   * process.exit, with no asynchronous gap for a successor to race.
   */
  attachProcessExitFallback(release: () => void): void {
    if (this.processExitFallback && this.processExitFallback !== release) {
      throw new Error('daemon host control already owns another process-exit fallback');
    }
    this.processExitFallback = release;
  }

  /**
   * Install before aggregate acquisition and retain through policy cleanup.
   * Repeated installation on the same host returns the exact same disposer.
   */
  installSignalObserver(host: SignalHost = process): () => void {
    if (this.removeObserver) {
      if (this.installedHost !== host) {
        throw new Error('daemon signal observer already installed on another host');
      }
      return this.removeObserver;
    }

    const onSigint = (): void => {
      this.observeSignal('SIGINT');
    };
    const onSigterm = (): void => {
      this.observeSignal('SIGTERM');
    };
    host.on('SIGINT', onSigint);
    host.on('SIGTERM', onSigterm);
    this.installedHost = host;

    let installed = true;
    const remove = (): void => {
      if (!installed) return;
      installed = false;
      host.removeListener('SIGINT', onSigint);
      host.removeListener('SIGTERM', onSigterm);
      if (this.removeObserver === remove) {
        this.removeObserver = null;
        this.installedHost = null;
      }
    };
    this.removeObserver = remove;
    return remove;
  }

  recordLifecycleOutcome(outcome: LifecycleCloseOutcome, resourceExitCode: 0 | 1): void {
    this.recordExitCode(coordinatedShutdownExitCode(outcome, resourceExitCode));
  }

  recordExitCode(next: 0 | 1): void {
    // Re-entrant/fallback finalizers can report the same outcome again. Exit
    // status is monotonic: a later failure may upgrade 0 to 1, never hide one.
    this.coordinatedExitCode = this.coordinatedExitCode === 1 ? 1 : next;
  }

  /** BunRuntime calls this only after the root Effect and its async cleanup end. */
  readonly teardown: Runtime.Teardown = <E, A>(
    exit: Exit.Exit<E, A>,
    onExit: (code: number) => void,
  ): void => {
    const release = this.processExitFallback;
    this.processExitFallback = null;
    if (release) {
      try {
        release();
      } catch {
        this.recordExitCode(1);
      }
    }
    if (this.coordinatedExitCode !== null) {
      onExit(this.coordinatedExitCode);
      return;
    }
    Runtime.defaultTeardown(exit, onExit);
  };

  private observeSignal(signal: DaemonSignal): void {
    this.observedSignalCount++;
    this.observedFirstSignal ??= signal;

    let force: ShutdownForceReason | null = null;
    if (this.observedSignalCount > 1) {
      force = { _tag: 'SecondSignal', signal };
    } else if (this.coordinator?.closing) {
      force = { _tag: 'SignalDuringShutdown', signal };
    }

    if (!force) return;
    this.pendingForce ??= force;
    this.coordinator?.force(force);
  }
}
