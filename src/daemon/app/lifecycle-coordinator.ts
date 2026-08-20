import * as Effect from 'effect/Effect';

/** Policy order from docs/v1/effect-migration-plan.md section 6. */
export const ShutdownPhaseOrder = Object.freeze([
  'quiescing',
  'stopping-producers',
  'withdrawing',
  'releasing-holds',
  'closing-clients',
  'closing-http',
  'closing-store',
  'releasing-process',
] as const);

export type ShutdownPhase = (typeof ShutdownPhaseOrder)[number];
export type LifecycleState = 'running' | ShutdownPhase | 'closed';

export type ShutdownTrigger =
  | { readonly _tag: 'Requested'; readonly reason?: string }
  | { readonly _tag: 'Success' }
  | { readonly _tag: 'Failure'; readonly error: unknown }
  | { readonly _tag: 'Defect'; readonly defect: unknown }
  | {
      readonly _tag: 'Interruption';
      readonly signal?: 'SIGINT' | 'SIGTERM';
    };

export type ShutdownForceReason =
  | { readonly _tag: 'SecondSignal'; readonly signal: 'SIGINT' | 'SIGTERM' }
  | { readonly _tag: 'SignalDuringShutdown'; readonly signal: 'SIGINT' | 'SIGTERM' }
  | { readonly _tag: 'DeadlineReserve'; readonly reserveMs: number }
  | { readonly _tag: 'DeadlineExceeded' }
  | { readonly _tag: 'External'; readonly reason?: string };

export interface ShutdownForceSignal {
  readonly reason: ShutdownForceReason;
}

type ForceListener = (signal: ShutdownForceSignal) => void;

/**
 * A host-callback-safe, one-way force signal.
 *
 * `force` only mutates local JavaScript state, resolves one already-created
 * Promise, and invokes already-registered synchronous callbacks. It never
 * creates an Effect, runtime, timer, or finalizer. Listener failures are
 * deliberately contained: a signal handler must never throw before the root
 * coordinator has had a chance to continue cleanup.
 */
export class ForceLatch {
  private forcedSignal: ShutdownForceSignal | null = null;
  private readonly listeners = new Set<ForceListener>();
  private readonly forcePromise: Promise<ShutdownForceSignal>;
  private resolveForce: (signal: ShutdownForceSignal) => void = () => undefined;

  constructor() {
    this.forcePromise = new Promise<ShutdownForceSignal>((resolve) => {
      this.resolveForce = resolve;
    });
  }

  get forced(): boolean {
    return this.forcedSignal !== null;
  }

  get signal(): ShutdownForceSignal | null {
    return this.forcedSignal;
  }

  /** Returns true only for the transition that opened the latch. */
  force(reason: ShutdownForceReason): boolean {
    if (this.forcedSignal) return false;

    const signal: ShutdownForceSignal = { reason };
    this.forcedSignal = signal;
    this.resolveForce(signal);

    const listeners = [...this.listeners];
    this.listeners.clear();
    for (const listener of listeners) this.notify(listener, signal);
    return true;
  }

  /**
   * Register a synchronous escalation callback. A callback registered after
   * force observes the same signal immediately. The returned removal is
   * idempotent.
   */
  onForce(listener: ForceListener): () => void {
    const signal = this.forcedSignal;
    if (signal) {
      this.notify(listener, signal);
      return () => undefined;
    }

    this.listeners.add(listener);
    let registered = true;
    return () => {
      if (!registered) return;
      registered = false;
      this.listeners.delete(listener);
    };
  }

  /** One shared completion; callers do not allocate per-signal waiters. */
  whenForced(): Promise<ShutdownForceSignal> {
    return this.forcePromise;
  }

  private notify(listener: ForceListener, signal: ShutdownForceSignal): void {
    try {
      listener(signal);
    } catch {
      // The coordinator records failures from its own escalation callbacks.
      // Unknown observers cannot be allowed to break a host signal callback.
    }
  }
}

export type MonotonicNow = () => number;

function liveMonotonicNow(): number {
  return globalThis.performance.now();
}

function finiteMilliseconds(value: number, label: string): number {
  if (!Number.isFinite(value)) throw new RangeError(`${label} must be finite`);
  return value;
}

/**
 * One absolute, monotonic shutdown deadline shared by every phase.
 *
 * The sampled clock is clamped against its last observation. This makes the
 * budget non-increasing even when an injected/test clock is imperfect; the
 * production clock is `performance.now()`, whose origin is intentionally not
 * serialized or compared with wall time.
 */
