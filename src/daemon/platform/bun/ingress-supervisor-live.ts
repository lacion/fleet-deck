import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import type * as Fiber from 'effect/Fiber';
import * as Layer from 'effect/Layer';
import * as Scope from 'effect/Scope';
import { ApplicationQuiescingError } from '../../app/errors.ts';
import { IngressSupervisor } from '../../app/services/ingress-supervisor.ts';
import type {
  IngressCallbackOptions,
  IngressRunOptions,
  IngressSupervisorService,
  IngressSupervisorState,
} from '../../app/services/ingress-supervisor.ts';

type ContextForkRunner<Services> = <A, E>(
  effect: Effect.Effect<A, E, Services>,
  options?: Effect.RunOptions,
) => Fiber.Fiber<A, E>;

type ContextCallbackRunner<Services> = <A, E>(
  effect: Effect.Effect<A, E, Services>,
  options?: Effect.RunOptions & { readonly onExit: (exit: Exit.Exit<A, E>) => void },
) => (interruptor?: number) => void;

type ContextPromiseRunner<Services> = <A, E>(
  effect: Effect.Effect<A, E, Services>,
  options?: Effect.RunOptions,
) => Promise<A>;

class LiveIngressSupervisor<Services> implements IngressSupervisorService<Services> {
  private phase: IngressSupervisorState = 'open';
  private readonly active = new Set<Fiber.Fiber<unknown, unknown>>();
  private readonly pendingPromises = new Set<Promise<unknown>>();
  private starting = 0;
  private joinPromise: Promise<void> | null = null;
  private resolveJoin: () => void = () => undefined;
  private closePromise: Promise<void> | null = null;
  private readonly runForkWithContext: ContextForkRunner<Services>;
  private readonly runCallbackWithContext: ContextCallbackRunner<Services>;
  private readonly runPromiseWithContext: ContextPromiseRunner<Services>;
  private readonly rootScope: Scope.Scope;

  constructor(context: Context.Context<Services>, rootScope: Scope.Scope) {
    // Capture one already-built Context exactly once. These runners neither
    // construct a runtime nor rebuild/provide a Layer for individual requests.
    this.runForkWithContext = Effect.runForkWith(context);
    this.runCallbackWithContext = Effect.runCallbackWith(context);
    this.runPromiseWithContext = Effect.runPromiseWith(context);
    this.rootScope = rootScope;
  }

  get state(): IngressSupervisorState {
    return this.phase;
  }

  get activeCount(): number {
    return this.active.size;
  }

  readonly runFork = <A, E>(
    operation: string,
    effect: Effect.Effect<A, E, Services>,
    options?: IngressRunOptions,
  ): Fiber.Fiber<A, E | ApplicationQuiescingError> => {
    const refusal = this.refusal(operation);
    if (refusal) return this.runForkWithContext(Effect.fail(refusal));

    this.starting++;
    try {
      const runnable: Effect.Effect<A, E | ApplicationQuiescingError, Services> =
        options?.signal?.aborted === true ? Effect.interrupt : effect;
      return this.runForkWithContext(runnable, {
        ...options,
        onFiberStart: this.trackFiber,
      });
    } finally {
      this.finishStarting();
    }
  };

  readonly runCallback = <A, E>(
    operation: string,
    effect: Effect.Effect<A, E, Services>,
    options: IngressCallbackOptions<A, E>,
  ): ((interruptor?: number) => void) => {
    const refusal = this.refusal(operation);
    if (refusal) {
      options.onExit(Exit.fail(refusal));
      return () => undefined;
    }

    this.starting++;
    try {
      const runnable: Effect.Effect<A, E | ApplicationQuiescingError, Services> =
        options.signal?.aborted === true ? Effect.interrupt : effect;
      return this.runCallbackWithContext(runnable, {
        ...options,
        onFiberStart: this.trackFiber,
      });
    } finally {
      this.finishStarting();
    }
  };

  readonly runPromise = <A, E>(
    operation: string,
    effect: Effect.Effect<A, E, Services>,
    options?: IngressRunOptions,
  ): Promise<A> => {
    const refusal = this.refusal(operation);
    if (refusal) return Promise.reject(refusal);

    this.starting++;
    try {
      const runnable: Effect.Effect<A, E | ApplicationQuiescingError, Services> =
        options?.signal?.aborted === true ? Effect.interrupt : effect;
      const promise = this.runPromiseWithContext(runnable, {
        ...options,
        onFiberStart: this.trackFiber,
      });
      this.trackPromise(promise);
      return promise;
    } catch (defect) {
      return Promise.reject(defect);
    } finally {
      this.finishStarting();
    }
  };

