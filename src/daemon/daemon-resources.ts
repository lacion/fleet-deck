// Plain pre-Effect daemon resource aggregate (P1).
//
// This is deliberately policy-sized rather than a generic disposer stack:
// shutdown ordering is part of Fleet Deck's wire contract. P4 replaces the
// driver with LifecycleCoordinator while keeping these additive owner seams.

type MaybePromise<T = void> = T | Promise<T>;

export interface CloseOwner {
  close: () => MaybePromise<unknown>;
}

export interface CoreLifecycleOwner extends CloseOwner {
  quiesce?: () => MaybePromise<unknown>;
}

export interface HttpLifecycleOwner extends CloseOwner {
  quiesce: () => MaybePromise<unknown>;
  /** P4 starts this once during quiescing, but joins it only in closing-http. */
  beginGracefulStop?: () => Promise<void>;
  /** Synchronous invocation; its returned completion retains native stop failures. */
  forceStop?: () => Promise<void>;
  releaseHolds: () => MaybePromise<unknown>;
  /** Join route, terminal and client ownership while the listener remains owned. */
  closeClients?: () => Promise<void>;
  /** Callback-safe escalation for application clients; never retires native WS. */
  forceClients?: () => void;
}

export interface ProcessRuntimeOwner extends CloseOwner {
  quiesce: () => MaybePromise<unknown>;
  /** Callback-safe interruption request used by the coordinator force latch. */
  interrupt?: () => void;
  /** Join every admitted Effect before core/runtime/store retirement. */
  join?: () => MaybePromise<unknown>;
}

/** Root-owned ProcessRunner driver control, structurally independent of Effect. */
export interface ProcessDriverLifecycleOwner extends CloseOwner {
  /** Callback-safe immediate escalation for every active child/process group. */
  force: () => void;
}

export interface NamedCloseOwner {
  name: string;
  owner: CloseOwner;
}

export interface NamedProcessRuntimeOwner {
  name: string;
  owner: ProcessRuntimeOwner;
}

export interface NamedProcessDriverLifecycleOwner {
  name: string;
  owner: ProcessDriverLifecycleOwner;
}

export interface DaemonResourcesOptions {
  http?: HttpLifecycleOwner;
  core?: CoreLifecycleOwner;
  producers?: NamedCloseOwner[];
  discovery?: NamedCloseOwner[];
  processRuntime?: NamedProcessRuntimeOwner;
  processDriver?: NamedProcessDriverLifecycleOwner;
  store?: NamedCloseOwner;
  process?: NamedCloseOwner;
  onCloseError?: (name: string, error: unknown) => void;
}

export interface DaemonResourceCloseError {
  name: string;
  error: unknown;
}

/**
 * Sealed, dependency-free ownership view consumed by the app lifecycle adapter.
 * Keeping this type in the domain module preserves the directional boundary:
 * daemon resources know nothing about Effect or LifecycleCoordinator.
 */
export interface DaemonResourceLifecycleSnapshot {
  readonly http: HttpLifecycleOwner | null;
  readonly core: CoreLifecycleOwner | null;
  readonly producers: readonly NamedCloseOwner[];
  readonly discovery: readonly NamedCloseOwner[];
  readonly processRuntime: NamedProcessRuntimeOwner | null;
  readonly processDriver: NamedProcessDriverLifecycleOwner | null;
  readonly store: NamedCloseOwner | null;
  readonly process: NamedCloseOwner | null;
  readonly retainFailure: (name: string, error: unknown) => void;
}

/**
 * Owns every imperative daemon handle that exists before the Effect root.
 *
 * Registration is allowed only while running. Every phase is best-effort and
 * failures are retained for diagnostics. SQLite closes only when all of its
 * potential users report successful closure; otherwise process shutdown owns
 * the final handle retirement. Process ownership is always released. One close
 * Promise is shared by concurrent and repeated callers, which also makes every
 * partial-acquisition prefix safe.
 */
export class DaemonResources {
  private http: HttpLifecycleOwner | null = null;
  private core: CoreLifecycleOwner | null = null;
  private readonly producers: NamedCloseOwner[] = [];
  private readonly discovery: NamedCloseOwner[] = [];
  private processRuntime: NamedProcessRuntimeOwner | null = null;
  private processDriver: NamedProcessDriverLifecycleOwner | null = null;
  private store: NamedCloseOwner | null = null;
  private process: NamedCloseOwner | null = null;
  private readonly onCloseError: (name: string, error: unknown) => void;
  private readonly failures: DaemonResourceCloseError[] = [];
  private closePromise: Promise<void> | null = null;
  private lifecycleSnapshot: DaemonResourceLifecycleSnapshot | null = null;

  constructor(options: DaemonResourcesOptions = {}) {
    this.http = options.http ?? null;
    this.core = options.core ?? null;
    this.producers.push(...(options.producers ?? []));
    this.discovery.push(...(options.discovery ?? []));
    this.processRuntime = options.processRuntime ?? null;
    this.processDriver = options.processDriver ?? null;
    this.store = options.store ?? null;
    this.process = options.process ?? null;
    this.onCloseError =
      options.onCloseError ??
      (() => {
        /* the daemon entrypoint supplies its current logging policy */
      });
  }

  setHttp(owner: HttpLifecycleOwner): void {
    this.assertOpen();
    this.http = owner;
  }

  setCore(owner: CoreLifecycleOwner): void {
    this.assertOpen();
    this.core = owner;
  }

  addProducer(name: string, owner: CloseOwner): void {
    this.assertOpen();
    this.producers.push({ name, owner });
  }