export class ShutdownBudget {
  readonly startedAtMs: number;
  readonly deadlineMs: number;
  private lastObservedMs: number;
  private readonly monotonicNow: MonotonicNow;

  private constructor(timeoutMs: number, monotonicNow: MonotonicNow) {
    if (timeoutMs < 0) throw new RangeError('shutdown timeout must be non-negative');
    const startedAtMs = finiteMilliseconds(monotonicNow(), 'monotonic clock reading');
    const deadlineMs = finiteMilliseconds(startedAtMs + timeoutMs, 'shutdown deadline');
    this.monotonicNow = monotonicNow;
    this.startedAtMs = startedAtMs;
    this.deadlineMs = deadlineMs;
    this.lastObservedMs = startedAtMs;
  }

  static start(timeoutMs: number, monotonicNow: MonotonicNow = liveMonotonicNow): ShutdownBudget {
    return new ShutdownBudget(finiteMilliseconds(timeoutMs, 'shutdown timeout'), monotonicNow);
  }

  nowMs(): number {
    const sampled = finiteMilliseconds(this.monotonicNow(), 'monotonic clock reading');
    this.lastObservedMs = Math.max(this.lastObservedMs, sampled);
    return this.lastObservedMs;
  }

  remainingMs(): number {
    return Math.max(0, this.deadlineMs - this.nowMs());
  }

  get expired(): boolean {
    return this.remainingMs() <= 0;
  }
}

export interface ShutdownPhaseContext {
  readonly phase: ShutdownPhase;
  readonly trigger: ShutdownTrigger;
  readonly budget: ShutdownBudget;
  readonly forceLatch: ForceLatch;
  readonly startedAtMs: number;
  readonly deadlineMs: number;
  readonly remainingMs: () => number;
  readonly isForced: () => boolean;
}

type MaybePromise = void | PromiseLike<void>;

/**
 * P4.2's adapter-sized seam over the P1 owners. `run` must synchronously start
 * the named phase and return its completion when one exists. `force` must be
 * synchronous, non-blocking, and idempotent; it is invoked at most once for
 * this phase by the coordinator.
 */
export interface ShutdownPhaseOperation {
  readonly run: (context: ShutdownPhaseContext) => MaybePromise;
  readonly force?: (context: ShutdownPhaseContext, signal: ShutdownForceSignal) => void;
}

export type LifecycleOwner = Readonly<Record<ShutdownPhase, ShutdownPhaseOperation>>;

interface PhaseOutcomeBase {
  readonly phase: ShutdownPhase;
  readonly startedAtMs: number;
  readonly finishedAtMs: number;
  readonly remainingMs: number;
}

export interface ShutdownPhaseCompleted extends PhaseOutcomeBase {
  readonly _tag: 'Completed';
}

export interface ShutdownPhaseFailed extends PhaseOutcomeBase {
  readonly _tag: 'Failed';
  readonly error: unknown;
}

export interface ShutdownPhaseForced extends PhaseOutcomeBase {
  readonly _tag: 'Forced';
  readonly signal: ShutdownForceSignal;
}

export interface ShutdownPhaseTimedOut extends PhaseOutcomeBase {
  readonly _tag: 'TimedOut';
}

export interface ShutdownPhaseSkipped extends PhaseOutcomeBase {
  readonly _tag: 'Skipped';
  readonly reason: 'store-unsafe';
}

export type ShutdownPhaseOutcome =
  | ShutdownPhaseCompleted
  | ShutdownPhaseFailed
  | ShutdownPhaseForced
  | ShutdownPhaseTimedOut
  | ShutdownPhaseSkipped;

export interface ShutdownForceFailure {
  readonly phase: ShutdownPhase;
  readonly error: unknown;
}

export interface LifecycleCloseOutcome {
  readonly _tag: 'LifecycleCloseOutcome';
  readonly trigger: ShutdownTrigger;
  readonly startedAtMs: number;
  readonly deadlineMs: number;
  readonly finishedAtMs: number;
  readonly forced: boolean;
  readonly forceSignal: ShutdownForceSignal | null;
  readonly deadlineExpired: boolean;
  readonly phases: readonly ShutdownPhaseOutcome[];
  readonly failures: readonly ShutdownPhaseFailed[];
  readonly forceFailures: readonly ShutdownForceFailure[];
}

