import type {
  DaemonResourceLifecycleSnapshot,
  DaemonResources,
  HttpLifecycleOwner,
  NamedCloseOwner,
  NamedProcessDriverLifecycleOwner,
  NamedProcessRuntimeOwner,
  ProcessDriverLifecycleOwner,
  ProcessRuntimeOwner,
} from '../daemon-resources.ts';
import type {
  LifecycleOwner,
  ShutdownForceSignal,
  ShutdownPhase,
  ShutdownPhaseContext,
} from './lifecycle-coordinator.ts';

type MaybePromise<T = void> = T | Promise<T>;

interface PhasedHttpLifecycleOwner extends HttpLifecycleOwner {
  beginGracefulStop: () => Promise<void>;
  forceStop: () => Promise<void>;
  closeClients: () => Promise<void>;
  forceClients: () => void;
}

interface PhasedProcessRuntimeOwner extends ProcessRuntimeOwner {
  interrupt: () => void;
  join: () => MaybePromise<unknown>;
}

interface PhasedProcessDriverLifecycleOwner extends ProcessDriverLifecycleOwner {
  force: () => void;
}

interface PhasedResourceSnapshot extends DaemonResourceLifecycleSnapshot {
  readonly http: PhasedHttpLifecycleOwner | null;
  readonly processRuntime: (NamedProcessRuntimeOwner & { owner: PhasedProcessRuntimeOwner }) | null;
  readonly processDriver:
    | (NamedProcessDriverLifecycleOwner & { owner: PhasedProcessDriverLifecycleOwner })
    | null;
}

interface NamedFailure {
  readonly name: string;
  readonly error: unknown;
}

const adapters = new WeakMap<DaemonResources, LifecycleOwner>();

function missingMethods(owner: object, methods: readonly string[]): string[] {
  return methods.filter(
    (method) => typeof (owner as Record<string, unknown>)[method] !== 'function',
  );
}

function phasedHttpOwner(owner: HttpLifecycleOwner | null): PhasedHttpLifecycleOwner | null {
  if (!owner) return null;
  const missing = missingMethods(owner, [
    'beginGracefulStop',
    'forceStop',
    'closeClients',
    'forceClients',
  ]);
  if (missing.length > 0) {
    throw new Error(`HTTP lifecycle owner cannot be split safely; missing ${missing.join(', ')}`);
  }
  return owner as PhasedHttpLifecycleOwner;
}

function phasedProcessRuntimeOwner(
  entry: NamedProcessRuntimeOwner | null,
): (NamedProcessRuntimeOwner & { owner: PhasedProcessRuntimeOwner }) | null {
  if (!entry) return null;
  const missing = missingMethods(entry.owner, ['interrupt', 'join']);
  if (missing.length > 0) {
    throw new Error(
      `process runtime lifecycle owner cannot be split safely; missing ${missing.join(', ')}`,
    );
  }
  return entry as NamedProcessRuntimeOwner & { owner: PhasedProcessRuntimeOwner };
}

function phasedProcessDriverOwner(
  entry: NamedProcessDriverLifecycleOwner | null,
): (NamedProcessDriverLifecycleOwner & { owner: PhasedProcessDriverLifecycleOwner }) | null {
  if (!entry) return null;
  const missing = missingMethods(entry.owner, ['force']);
  if (missing.length > 0) {
    throw new Error(
      `process driver lifecycle owner cannot be forced safely; missing ${missing.join(', ')}`,
    );
  }
  return entry as NamedProcessDriverLifecycleOwner & {
    owner: PhasedProcessDriverLifecycleOwner;
  };
}

function phaseFailure(phase: ShutdownPhase, failures: readonly NamedFailure[]): unknown {
  if (failures.length === 1) return failures[0]?.error;
  return new AggregateError(
    failures.map(({ error }) => error),
    `daemon ${phase} phase failed: ${failures.map(({ name }) => name).join(', ')}`,
  );
}

