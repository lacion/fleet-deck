import * as Context from 'effect/Context';
import type * as Effect from 'effect/Effect';
import type * as Exit from 'effect/Exit';
import type * as Fiber from 'effect/Fiber';
import type { ApplicationQuiescingError } from '../errors.ts';

export type IngressSupervisorState = 'open' | 'quiescing' | 'closed';

/**
 * Host-runner options deliberately exclude `onFiberStart` and
 * `uninterruptible`. Fiber ownership belongs to the supervisor, and ingress
 * work must remain interruptible before downstream resources can close.
 */
export type IngressRunOptions = Pick<Effect.RunOptions, 'scheduler' | 'signal'>;

export interface IngressCallbackOptions<A, E> extends IngressRunOptions {
  readonly onExit: (exit: Exit.Exit<A, E | ApplicationQuiescingError>) => void;
}

/**
 * The sole imperative callback edge into one already-built root Context.
 *
 * `Services` is the exact service union captured by the platform
 * implementation. This keeps missing root services as compile-time errors
 * without exposing the Context itself or allowing a callback to build a Layer.
 * The service's registry, rather than an RC.110 `Fiber.runIn` Scope finalizer,
 * is the authoritative owner: policy close joins it before resource retirement,
 * and an absolute-deadline loss remains observable as an unsafe/incomplete join.
 */
export interface IngressSupervisorService<Services> {
  readonly state: IngressSupervisorState;
  readonly activeCount: number;

  readonly runFork: <A, E>(
    operation: string,
    effect: Effect.Effect<A, E, Services>,
    options?: IngressRunOptions,
  ) => Fiber.Fiber<A, E | ApplicationQuiescingError>;

  readonly runCallback: <A, E>(
    operation: string,
    effect: Effect.Effect<A, E, Services>,
    options: IngressCallbackOptions<A, E>,
  ) => (interruptor?: number) => void;

  readonly runPromise: <A, E>(
    operation: string,
    effect: Effect.Effect<A, E, Services>,
    options?: IngressRunOptions,
  ) => Promise<A>;

  /**
   * Preserve the complete Effect exit for compatibility interpreters that must
   * distinguish typed failure, interruption, and defect without FiberFailure
   * rejection/squashing.
   */
  readonly runPromiseExit: <A, E>(
    operation: string,
    effect: Effect.Effect<A, E, Services>,
    options?: IngressRunOptions,
  ) => Promise<Exit.Exit<A, E | ApplicationQuiescingError>>;

  /** Synchronously close admission without interrupting already-admitted work. */
  readonly quiesce: () => void;
  /** Quiesce and synchronously request interruption without awaiting cleanup. */
  readonly interrupt: () => void;
  /** Quiesce and wait for already-admitted work to settle naturally. */
  readonly join: () => Promise<void>;
  /**
   * Quiesce, interrupt all admitted work, and join it. Idempotent by identity;
   * the lifecycle coordinator must race this wait with its absolute deadline
   * and must not close store resources if this wait loses that race.
   */
  readonly close: () => Promise<void>;
}

/**
 * Existential root service used by Layer composition. The scoped constructor
 * is generic and captures the complete root service union; platform callbacks
 * may then submit only Effects whose requirements are present in that root.
 */
export type RootIngressSupervisorService = IngressSupervisorService<unknown>;

export class IngressSupervisor extends Context.Service<
  IngressSupervisor,
  RootIngressSupervisorService
>()('fleetdeck/daemon/app/IngressSupervisor') {}