export interface LifecycleCoordinatorOptions {
  /** Whole-daemon budget; phases never receive a fresh relative timeout. */
  readonly timeoutMs: number;
  /**
   * Open the force latch this many milliseconds before the absolute deadline.
   * The active owner is still awaited only until that original deadline. Zero
   * preserves deadline-only escalation for callers without a forced-cleanup
   * reserve.
   */
  readonly forceReserveMs?: number;
  readonly monotonicNow?: MonotonicNow;
  readonly forceLatch?: ForceLatch;
}

type PhaseRaceResult =
  | { readonly _tag: 'Completed' }
  | { readonly _tag: 'Failed'; readonly error: unknown }
  | { readonly _tag: 'Forced'; readonly signal: ShutdownForceSignal }
  | { readonly _tag: 'TimedOut' };

interface DeadlineWait {
  readonly completion: Promise<PhaseRaceResult>;
  readonly cancel: () => void;
}

const MAX_TIMER_MS = 2_147_483_647;
const DEFAULT_TRIGGER: ShutdownTrigger = { _tag: 'Requested' };

function phaseContext(
  phase: ShutdownPhase,
  trigger: ShutdownTrigger,
  budget: ShutdownBudget,
  forceLatch: ForceLatch,
  startedAtMs: number,
): ShutdownPhaseContext {
  return {
    phase,
    trigger,
    budget,
    forceLatch,
    startedAtMs,
    deadlineMs: budget.deadlineMs,
    remainingMs: () => budget.remainingMs(),
    isForced: () => forceLatch.forced,
  };
}

function settledOperation(result: MaybePromise): Promise<PhaseRaceResult> {
  return Promise.resolve(result).then<PhaseRaceResult, PhaseRaceResult>(
    () => ({ _tag: 'Completed' }),
    (error: unknown) => ({ _tag: 'Failed', error }),
  );
}

function deadlineWait(
  budget: ShutdownBudget,
  forceReserveMs: number,
  onForceReserve: () => void,
  onDeadline: () => void,
): DeadlineWait {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let cancelled = false;
  let reserveOpened = false;
  let settle: (result: PhaseRaceResult) => void = () => undefined;
  const completion = new Promise<PhaseRaceResult>((resolve) => {
    settle = resolve;
  });

  const arm = (): void => {
    if (cancelled) return;
    const remainingMs = budget.remainingMs();
    if (remainingMs <= 0) {
      // Publish timeout before force resolves its Promise so the deadline wins
      // this phase's race while escalation still happens synchronously.
      settle({ _tag: 'TimedOut' });
      onDeadline();
      return;
    }

    if (!reserveOpened && forceReserveMs > 0 && remainingMs <= forceReserveMs) {
      reserveOpened = true;
      onForceReserve();
    }

    const untilReserve =
      !reserveOpened && forceReserveMs > 0
        ? Math.max(0, remainingMs - forceReserveMs)
        : remainingMs;
    timer = setTimeout(
      arm,
      Math.max(1, Math.ceil(Math.min(untilReserve, remainingMs, MAX_TIMER_MS))),
    );
    timer.unref?.();
  };
  arm();

  return {
    completion,
    cancel() {
      if (cancelled) return;
      cancelled = true;
      if (timer) clearTimeout(timer);
      timer = null;
    },
  };
}

/**
 * Drives Fleet Deck's policy-sized shutdown sequence exactly once.
 *
 * Construction and `close()` are cold: owner callbacks, clock reads, and
 * timers begin only when the returned Effect is executed. Every execution of
 * that same Effect observes one shared completion, so concurrent/double close
 * cannot duplicate a phase or finalizer. The close Effect is uninterruptible;
 * blocking owner waits remain bounded by the one absolute ShutdownBudget.
 */
export class LifecycleCoordinator {
  private readonly owner: LifecycleOwner;
  private readonly forceLatch: ForceLatch;
  private readonly timeoutMs: number;
  private readonly forceReserveMs: number;
  private readonly monotonicNow: MonotonicNow;
  private currentState: LifecycleState = 'running';
  private closeRequested = false;
  private trigger: ShutdownTrigger = DEFAULT_TRIGGER;
  private closeEffect: Effect.Effect<LifecycleCloseOutcome> | null = null;
  private closePromise: Promise<LifecycleCloseOutcome> | null = null;
  private readonly forceFailures: ShutdownForceFailure[] = [];

