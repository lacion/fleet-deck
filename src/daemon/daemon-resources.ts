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
  releaseHolds: () => MaybePromise<unknown>;
}

export interface NamedCloseOwner {
  name: string;
  owner: CloseOwner;
}

export interface DaemonResourcesOptions {
  http?: HttpLifecycleOwner;
  core?: CoreLifecycleOwner;
  producers?: NamedCloseOwner[];
  discovery?: NamedCloseOwner[];
  store?: NamedCloseOwner;
  process?: NamedCloseOwner;
  onCloseError?: (name: string, error: unknown) => void;
}

export interface DaemonResourceCloseError {
  name: string;
  error: unknown;
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
  private store: NamedCloseOwner | null = null;
  private process: NamedCloseOwner | null = null;
  private readonly onCloseError: (name: string, error: unknown) => void;
  private readonly failures: DaemonResourceCloseError[] = [];
  private closePromise: Promise<void> | null = null;

  constructor(options: DaemonResourcesOptions = {}) {
    this.http = options.http ?? null;
    this.core = options.core ?? null;
    this.producers.push(...(options.producers ?? []));
    this.discovery.push(...(options.discovery ?? []));
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

  close(): Promise<void> {
    // Publish the shared Promise before invoking any owner callback. A lifecycle
    // callback is allowed to observe/re-enter aggregate shutdown, and must see
    // the exact same completion instead of starting a second close pass.
    this.closePromise ??= Promise.resolve().then(() => this.closeOnce());
    return this.closePromise;
  }

  private assertOpen(): void {
    if (this.closePromise) throw new Error('daemon resources are closing');
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
      this.failures.push({ name, error });
      try {
        this.onCloseError(name, error);
      } catch {
        /* diagnostics cannot interrupt cleanup */
      }
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
    // Refuse native ingress first. Quiescing core maintenance prevents expiry or
    // retention callbacks from racing the later transport/store phases.
    let storeSafe = await this.attempt('http.quiesce', this.http?.quiesce);
    storeSafe = (await this.attempt('core.quiesce', this.core?.quiesce)) && storeSafe;

    // Stop and join every producer before discovery withdraws or downstream
    // resources close. This includes boot work, agents polling and LAN refresh.
    storeSafe = (await this.closeGroup(this.producers)) && storeSafe;
    await this.closeGroup(this.discovery);

    // Hook responses must be settled while the HTTP transport is still alive.
    storeSafe = (await this.attempt('http.releaseHolds', this.http?.releaseHolds)) && storeSafe;
    storeSafe = (await this.attempt('http.close', this.http?.close)) && storeSafe;

    // Core close joins retention/question maintenance. Only after every store
    // user is gone may SQLite close; pid ownership is released last.
    storeSafe = (await this.attempt('core.close', this.core?.close)) && storeSafe;
    // A rejected upstream close cannot prove that its DB-using callbacks are
    // gone. The process will still release the pid and exit non-zero, at which
    // point the OS closes SQLite; closing it here would permit a late callback
    // to run against a retired handle in the meantime.
    if (this.store && storeSafe) await this.attempt(this.store.name, this.store.owner.close);
    if (this.process) await this.attempt(this.process.name, this.process.owner.close);
  }
}
