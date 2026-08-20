import assert from 'node:assert/strict';
import { describe, test } from 'bun:test';
import * as Exit from 'effect/Exit';
import {
  ForceLatch,
  type LifecycleCloseOutcome,
  LifecycleCoordinator,
  type LifecycleOwner,
  ShutdownBudget,
  type ShutdownPhase,
  ShutdownPhaseOrder,
  type ShutdownPhaseOperation,
} from '../../src/daemon/app/lifecycle-coordinator.ts';
import { runEffectExit } from './helpers.ts';

const immediateOperation: ShutdownPhaseOperation = { run: () => undefined };

function ownerWith(
  operation: (phase: ShutdownPhase) => ShutdownPhaseOperation = () => immediateOperation,
): LifecycleOwner {
  return Object.fromEntries(
    ShutdownPhaseOrder.map((phase) => [phase, operation(phase)]),
  ) as LifecycleOwner;
}

async function runClose(coordinator: LifecycleCoordinator): Promise<LifecycleCloseOutcome> {
  const exit = await runEffectExit(coordinator.close());
  assert.ok(Exit.isSuccess(exit));
  return exit.value;
}

describe('LifecycleCoordinator', () => {
  test('construction and close Effect creation acquire nothing', async () => {
    let clockReads = 0;
    let phaseRuns = 0;
    const owner = ownerWith(() => ({
      run() {
        phaseRuns++;
      },
    }));
    const coordinator = new LifecycleCoordinator(owner, {
      timeoutMs: 1_000,
      monotonicNow: () => {
        clockReads++;
        return 100;
      },
    });

    const close = coordinator.close({ _tag: 'Requested', reason: 'test' });
    assert.equal(coordinator.state, 'running');
    assert.equal(coordinator.closing, true, 'close intent is callback-visible before execution');
    assert.equal(clockReads, 0);
    assert.equal(phaseRuns, 0);

    const outcome = await runClose(coordinator);
    assert.equal(outcome.trigger._tag, 'Requested');
    assert.equal(clockReads > 0, true);
    assert.equal(phaseRuns, ShutdownPhaseOrder.length);
    assert.equal(close, coordinator.close(), 'close returns one cold Effect identity');
  });

  test('executes the exact section 6 order once', async () => {
    const calls: string[] = [];
    const coordinator = new LifecycleCoordinator(
      ownerWith((phase) => ({
        run(context) {
          assert.equal(context.phase, phase);
          assert.equal(context.deadlineMs, context.budget.deadlineMs);
          calls.push(phase);
        },
      })),
      { timeoutMs: 1_000 },
    );

    const outcome = await runClose(coordinator);
    assert.deepEqual(calls, [...ShutdownPhaseOrder]);
    assert.deepEqual(
      outcome.phases.map((phase) => phase.phase),
      [...ShutdownPhaseOrder],
    );
    assert.deepEqual(
      outcome.phases.map((phase) => phase._tag),
      ShutdownPhaseOrder.map(() => 'Completed'),
    );
    assert.equal(coordinator.state, 'closed');
    assert.equal(outcome.failures.length, 0);
    assert.equal(outcome.forceFailures.length, 0);
    assert.equal(
      coordinator.force({ _tag: 'External', reason: 'late signal' }),
      false,
      'a completed coordinator cannot be escalated or replay finalizers',
    );
  });

  test('double, concurrent, and re-entrant close share identity and finalizers', async () => {
    const calls = new Map<ShutdownPhase, number>();
    let coordinator: LifecycleCoordinator;
    let firstClose: ReturnType<LifecycleCoordinator['close']> | null = null;
    let reentrantClose: ReturnType<LifecycleCoordinator['close']> | null = null;
    coordinator = new LifecycleCoordinator(
      ownerWith((phase) => ({
        run() {
          calls.set(phase, (calls.get(phase) ?? 0) + 1);
          if (phase === 'quiescing') reentrantClose = coordinator.close();
        },
      })),
      { timeoutMs: 1_000 },
    );

    firstClose = coordinator.close({ _tag: 'Success' });
    const secondClose = coordinator.close({ _tag: 'Failure', error: new Error('ignored') });
    assert.equal(firstClose, secondClose);

    const [firstExit, secondExit] = await Promise.all([
      runEffectExit(firstClose),
      runEffectExit(secondClose),
    ]);
    assert.ok(Exit.isSuccess(firstExit));
    assert.ok(Exit.isSuccess(secondExit));
    assert.equal(firstExit.value, secondExit.value, 'all executions share one outcome object');
    assert.equal(reentrantClose, firstClose, 'phase re-entry observes the same close Effect');
    assert.equal(firstExit.value.trigger._tag, 'Success', 'the first close trigger wins');
    assert.deepEqual(
      Object.fromEntries(calls),
      Object.fromEntries(ShutdownPhaseOrder.map((phase) => [phase, 1])),
    );
  });

  test('every phase failure is retained and later phases still execute', async () => {
    const errors = new Map<ShutdownPhase, Error>();
    const calls: ShutdownPhase[] = [];
    const coordinator = new LifecycleCoordinator(
      ownerWith((phase) => ({
        run() {
          calls.push(phase);
          const error = new Error(`${phase} failed`);
          errors.set(phase, error);
          if (ShutdownPhaseOrder.indexOf(phase) % 2 === 0) throw error;
          return Promise.reject(error);
        },
      })),
      { timeoutMs: 1_000 },
    );

    const outcome = await runClose(coordinator);
    const runnablePhases = ShutdownPhaseOrder.filter((phase) => phase !== 'closing-store');
    assert.deepEqual(calls, runnablePhases);
    assert.equal(outcome.failures.length, runnablePhases.length);
    for (const phase of outcome.phases) {
      assert.equal(phase._tag, phase.phase === 'closing-store' ? 'Skipped' : 'Failed');
      if (phase._tag === 'Failed') assert.equal(phase.error, errors.get(phase.phase));
      if (phase._tag === 'Skipped') assert.equal(phase.reason, 'store-unsafe');
    }
  });

  test('an upstream join failure skips SQLite but still runs the final host-release phase', async () => {
    const failure = new Error('client join failed');
    const runs: ShutdownPhase[] = [];
    const coordinator = new LifecycleCoordinator(
      ownerWith((phase) => ({
        run() {
          runs.push(phase);
          if (phase === 'closing-clients') throw failure;
        },
      })),
      { timeoutMs: 1_000 },
    );

    const outcome = await runClose(coordinator);
    assert.equal(runs.includes('closing-store'), false, 'unsafe SQLite owner must not run');
    assert.equal(runs.at(-1), 'releasing-process', 'the host-release policy phase always runs');
    assert.deepEqual(outcome.phases[6], {
      _tag: 'Skipped',
      reason: 'store-unsafe',
      phase: 'closing-store',
      startedAtMs: outcome.phases[6]?.startedAtMs,
      finishedAtMs: outcome.phases[6]?.finishedAtMs,
      remainingMs: outcome.phases[6]?.remainingMs,
    });
    assert.equal(outcome.failures[0]?.error, failure);
  });

  test('one absolute deadline times out blocked and later phases without resetting their waits', async () => {
    const runs: ShutdownPhase[] = [];
    const forces: ShutdownPhase[] = [];
    const never = new Promise<void>(() => undefined);
    const coordinator = new LifecycleCoordinator(
      ownerWith((phase) => ({
        run() {
          runs.push(phase);
          return phase === 'quiescing' ? never : undefined;
        },
        force() {
          forces.push(phase);
        },
      })),
      { timeoutMs: 25 },
    );

    const outcome = await runClose(coordinator);
    const runnablePhases = ShutdownPhaseOrder.filter((phase) => phase !== 'closing-store');
    assert.deepEqual(runs, runnablePhases);
    assert.deepEqual(forces, runnablePhases);
    assert.equal(outcome.phases[0]?._tag, 'TimedOut');
    assert.deepEqual(
      outcome.phases.slice(1).map((phase) => phase._tag),
      ShutdownPhaseOrder.slice(1).map((phase) =>
        phase === 'closing-store' ? 'Skipped' : 'TimedOut',
      ),
    );
    assert.equal(outcome.deadlineExpired, true);
    assert.equal(outcome.forceSignal?.reason._tag, 'DeadlineExceeded');
  });

  test('opens one force reserve before the absolute deadline and still awaits owner settlement', async () => {
    let settleActive: () => void = () => undefined;
    const active = new Promise<void>((resolve) => {
      settleActive = resolve;
    });
    const runs: ShutdownPhase[] = [];
    const forces: ShutdownPhase[] = [];
    const coordinator = new LifecycleCoordinator(
      ownerWith((phase) => ({
        run() {
          runs.push(phase);
          return phase === 'quiescing' ? active : undefined;
        },
        force() {
          forces.push(phase);
          if (phase === 'quiescing') settleActive();
        },
      })),
      { timeoutMs: 500, forceReserveMs: 400 },
    );

    const outcome = await runClose(coordinator);
    assert.deepEqual(runs, [...ShutdownPhaseOrder]);
    assert.deepEqual(forces, [...ShutdownPhaseOrder]);
    assert.equal(outcome.forceSignal?.reason._tag, 'DeadlineReserve');
    if (outcome.forceSignal?.reason._tag === 'DeadlineReserve') {
      assert.equal(outcome.forceSignal.reason.reserveMs, 400);
    }
    assert.equal(outcome.deadlineExpired, false);
    assert.ok(outcome.finishedAtMs < outcome.deadlineMs);
    assert.deepEqual(
      outcome.phases.map(({ _tag }) => _tag),
      ShutdownPhaseOrder.map(() => 'Forced'),
    );
  });

  test('validates the force reserve against the one root timeout', () => {
    const owner = ownerWith();
    assert.throws(
      () => new LifecycleCoordinator(owner, { timeoutMs: 100, forceReserveMs: -1 }),
      /force reserve/,
    );
    assert.throws(
      () => new LifecycleCoordinator(owner, { timeoutMs: 100, forceReserveMs: 101 }),
      /force reserve/,
    );
    assert.throws(
      () =>
        new LifecycleCoordinator(owner, {
          timeoutMs: 100,
          forceReserveMs: Number.POSITIVE_INFINITY,
        }),
      /force reserve/,
    );
  });

  test('force escalates synchronously but the next phase waits for active owner settlement', async () => {
    let markStarted: () => void = () => undefined;
    const started = new Promise<void>((resolve) => {
      markStarted = resolve;
    });
    let settleQuiescing: () => void = () => undefined;
    const quiescingSettled = new Promise<void>((resolve) => {
      settleQuiescing = resolve;
    });
    const runs: ShutdownPhase[] = [];
    const forceCalls: ShutdownPhase[] = [];
    const coordinator = new LifecycleCoordinator(
      ownerWith((phase) => ({
        run() {
          runs.push(phase);
          if (phase === 'quiescing') {
            markStarted();
            return quiescingSettled;
          }
          return undefined;
        },
        force() {
          forceCalls.push(phase);
        },
      })),
      { timeoutMs: 5_000 },
    );

    const closing = runEffectExit(coordinator.close({ _tag: 'Interruption', signal: 'SIGTERM' }));
    await started;
    assert.equal(coordinator.force({ _tag: 'SecondSignal', signal: 'SIGTERM' }), true);
    assert.deepEqual(forceCalls, ['quiescing'], 'active escalation runs before force returns');
    assert.equal(
      coordinator.force({ _tag: 'SecondSignal', signal: 'SIGTERM' }),
      false,
      'force is one-way',
    );
    await Bun.sleep(10);
    assert.deepEqual(
      runs,
      ['quiescing'],
      'force cannot advance toward store close before the active owner joins',
    );

    settleQuiescing();
    const exit = await closing;
    assert.ok(Exit.isSuccess(exit));
    assert.equal(exit.value.phases[0]?._tag, 'Forced');
    assert.deepEqual(runs, [...ShutdownPhaseOrder]);
    assert.equal(
      runs.includes('closing-store'),
      true,
      'forced work that actually joined retains the store-safety proof',
    );
    assert.deepEqual(forceCalls, [...ShutdownPhaseOrder]);
    assert.equal(new Set(forceCalls).size, ShutdownPhaseOrder.length);
  });

  test('a second signal synchronously escalates each possible active shutdown phase exactly once', async () => {
    for (const activePhase of ShutdownPhaseOrder) {
      let markStarted: () => void = () => undefined;
      const started = new Promise<void>((resolve) => {
        markStarted = resolve;
      });
      let settleActive: () => void = () => undefined;
      const active = new Promise<void>((resolve) => {
        settleActive = resolve;
      });
      const forceCalls: ShutdownPhase[] = [];
      const coordinator = new LifecycleCoordinator(
        ownerWith((phase) => ({
          run() {
            if (phase !== activePhase) return;
            markStarted();
            return active;
          },
          force() {
            forceCalls.push(phase);
          },
        })),
        { timeoutMs: 1_000 },
      );

      const closing = runEffectExit(coordinator.close({ _tag: 'Interruption', signal: 'SIGTERM' }));
      await started;
      assert.equal(coordinator.state, activePhase);
      assert.equal(coordinator.force({ _tag: 'SecondSignal', signal: 'SIGINT' }), true);
      assert.deepEqual(
        forceCalls,
        [activePhase],
        `${activePhase} force callback must run before force() returns`,
      );
      settleActive();

      const exit = await closing;
      assert.ok(Exit.isSuccess(exit));
      assert.equal(exit.value.forceSignal?.reason._tag, 'SecondSignal');
      const forcedPhases = ShutdownPhaseOrder.slice(ShutdownPhaseOrder.indexOf(activePhase));
      assert.deepEqual(forceCalls, forcedPhases);
      assert.equal(new Set(forceCalls).size, forcedPhases.length);
    }
  });

  test('a forced owner that never settles gates later phases until the absolute deadline', async () => {
    let markStarted: () => void = () => undefined;
    const started = new Promise<void>((resolve) => {
      markStarted = resolve;
    });
    const never = new Promise<void>(() => undefined);
    const runs: ShutdownPhase[] = [];
    const coordinator = new LifecycleCoordinator(
      ownerWith((phase) => ({
        run() {
          runs.push(phase);
          if (phase === 'quiescing') {
            markStarted();
            return never;
          }
          return undefined;
        },
        force() {
          // This deliberately broken owner never publishes its join. The root
          // deadline, not force itself, is what may eventually release the wait.
        },
      })),
      { timeoutMs: 40 },
    );

    const closing = runEffectExit(coordinator.close());
    await started;
    assert.equal(coordinator.force({ _tag: 'External', reason: 'test force' }), true);
    await Bun.sleep(10);
    assert.deepEqual(runs, ['quiescing']);

    const exit = await closing;
    assert.ok(Exit.isSuccess(exit));
    assert.equal(exit.value.phases[0]?._tag, 'TimedOut');
    assert.deepEqual(
      runs,
      ShutdownPhaseOrder.filter((phase) => phase !== 'closing-store'),
    );
    assert.equal(exit.value.phases[6]?._tag, 'Skipped');
    assert.equal(runs.at(-1), 'releasing-process');
    assert.equal(exit.value.deadlineExpired, true);
  });

  test('force callback errors are contained, recorded, and never duplicate cleanup', async () => {
    const forceError = new Error('force callback failed');
    const runCounts = new Map<ShutdownPhase, number>();
    const forceCounts = new Map<ShutdownPhase, number>();
    const latch = new ForceLatch();
    latch.force({ _tag: 'External', reason: 'pre-forced test' });
    const coordinator = new LifecycleCoordinator(
      ownerWith((phase) => ({
        run() {
          runCounts.set(phase, (runCounts.get(phase) ?? 0) + 1);
        },
        force() {
          forceCounts.set(phase, (forceCounts.get(phase) ?? 0) + 1);
          if (phase === 'closing-http') throw forceError;
        },
      })),
      { timeoutMs: 1_000, forceLatch: latch },
    );

    const first = coordinator.close();
    const [one, two] = await Promise.all([runEffectExit(first), runEffectExit(first)]);
    assert.ok(Exit.isSuccess(one));
    assert.ok(Exit.isSuccess(two));
    assert.equal(one.value, two.value);
    assert.deepEqual(
      Object.fromEntries(runCounts),
      Object.fromEntries(ShutdownPhaseOrder.map((phase) => [phase, 1])),
    );
    assert.deepEqual(
      Object.fromEntries(forceCounts),
      Object.fromEntries(ShutdownPhaseOrder.map((phase) => [phase, 1])),
    );
    assert.deepEqual(one.value.forceFailures, [{ phase: 'closing-http', error: forceError }]);
  });

  test('a pre-forced phase that starts then throws still receives force exactly once', async () => {
    const runError = new Error('partially started owner failed');
    const events: string[] = [];
    const latch = new ForceLatch();
    latch.force({ _tag: 'External', reason: 'forced before close' });
    const coordinator = new LifecycleCoordinator(
      ownerWith((phase) => ({
        run() {
          events.push(`run:${phase}`);
          if (phase === 'quiescing') {
            events.push(`throw:${phase}`);
            throw runError;
          }
        },
        force() {
          events.push(`force:${phase}`);
        },
      })),
      { timeoutMs: 1_000, forceLatch: latch },
    );

    const outcome = await runClose(coordinator);
    assert.deepEqual(events.slice(0, 3), ['run:quiescing', 'throw:quiescing', 'force:quiescing']);
    assert.equal(outcome.phases[0]?._tag, 'Failed');
    assert.equal(outcome.failures[0]?.error, runError);
    assert.deepEqual(
      events.filter((event) => event.startsWith('run:')),
      ShutdownPhaseOrder.filter((phase) => phase !== 'closing-store').map(
        (phase) => `run:${phase}`,
      ),
    );
    assert.deepEqual(
      events.filter((event) => event.startsWith('force:')),
      ShutdownPhaseOrder.filter((phase) => phase !== 'closing-store').map(
        (phase) => `force:${phase}`,
      ),
    );
  });
});