  constructor(owner: LifecycleOwner, options: LifecycleCoordinatorOptions) {
    if (!Number.isFinite(options.timeoutMs) || options.timeoutMs < 0) {
      throw new RangeError('lifecycle timeout must be a finite non-negative number');
    }
    const forceReserveMs = options.forceReserveMs ?? 0;
    if (
      !Number.isFinite(forceReserveMs) ||
      forceReserveMs < 0 ||
      forceReserveMs > options.timeoutMs
    ) {
      throw new RangeError(
        'lifecycle force reserve must be finite, non-negative, and no greater than timeout',
      );
    }
    this.timeoutMs = options.timeoutMs;
    this.forceReserveMs = forceReserveMs;
    this.owner = owner;
    this.monotonicNow = options.monotonicNow ?? liveMonotonicNow;
    this.forceLatch = options.forceLatch ?? new ForceLatch();
  }

  get state(): LifecycleState {
    return this.currentState;
  }

  get closing(): boolean {
    return this.closeRequested;
  }

  get forced(): boolean {
    return this.forceLatch.forced;
  }

  /** Callback-safe escalation entrypoint for the host signal observer. */
  force(reason: ShutdownForceReason): boolean {
    if (this.currentState === 'closed') return false;
    return this.forceLatch.force(reason);
  }

  /**
   * Returns the exact same cold Effect for every call. The first trigger wins;
   * later callers participate in the same close and may escalate via `force`.
   */
  close(trigger: ShutdownTrigger = DEFAULT_TRIGGER): Effect.Effect<LifecycleCloseOutcome> {
    if (this.closeEffect) return this.closeEffect;
    // Publish closing synchronously so a host callback arriving between Effect
    // construction and its first instruction can escalate the same close.
    this.closeRequested = true;
    this.trigger = trigger;
    this.closeEffect = Effect.uninterruptible(Effect.promise(() => this.ensureCloseStarted()));
    return this.closeEffect;
  }

  private ensureCloseStarted(): Promise<LifecycleCloseOutcome> {
    if (this.closePromise) return this.closePromise;

    let resolveClose: (outcome: LifecycleCloseOutcome) => void = () => undefined;
    let rejectClose: (error: unknown) => void = () => undefined;
    // Publish before invoking any owner. A phase callback may re-enter close()
    // or cause another consumer to execute the memoized Effect immediately.
    this.closePromise = new Promise<LifecycleCloseOutcome>((resolve, reject) => {
      resolveClose = resolve;
      rejectClose = reject;
    });
    void this.runClose().then(resolveClose, rejectClose);
    return this.closePromise;
  }

  private async runClose(): Promise<LifecycleCloseOutcome> {
    const budget = ShutdownBudget.start(this.timeoutMs, this.monotonicNow);
    const phases: ShutdownPhaseOutcome[] = [];
    let storeSafe = true;
    try {
      for (const phase of ShutdownPhaseOrder) {
        if (phase === 'closing-store' && !storeSafe) {
          phases.push(this.skipUnsafeStore(budget));
          continue;
        }

        const outcome = await this.runPhase(phase, budget);
        phases.push(outcome);
        // Only an actual join failure or deadline loss makes downstream store
        // retirement unsafe. A forced phase that subsequently settled has
        // still discharged its join obligation and may keep the proof intact.
        if (outcome._tag === 'Failed' || outcome._tag === 'TimedOut') storeSafe = false;
      }
    } finally {
      this.currentState = 'closed';
    }

    const failures = phases.filter(
      (outcome): outcome is ShutdownPhaseFailed => outcome._tag === 'Failed',
    );
    const finishedAtMs = budget.nowMs();
    return {
      _tag: 'LifecycleCloseOutcome',
      trigger: this.trigger,
      startedAtMs: budget.startedAtMs,
      deadlineMs: budget.deadlineMs,
      finishedAtMs,
      forced: this.forceLatch.forced,
      forceSignal: this.forceLatch.signal,
      deadlineExpired:
        phases.some((outcome) => outcome._tag === 'TimedOut') || finishedAtMs >= budget.deadlineMs,
      phases,
      failures,
      forceFailures: [...this.forceFailures],
    };
  }

  private skipUnsafeStore(budget: ShutdownBudget): ShutdownPhaseSkipped {
    this.currentState = 'closing-store';
    const startedAtMs = budget.nowMs();
    const finishedAtMs = budget.nowMs();
    return {
      _tag: 'Skipped',
      reason: 'store-unsafe',
      phase: 'closing-store',
      startedAtMs,
      finishedAtMs,
      remainingMs: Math.max(0, budget.deadlineMs - finishedAtMs),
    };
  }