  addDiscovery(name: string, owner: CloseOwner): void {
    this.assertOpen();
    this.discovery.push({ name, owner });
  }

  setProcessRuntime(name: string, owner: ProcessRuntimeOwner): void {
    this.assertOpen();
    this.processRuntime = { name, owner };
  }

  setProcessDriver(name: string, owner: ProcessDriverLifecycleOwner): void {
    this.assertOpen();
    this.processDriver = { name, owner };
  }

  setStore(name: string, owner: CloseOwner): void {
    this.assertOpen();
    this.store = { name, owner };
  }

  setProcess(name: string, owner: CloseOwner): void {
    this.assertOpen();
    this.process = { name, owner };
  }

  get closeErrors(): readonly DaemonResourceCloseError[] {
    return this.failures;
  }

  /** Seal acquisition and expose an app-agnostic ownership snapshot exactly once. */
  sealLifecycleOwnership(): DaemonResourceLifecycleSnapshot {
    if (this.lifecycleSnapshot) return this.lifecycleSnapshot;
    if (this.closePromise) throw new Error('daemon resources are closing');

    this.lifecycleSnapshot = {
      http: this.http,
      core: this.core,
      producers: [...this.producers],
      discovery: [...this.discovery],
      processRuntime: this.processRuntime,
      processDriver: this.processDriver,
      store: this.store,
      process: this.process,
      retainFailure: (name, error) => {
        this.retainFailure(name, error);
      },
    };
    return this.lifecycleSnapshot;
  }

  close(): Promise<void> {
    // Publish the shared Promise before invoking any owner callback. A lifecycle
    // callback is allowed to observe/re-enter aggregate shutdown, and must see
    // the exact same completion instead of starting a second close pass.
    this.closePromise ??= Promise.resolve().then(() => this.closeOnce());
    return this.closePromise;
  }

  private assertOpen(): void {
    if (this.closePromise || this.lifecycleSnapshot) {
      throw new Error('daemon resources are closing or lifecycle ownership is sealed');
    }
  }

  private retainFailure(name: string, error: unknown): void {
    this.failures.push({ name, error });
    try {
      this.onCloseError(name, error);
    } catch {
      /* diagnostics cannot interrupt cleanup */
    }
  }

  private async attempt(
    name: string,
    action: (() => MaybePromise<unknown>) | undefined,
  ): Promise<boolean> {
    if (!action) return true;
    try {
      await action();
      return true;
    } catch (error) {
      this.retainFailure(name, error);
      return false;
    }
  }

  private async closeGroup(group: readonly NamedCloseOwner[]): Promise<boolean> {
    // Reverse acquisition order within a dependency class. The class order
    // itself is explicit below and is never inferred from registration order.
    let closed = true;
    for (let index = group.length - 1; index >= 0; index -= 1) {
      const entry = group[index];
      if (entry) closed = (await this.attempt(entry.name, entry.owner.close)) && closed;
    }
    return closed;
  }

  private async closeOnce(): Promise<void> {
    // Refuse Promise-facade process submissions before any callback can enqueue
    // more native work. The runtime itself stays alive until every legacy
    // caller has joined, so interrupted Effects can finish their cleanup while
    // HTTP/core still own the state their continuations may observe.
    let storeSafe = await this.attempt(
      this.processRuntime ? `${this.processRuntime.name}.quiesce` : 'process-runtime.quiesce',
      this.processRuntime?.owner.quiesce,
    );

    // Refuse native ingress next. Quiescing core maintenance prevents expiry or
    // retention callbacks from racing the later transport/store phases.
    storeSafe = (await this.attempt('http.quiesce', this.http?.quiesce)) && storeSafe;
    storeSafe = (await this.attempt('core.quiesce', this.core?.quiesce)) && storeSafe;

    // Stop and join every producer before discovery withdraws or downstream
    // resources close. This includes boot work, agents polling and LAN refresh.
    storeSafe = (await this.closeGroup(this.producers)) && storeSafe;
    await this.closeGroup(this.discovery);

    // Hook responses must be settled while the HTTP transport is still alive.
    storeSafe = (await this.attempt('http.releaseHolds', this.http?.releaseHolds)) && storeSafe;
    storeSafe = (await this.attempt('http.close', this.http?.close)) && storeSafe;

    // Core close joins retention/question maintenance. Only now can root
    // ingress be closed and unbound: every Promise caller has returned, and
    // its tracked Effects are interrupted and joined before store retirement.
    // The shared ProcessRunner driver closes next; its Layer finalizer observes
    // this same idempotent join as a fallback rather than starting new cleanup.
    storeSafe = (await this.attempt('core.close', this.core?.close)) && storeSafe;
    if (this.processRuntime) {
      storeSafe =
        (await this.attempt(
          `${this.processRuntime.name}.close`,
          this.processRuntime.owner.close,
        )) && storeSafe;
    }
    if (this.processDriver) {
      storeSafe =
        (await this.attempt(this.processDriver.name, this.processDriver.owner.close)) && storeSafe;
    }

    // Only after every store user is gone may SQLite close; pid ownership is
    // released last.
    // A rejected upstream close cannot prove that its DB-using callbacks are
    // gone. The process will still release the pid and exit non-zero, at which
    // point the OS closes SQLite; closing it here would permit a late callback
    // to run against a retired handle in the meantime.
    if (this.store && storeSafe) await this.attempt(this.store.name, this.store.owner.close);
    if (this.process) await this.attempt(this.process.name, this.process.owner.close);
  }
}