function phasedSnapshot(snapshot: DaemonResourceLifecycleSnapshot): PhasedResourceSnapshot {
  return {
    ...snapshot,
    http: phasedHttpOwner(snapshot.http),
    processRuntime: phasedProcessRuntimeOwner(snapshot.processRuntime),
    processDriver: phasedProcessDriverOwner(snapshot.processDriver),
  };
}

/**
 * Resource-sized P4 adapter. It owns no timer or deadline: the coordinator
 * races each returned completion against its one absolute ShutdownBudget.
 */
class DaemonResourceLifecycleAdapter {
  readonly owner: LifecycleOwner;

  private readonly resources: PhasedResourceSnapshot;
  private readonly runs = new Map<ShutdownPhase, Promise<void>>();
  private gracefulStop: Promise<void> | null = null;
  private forceStop: Promise<void> | null = null;
  private readonly forceRequested: Promise<void>;
  private resolveForceRequested: () => void = () => undefined;
  private runtimeInterrupted = false;
  private runtimeInterruptFailure: NamedFailure | null = null;
  private processDriverForced = false;
  private processDriverForceFailure: NamedFailure | null = null;
  private processDriverClose: Promise<void> | null = null;
  private httpClientsForced = false;
  private httpClientForceFailure: NamedFailure | null = null;

  constructor(resources: PhasedResourceSnapshot) {
    this.resources = resources;
    this.forceRequested = new Promise<void>((resolve) => {
      this.resolveForceRequested = resolve;
    });

    const interrupt = (_context: ShutdownPhaseContext, _signal: ShutdownForceSignal): void => {
      this.forceIngressWork();
    };
    const forceHttp = (_context: ShutdownPhaseContext, _signal: ShutdownForceSignal): void => {
      this.forceHttpWork();
    };
    const forceClients = (_context: ShutdownPhaseContext, _signal: ShutdownForceSignal): void => {
      this.forceClientWork();
    };
    this.owner = {
      quiescing: {
        run: (context) => this.runOnce('quiescing', () => this.quiesce(context)),
        force: interrupt,
      },
      'stopping-producers': {
        run: (_context) =>
          this.runOnce('stopping-producers', () =>
            this.closeGroup('stopping-producers', this.resources.producers),
          ),
        force: interrupt,
      },
      withdrawing: {
        run: (_context) =>
          this.runOnce('withdrawing', () =>
            this.closeGroup('withdrawing', this.resources.discovery),
          ),
        force: interrupt,
      },
      'releasing-holds': {
        run: (context) => this.runOnce('releasing-holds', () => this.releaseHolds(context)),
        force: interrupt,
      },
      'closing-clients': {
        run: (context) => this.runOnce('closing-clients', () => this.closeClients(context)),
        force: forceClients,
      },
      'closing-http': {
        run: (context) => this.runOnce('closing-http', () => this.closeHttp(context)),
        force: forceHttp,
      },
      'closing-store': {
        run: (context) => this.runOnce('closing-store', () => this.closeStore(context)),
        force: interrupt,
      },
      'releasing-process': {
        run: (context) => this.runOnce('releasing-process', () => this.releaseProcess(context)),
        force: interrupt,
      },
    };
  }

  /** Publish the shared completion before invoking a re-entrant owner callback. */
  private runOnce(phase: ShutdownPhase, action: () => MaybePromise): Promise<void> {
    const existing = this.runs.get(phase);
    if (existing) return existing;

    let resolveRun: () => void = () => undefined;
    let rejectRun: (error: unknown) => void = () => undefined;
    const completion = new Promise<void>((resolve, reject) => {
      resolveRun = resolve;
      rejectRun = reject;
    });
    this.runs.set(phase, completion);
    try {
      Promise.resolve(action()).then(resolveRun, rejectRun);
    } catch (error) {
      rejectRun(error);
    }
    return completion;
  }

