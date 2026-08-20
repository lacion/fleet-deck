import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { describe, test } from 'bun:test';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import {
  coordinatedShutdownExitCode,
  DaemonHostControl,
} from '../../src/daemon/app/host-control.ts';
import {
  LifecycleCoordinator,
  type LifecycleOwner,
  ShutdownPhaseOrder,
} from '../../src/daemon/app/lifecycle-coordinator.ts';
import { runEffectExit } from './helpers.ts';

function owner(run: () => void | Promise<void> = () => undefined): LifecycleOwner {
  return Object.fromEntries(
    ShutdownPhaseOrder.map((phase) => [phase, { run }]),
  ) as unknown as LifecycleOwner;
}

async function closeOutcome(coordinator: LifecycleCoordinator) {
  const exit = await runEffectExit(coordinator.close());
  assert.ok(Exit.isSuccess(exit));
  return exit.value;
}

function teardownCode(
  control: DaemonHostControl,
  exit: Exit.Exit<unknown, unknown>,
): Promise<number> {
  return new Promise((resolve) => {
    control.teardown(exit, resolve);
  });
}

describe('DaemonHostControl', () => {
  test('observes signals without driving cleanup and the second signal forces synchronously', () => {
    const host = new EventEmitter();
    const control = new DaemonHostControl();
    const coordinator = new LifecycleCoordinator(owner(), { timeoutMs: 1_000 });
    control.attachLifecycle(coordinator);
    const remove = control.installSignalObserver(host);
    assert.equal(remove, control.installSignalObserver(host));

    host.emit('SIGTERM');
    assert.equal(control.firstSignal, 'SIGTERM');
    assert.equal(control.signalCount, 1);
    assert.equal(coordinator.closing, false, 'the host callback never starts coordinator close');
    assert.equal(coordinator.forced, false);

    host.emit('SIGINT');
    assert.equal(control.signalCount, 2);
    assert.equal(coordinator.forced, true, 'second-signal escalation is synchronous');

    remove();
    remove();
    assert.equal(host.listenerCount('SIGINT'), 0);
    assert.equal(host.listenerCount('SIGTERM'), 0);
    host.emit('SIGTERM');
    assert.equal(control.signalCount, 2);
  });

  test('queues second-signal force during acquisition and applies it when lifecycle attaches', () => {
    const host = new EventEmitter();
    const control = new DaemonHostControl();
    const remove = control.installSignalObserver(host);
    try {
      host.emit('SIGTERM');
      host.emit('SIGTERM');
      const coordinator = new LifecycleCoordinator(owner(), { timeoutMs: 1_000 });
      assert.equal(coordinator.forced, false);
      control.attachLifecycle(coordinator);
      assert.equal(coordinator.forced, true);
    } finally {
      remove();
    }
  });

  test('a first signal during an existing internal shutdown forces that coordinator', () => {
    const host = new EventEmitter();
    const control = new DaemonHostControl();
    const coordinator = new LifecycleCoordinator(owner(), { timeoutMs: 1_000 });
    coordinator.close({ _tag: 'Requested', reason: 'internal shutdown' });
    control.attachLifecycle(coordinator);
    const remove = control.installSignalObserver(host);
    try {
      host.emit('SIGINT');
      assert.equal(coordinator.forced, true);
    } finally {
      remove();
    }
  });

  test('custom teardown maps a healthy coordinated signal to 0 and preserves unsafe cleanup as 1', async () => {
    const healthy = await closeOutcome(new LifecycleCoordinator(owner(), { timeoutMs: 1_000 }));
    assert.equal(coordinatedShutdownExitCode(healthy, 0), 0);
    assert.equal(coordinatedShutdownExitCode(healthy, 1), 1);

    const failed = await closeOutcome(
      new LifecycleCoordinator(
        owner(() => {
          throw new Error('owner failed');
        }),
        { timeoutMs: 1_000 },
      ),
    );
    assert.equal(coordinatedShutdownExitCode(failed, 0), 1);

    const interrupted = await runEffectExit(Effect.interrupt);
    const control = new DaemonHostControl();
    assert.equal(await teardownCode(control, interrupted), 130);
    control.recordLifecycleOutcome(healthy, 0);
    assert.equal(await teardownCode(control, interrupted), 0);
    control.recordLifecycleOutcome(failed, 0);
    assert.equal(
      await teardownCode(control, interrupted),
      1,
      'exit status only upgrades to failure',
    );
  });

  test('host-exit fallback runs synchronously before the runtime exit callback', async () => {
    const events: string[] = [];
    const control = new DaemonHostControl();
    control.attachProcessExitFallback(() => {
      events.push('release-process');
    });
    control.recordExitCode(0);

    const code = await new Promise<number>((resolve) => {
      control.teardown(Exit.void, (value) => {
        events.push(`exit:${value}`);
        resolve(value);
      });
    });

    assert.equal(code, 0);
    assert.deepEqual(events, ['release-process', 'exit:0']);
  });

  test('a host-exit fallback failure upgrades process status without blocking exit', async () => {
    const control = new DaemonHostControl();
    control.attachProcessExitFallback(() => {
      throw new Error('pid release failed');
    });
    control.recordExitCode(0);

    assert.equal(await teardownCode(control, Exit.void), 1);
  });
});