  readonly runPromiseExit = <A, E>(
    operation: string,
    effect: Effect.Effect<A, E, Services>,
    options?: IngressRunOptions,
  ): Promise<Exit.Exit<A, E | ApplicationQuiescingError>> => {
    const refusal = this.refusal(operation);
    if (refusal) return Promise.resolve(Exit.fail(refusal));

    this.starting++;
    try {
      const runnable: Effect.Effect<A, E | ApplicationQuiescingError, Services> =
        options?.signal?.aborted === true ? Effect.interrupt : effect;
      const promise = this.runPromiseWithContext(Effect.exit(runnable), {
        ...options,
        onFiberStart: this.trackFiber,
      });
      this.trackPromise(promise);
      return promise;
    } catch (defect) {
      return Promise.reject(defect);
    } finally {
      this.finishStarting();
    }
  };

  readonly quiesce = (): void => {
    if (this.phase !== 'open') return;
    this.phase = 'quiescing';
    this.maybeResolveJoin();
  };

  readonly interrupt = (): void => {
    this.quiesce();
    for (const fiber of [...this.active]) fiber.interruptUnsafe();
  };

  readonly join = (): Promise<void> => {
    if (this.joinPromise) return this.joinPromise;

    this.quiesce();
    this.joinPromise = new Promise<void>((resolve) => {
      this.resolveJoin = resolve;
    });
    this.maybeResolveJoin();
    return this.joinPromise;
  };

  readonly close = (): Promise<void> => {
    if (this.closePromise) return this.closePromise;

    let resolveClose: () => void = () => undefined;
    this.closePromise = new Promise<void>((resolve) => {
      resolveClose = resolve;
    });

    this.interrupt();
    void this.join().then(() => {
      this.phase = 'closed';
      resolveClose();
    });
    return this.closePromise;
  };

  private refusal(operation: string): ApplicationQuiescingError | null {
    if (this.phase === 'open' && this.rootScope.state._tag === 'Closed') this.quiesce();
    return this.phase === 'open'
      ? null
      : new ApplicationQuiescingError({
          operation,
          message: `Application is quiescing; refused ${operation}`,
        });
  }

  private readonly trackFiber = (fiber: Fiber.Fiber<unknown, unknown>): void => {
    this.active.add(fiber);
    fiber.addObserver(() => {
      this.active.delete(fiber);
      this.maybeResolveJoin();
    });

    // An Effect may synchronously trigger quiesce while run*With is still
    // evaluating it, before RC.110 invokes onFiberStart. Close that race here.
    if (this.phase !== 'open') fiber.interruptUnsafe();
  };

  private trackPromise(promise: Promise<unknown>): void {
    this.pendingPromises.add(promise);
    const settled = (): void => {
      this.pendingPromises.delete(promise);
      this.maybeResolveJoin();
    };
    void promise.then(settled, settled);
  }

  private finishStarting(): void {
    this.starting--;
    this.maybeResolveJoin();
  }

  private maybeResolveJoin(): void {
    if (
      this.phase !== 'open' &&
      this.joinPromise &&
      this.starting === 0 &&
      this.active.size === 0 &&
      this.pendingPromises.size === 0
    ) {
      this.resolveJoin();
    }
  }
}

/**
 * Construct the root-owned callback bridge from one already-built Context and
 * register its idempotent fallback finalizer in the root Scope. The fallback
 * deliberately does not start a second `join`.
 *
 * RC.110's `Fiber.runIn` installs a Scope finalizer that interrupts *and awaits*
 * the fiber. That hidden second join can outlive the coordinator's one absolute
 * deadline when interruption cleanup is stuck. The explicit registry above is
 * therefore the sole owner of callback fibers: normal policy close interrupts
 * and joins every tracked fiber before downstream resources close; a timed-out
 * join makes the coordinator's store-safety proof fail, while root Scope
 * fallback can still finish without re-awaiting the same stuck cleanup.
 */
export function makeIngressSupervisor<Services>(
  context: Context.Context<Services>,
  rootScope: Scope.Scope,
): Effect.Effect<IngressSupervisorService<Services>> {
  const supervisor = new LiveIngressSupervisor(context, rootScope);
  return Scope.addFinalizer(rootScope, Effect.sync(supervisor.interrupt)).pipe(
    Effect.as(supervisor),
  );
}

/**
 * Scoped definition for root composition. Layer construction captures its
 * already-provided Context once; the only cast is the existential service tag
 * boundary, while direct construction retains the exact `Services` union.
 */
export function makeIngressSupervisorLayer<Services>(): Layer.Layer<
  IngressSupervisor,
  never,
  Services
> {
  const acquire = Effect.gen(function* () {
    const context = yield* Effect.context<Services>();
    const rootScope = yield* Effect.scope;
    const supervisor = yield* makeIngressSupervisor(context, rootScope);
    return supervisor as IngressSupervisorService<unknown>;
  });
  return Layer.effect(IngressSupervisor, acquire);
}