  private async attempt(
    failures: NamedFailure[],
    name: string,
    action: (() => MaybePromise<unknown>) | undefined,
  ): Promise<void> {
    if (!action) return;
    try {
      await action();
    } catch (error) {
      failures.push({ name, error });
      this.resources.retainFailure(name, error);
    }
  }

  private finishAttempts(phase: ShutdownPhase, failures: readonly NamedFailure[]): void {
    if (failures.length > 0) throw phaseFailure(phase, failures);
  }

  private async quiesce(_context: ShutdownPhaseContext): Promise<void> {
    const failures: NamedFailure[] = [];
    const runtime = this.resources.processRuntime;
    await this.attempt(
      failures,
      runtime ? `${runtime.name}.quiesce` : 'process-runtime.quiesce',
      runtime?.owner.quiesce,
    );
    await this.attempt(failures, 'http.quiesce', this.resources.http?.quiesce);

    if (this.resources.http && !this.gracefulStop) {
      try {
        this.gracefulStop = Promise.resolve(this.resources.http.beginGracefulStop()).catch(
          (error: unknown) => {
            this.resources.retainFailure('http.beginGracefulStop', error);
            throw error;
          },
        );
      } catch (error) {
        failures.push({ name: 'http.beginGracefulStop', error });
        this.resources.retainFailure('http.beginGracefulStop', error);
        this.gracefulStop = Promise.reject(error);
      }
      // The operation is joined in closing-http. Observe it in the meantime so
      // a delayed native rejection cannot become process-level noise.
      void this.gracefulStop.catch(() => undefined);
    }

    await this.attempt(failures, 'core.quiesce', this.resources.core?.quiesce);
    this.finishAttempts('quiescing', failures);
  }

  private async closeGroup(
    phase: 'stopping-producers' | 'withdrawing',
    group: readonly NamedCloseOwner[],
  ): Promise<void> {
    const pending: Promise<NamedFailure | null>[] = [];
    for (let index = group.length - 1; index >= 0; index -= 1) {
      const entry = group[index];
      if (!entry) continue;
      try {
        // Publish every stop latch synchronously in reverse acquisition order
        // before awaiting any one producer. A stuck LAN callback must not keep
        // agents or boot work running past discovery withdrawal.
        pending.push(
          Promise.resolve(entry.owner.close()).then(
            () => null,
            (error: unknown) => {
              this.resources.retainFailure(entry.name, error);
              return { name: entry.name, error };
            },
          ),
        );
      } catch (error) {
        this.resources.retainFailure(entry.name, error);
        pending.push(Promise.resolve({ name: entry.name, error }));
      }
    }
    const failures = (await Promise.all(pending)).filter(
      (failure): failure is NamedFailure => failure !== null,
    );
    this.finishAttempts(phase, failures);
  }

  private async releaseHolds(_context: ShutdownPhaseContext): Promise<void> {
    const failures: NamedFailure[] = [];
    await this.attempt(failures, 'http.releaseHolds', this.resources.http?.releaseHolds);
    this.finishAttempts('releasing-holds', failures);
  }

  private interruptRuntime(): void {
    const runtime = this.resources.processRuntime;
    if (!runtime || this.runtimeInterrupted) return;
    this.runtimeInterrupted = true;
    try {
      runtime.owner.interrupt();
    } catch (error) {
      const failure = { name: `${runtime.name}.interrupt`, error };
      this.runtimeInterruptFailure = failure;
      this.resources.retainFailure(failure.name, error);
      throw error;
    }
  }