describe('ForceLatch', () => {
  test('notifies synchronously once and late observers see the same signal', async () => {
    const latch = new ForceLatch();
    const calls: string[] = [];
    latch.onForce((signal) => {
      calls.push(signal.reason._tag);
    });
    latch.onForce(() => {
      throw new Error('observer failures are callback-safe');
    });

    assert.equal(latch.force({ _tag: 'External', reason: 'unit test' }), true);
    assert.deepEqual(calls, ['External']);
    assert.equal(latch.force({ _tag: 'DeadlineExceeded' }), false);
    latch.onForce((signal) => {
      calls.push(`late:${signal.reason._tag}`);
    });
    assert.deepEqual(calls, ['External', 'late:External']);
    assert.equal((await latch.whenForced()).reason._tag, 'External');
  });
});

describe('ShutdownBudget', () => {
  test('uses one absolute deadline and remaining time never increases', () => {
    const readings = [100, 120, 110, 170, 250];
    let index = 0;
    const budget = ShutdownBudget.start(100, () => readings[index++] ?? 250);

    assert.equal(budget.startedAtMs, 100);
    assert.equal(budget.deadlineMs, 200);
    assert.equal(budget.remainingMs(), 80);
    assert.equal(budget.remainingMs(), 80, 'a backward sample is clamped');
    assert.equal(budget.remainingMs(), 30);
    assert.equal(budget.remainingMs(), 0);
    assert.equal(budget.expired, true);
  });

  test('rejects invalid durations without acquiring a clock', () => {
    let reads = 0;
    assert.throws(
      () =>
        ShutdownBudget.start(-1, () => {
          reads++;
          return 0;
        }),
      /non-negative/,
    );
    assert.equal(reads, 0);
    assert.throws(() => ShutdownBudget.start(Number.POSITIVE_INFINITY), /finite/);
  });
});