  private async runPhase(
    phase: ShutdownPhase,
    budget: ShutdownBudget,
  ): Promise<ShutdownPhaseOutcome> {
    this.currentState = phase;
    const startedAtMs = budget.nowMs();
    const context = phaseContext(phase, this.trigger, budget, this.forceLatch, startedAtMs);
    const operation = this.owner[phase];

    let operationCompletion: Promise<PhaseRaceResult>;
    let synchronousFailure: unknown;
    let failedSynchronously = false;
    try {
      // Invoke exactly once before observing force/deadline. Even forced cleanup
      // must initiate every ordered phase rather than silently skip finalizers.
      operationCompletion = settledOperation(operation.run(context));
    } catch (error) {
      // A phase may acquire or wake part of its owner before throwing. Preserve
      // that failure, but continue through force observation below so a
      // pre-open latch or an expired budget still runs its synchronous cleanup
      // callback exactly once.
      synchronousFailure = error;
      failedSynchronously = true;
      operationCompletion = Promise.resolve({ _tag: 'Failed', error });
    }

    let forceInvoked = false;
    let observedForce: ShutdownForceSignal | null = null;
    const invokeForce = (signal: ShutdownForceSignal): void => {
      if (forceInvoked) return;
      forceInvoked = true;
      observedForce = signal;
      try {
        operation.force?.(context, signal);
      } catch (error) {
        this.forceFailures.push({ phase, error });
      }
    };
    const removeForceListener = this.forceLatch.onForce(invokeForce);

    // Still call run() above when the deadline is already exhausted. That is
    // the exact once-only cleanup attempt; force then supplies its synchronous
    // escalation and no expired phase is awaited.
    if (budget.remainingMs() <= 0) {
      this.forceLatch.force({ _tag: 'DeadlineExceeded' });
      removeForceListener();
      if (failedSynchronously) {
        return this.finishPhase(
          { _tag: 'Failed', error: synchronousFailure },
          phase,
          startedAtMs,
          budget,
        );
      }
      return this.finishPhase({ _tag: 'TimedOut' }, phase, startedAtMs, budget);
    }

    if (
      !this.forceLatch.forced &&
      this.forceReserveMs > 0 &&
      budget.remainingMs() <= this.forceReserveMs
    ) {
      this.forceLatch.force({ _tag: 'DeadlineReserve', reserveMs: this.forceReserveMs });
    }

    if (failedSynchronously) {
      removeForceListener();
      return this.finishPhase(
        { _tag: 'Failed', error: synchronousFailure },
        phase,
        startedAtMs,
        budget,
      );
    }

    const deadline = deadlineWait(
      budget,
      this.forceReserveMs,
      () => {
        this.forceLatch.force({ _tag: 'DeadlineReserve', reserveMs: this.forceReserveMs });
      },
      () => {
        this.forceLatch.force({ _tag: 'DeadlineExceeded' });
      },
    );

    let result: PhaseRaceResult;
    try {
      // Force accelerates the owner but does not discharge its join obligation.
      // Only actual owner settlement or the shared absolute deadline advances
      // to a phase that may release downstream resources such as SQLite.
      result = await Promise.race([operationCompletion, deadline.completion]);
    } finally {
      deadline.cancel();
      removeForceListener();
    }
    if (result._tag === 'Completed' && observedForce) {
      return this.finishPhase(
        { _tag: 'Forced', signal: observedForce },
        phase,
        startedAtMs,
        budget,
      );
    }
    return this.finishPhase(result, phase, startedAtMs, budget);
  }

  private finishPhase(
    result: PhaseRaceResult,
    phase: ShutdownPhase,
    startedAtMs: number,
    budget: ShutdownBudget,
  ): ShutdownPhaseOutcome {
    const finishedAtMs = budget.nowMs();
    const base: PhaseOutcomeBase = {
      phase,
      startedAtMs,
      finishedAtMs,
      remainingMs: Math.max(0, budget.deadlineMs - finishedAtMs),
    };
    switch (result._tag) {
      case 'Completed':
        return { _tag: 'Completed', ...base };
      case 'Failed':
        return { _tag: 'Failed', ...base, error: result.error };
      case 'Forced':
        return { _tag: 'Forced', ...base, signal: result.signal };
      case 'TimedOut':
        return { _tag: 'TimedOut', ...base };
    }
  }
}