  private async closeClients(_context: ShutdownPhaseContext): Promise<void> {
    const failures: NamedFailure[] = [];
    try {
      this.interruptRuntime();
    } catch {
      // `interruptRuntime` retained the exact failure for this phase below.
    }
    if (this.runtimeInterruptFailure) failures.push(this.runtimeInterruptFailure);
    const processDriverClose = this.startProcessDriverClose();
    await this.attempt(failures, 'http.closeClients', this.resources.http?.closeClients);
    const runtime = this.resources.processRuntime;
    await this.attempt(
      failures,
      runtime ? `${runtime.name}.join` : 'process-runtime.join',
      runtime?.owner.join,
    );
    const processDriver = this.resources.processDriver;
    await this.attempt(
      failures,
      processDriver ? `${processDriver.name}.close` : 'process-driver.close',
      processDriverClose ? () => processDriverClose : undefined,
    );
    if (this.processDriverForceFailure) failures.push(this.processDriverForceFailure);
    if (this.httpClientForceFailure) failures.push(this.httpClientForceFailure);
    this.finishAttempts('closing-clients', failures);
  }

  private startProcessDriverClose(): Promise<void> | null {
    if (this.processDriverClose) return this.processDriverClose;
    const processDriver = this.resources.processDriver;
    if (!processDriver) return null;
    let close: Promise<void>;
    try {
      close = Promise.resolve(processDriver.owner.close()).then(() => undefined);
    } catch (error) {
      close = Promise.reject(error);
    }
    this.processDriverClose = close;
    // The phase (or Layer fallback) joins this Promise. Observe a prompt
    // rejection in the interval after a synchronous force callback.
    void close.catch(() => undefined);
    return close;
  }

  private startForceStop(): void {
    const http = this.resources.http;
    if (!http || this.forceStop) return;
    try {
      this.forceStop = Promise.resolve(http.forceStop());
    } catch (error) {
      this.forceStop = Promise.reject(error);
    }
    this.forceStop = this.forceStop.catch((error: unknown) => {
      this.resources.retainFailure('http.forceStop', error);
      throw error;
    });
    void this.forceStop.catch(() => undefined);
    this.resolveForceRequested();
  }

  private forceProcessDriver(): void {
    const processDriver = this.resources.processDriver;
    if (!processDriver || this.processDriverForced) return;
    this.processDriverForced = true;

    let forceFailed = false;
    let forceFailure: unknown;
    try {
      processDriver.owner.force();
    } catch (error) {
      forceFailed = true;
      forceFailure = error;
      const failure = { name: `${processDriver.name}.force`, error };
      this.processDriverForceFailure = failure;
      this.resources.retainFailure(failure.name, error);
    }
    this.startProcessDriverClose();
    if (forceFailed) throw forceFailure;
  }

  private forceIngressWork(): void {
    const failures: unknown[] = [];
    try {
      this.interruptRuntime();
    } catch (error) {
      failures.push(error);
    }
    try {
      this.forceProcessDriver();
    } catch (error) {
      failures.push(error);
    }
    if (failures.length === 1) throw failures[0];
    if (failures.length > 1) {
      throw new AggregateError(failures, 'ingress and process driver force both failed');
    }
  }

  private forceHttpClients(): void {
    const http = this.resources.http;
    if (!http || this.httpClientsForced) return;
    this.httpClientsForced = true;
    try {
      http.forceClients();
    } catch (error) {
      const failure = { name: 'http.forceClients', error };
      this.httpClientForceFailure = failure;
      this.resources.retainFailure(failure.name, error);
      throw error;
    }
  }

  private forceClientWork(): void {
    const failures: unknown[] = [];
    try {
      this.forceIngressWork();
    } catch (error) {
      failures.push(error);
    }
    try {
      this.forceHttpClients();
    } catch (error) {
      failures.push(error);
    }
    if (failures.length === 1) throw failures[0];
    if (failures.length > 1) {
      throw new AggregateError(failures, 'ingress/process and HTTP client force both failed');
    }
  }

  private forceHttpWork(): void {
    let runtimeForceFailed = false;
    let runtimeForceFailure: unknown;
    try {
      this.forceIngressWork();
    } catch (error) {
      runtimeForceFailed = true;
      runtimeForceFailure = error;
    }
    this.startForceStop();
    if (runtimeForceFailed) throw runtimeForceFailure;
  }

  private async closeHttp(_context: ShutdownPhaseContext): Promise<void> {
    const graceful = this.gracefulStop;
    if (!graceful && !this.forceStop) return;

    let gracefulFailed = false;
    let gracefulFailure: unknown;
    try {
      if (this.forceStop) {
        await this.forceStop;
        return;
      }
      await Promise.race([graceful ?? Promise.resolve(), this.forceRequested]);
    } catch (error) {
      gracefulFailed = true;
      gracefulFailure = error;
    }

    // A force callback publishes its Promise synchronously. Once observed, it
    // is the authoritative native-stop completion and its rejection is never
    // hidden behind a concurrently settling graceful Promise.
    if (this.forceStop) {
      await this.forceStop;
      return;
    }
    if (!gracefulFailed) return;

    // A rejected stop(false) is also a graceful loss. Do not wait for the
    // reserve/second-signal callback: publish stop(true) immediately so a
    // prompt native error cannot leave the listener owned while later phases
    // skip SQLite and the host proceeds toward exit.
    this.startForceStop();
    try {
      await this.forceStop;
    } catch (forceFailure) {
      throw new AggregateError(
        [gracefulFailure, forceFailure],
        'HTTP graceful and forced shutdown both failed',
      );
    }
    // Native ownership is retired, but the graceful failure remains a real
    // shutdown diagnostic and must make the root exit non-zero.
    throw gracefulFailure;
  }

  private async closeStore(context: ShutdownPhaseContext): Promise<void> {
    const failures: NamedFailure[] = [];
    await this.attempt(failures, 'core.close', this.resources.core?.close);
    const runtime = this.resources.processRuntime;
    await this.attempt(
      failures,
      runtime ? `${runtime.name}.close` : 'process-runtime.close',
      runtime?.owner.close,
    );

    // A failed DB user cannot prove that SQLite is safe. The coordinator also
    // skips this whole phase after any earlier failure/timeout; this local gate
    // covers a failure or deadline loss inside the phase itself.
    if (failures.length === 0 && context.remainingMs() > 0 && this.resources.store) {
      await this.attempt(failures, this.resources.store.name, this.resources.store.owner.close);
    }
    this.finishAttempts('closing-store', failures);
  }

  private async releaseProcess(_context: ShutdownPhaseContext): Promise<void> {
    const failures: NamedFailure[] = [];
    if (this.forceStop) {
      try {
        await this.forceStop;
      } catch (error) {
        // The force Promise retains this diagnostic when it rejects. Keep the
        // phase failure typed without recording the same owner failure twice.
        failures.push({ name: 'http.forceStop', error });
      }
    }
    // Never unlink the owned pidfile from an asynchronous lifecycle phase.
    // Even a nominally successful policy close is followed by Effect Layer
    // finalizers, while an unsafe close may deliberately leave SQLite/native
    // handles to the OS. DaemonHostControl retains this same idempotent owner
    // and invokes it synchronously in BunRuntime teardown, immediately before
    // process.exit, so a same-HOME challenger cannot enter during that gap.
    this.finishAttempts('releasing-process', failures);
  }
}

/**
 * Seal one fully acquired DaemonResources value and expose its exact P4 owner.
 * Repeated calls return the same adapter identity; opaque P1-only owners are
 * rejected instead of guessing at unsafe phase boundaries.
 */
export function makeDaemonResourceLifecycleOwner(resources: DaemonResources): LifecycleOwner {
  const existing = adapters.get(resources);
  if (existing) return existing;

  const adapter = new DaemonResourceLifecycleAdapter(
    phasedSnapshot(resources.sealLifecycleOwnership()),
  );
  adapters.set(resources, adapter.owner);
  return adapter.owner;
}
