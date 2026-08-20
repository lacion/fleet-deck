import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import type * as Fiber from 'effect/Fiber';
import * as Layer from 'effect/Layer';
import * as Scope from 'effect/Scope';
import { ApplicationQuiescingError } from '../../src/daemon/app/errors.ts';
import {
  LifecycleCoordinator,
  type LifecycleOwner,
} from '../../src/daemon/app/lifecycle-coordinator.ts';
import {
  IngressSupervisor,
  type IngressSupervisorService,
} from '../../src/daemon/app/services/ingress-supervisor.ts';
import {
  makeIngressSupervisor,
  makeIngressSupervisorLayer,
} from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';

class Probe extends Context.Service<Probe, { readonly value: string }>()(
  'fleetdeck/test/IngressProbe',
) {}

interface Fixture {
  readonly rootScope: Scope.Closeable;
  readonly supervisor: IngressSupervisorService<Probe>;
}

const runTestPromise = Effect.runPromiseWith(Context.empty());
const DEADLINE_PROCESS_FIXTURE = fileURLToPath(
  new URL('./fixtures/ingress-deadline-process.ts', import.meta.url),
);

async function makeFixture(value = 'captured'): Promise<Fixture> {
  const rootScope = Scope.makeUnsafe('sequential');
  const context = Context.make(Probe, { value });
  const supervisor = await runTestPromise(makeIngressSupervisor(context, rootScope));
  return { rootScope, supervisor };
}

function observe<A, E>(fiber: Fiber.Fiber<A, E>): Promise<Exit.Exit<A, E>> {
  const current = fiber.pollUnsafe();
  if (current) return Promise.resolve(current);
  return new Promise((resolve) => {
    fiber.addObserver(resolve);
  });
}

async function closeRoot(rootScope: Scope.Closeable): Promise<void> {
  await runTestPromise(Scope.close(rootScope, Exit.void));
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 500): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out after ${timeoutMs}ms`)),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function typedFailure(exit: Exit.Exit<unknown, unknown>): unknown {
  assert.ok(Exit.isFailure(exit));
  return exit.cause.reasons.find(Cause.isFailReason)?.error;
}

describe('IngressSupervisor', () => {
  test('runs all three RC.110 callback forms against one captured Context', async () => {
    const { rootScope, supervisor } = await makeFixture('root-context');
    try {
      const readProbe = Effect.map(Probe, (probe) => probe.value);
      assert.equal(await supervisor.runPromise('promise probe', readProbe), 'root-context');

      const callbackExit = await new Promise<Exit.Exit<string, ApplicationQuiescingError>>(
        (resolve) => {
          supervisor.runCallback('callback probe', readProbe, { onExit: resolve });
        },
      );
      assert.ok(Exit.isSuccess(callbackExit));
      assert.equal(callbackExit.value, 'root-context');

      const forkExit = await observe(supervisor.runFork('fork probe', readProbe));
      assert.ok(Exit.isSuccess(forkExit));
      assert.equal(forkExit.value, 'root-context');
      assert.equal(supervisor.activeCount, 0);
    } finally {
      await closeRoot(rootScope);
    }
  });

  test('pre-aborted submissions never evaluate the caller Effect', async () => {
    const { rootScope, supervisor } = await makeFixture();
    const controller = new AbortController();
    controller.abort();
    let evaluations = 0;
    const work = Effect.sync(() => {
      evaluations++;
      return 'must-not-run';
    });

    try {
      await assert.rejects(
        supervisor.runPromise('pre-aborted promise', work, { signal: controller.signal }),
      );

      const callbackExit = await new Promise<Exit.Exit<string, ApplicationQuiescingError>>(
        (resolve) => {
          supervisor.runCallback('pre-aborted callback', work, {
            signal: controller.signal,
            onExit: resolve,
          });
        },
      );
      assert.ok(Exit.isFailure(callbackExit));
      assert.equal(Cause.hasInterruptsOnly(callbackExit.cause), true);

      const forkExit = await observe(
        supervisor.runFork('pre-aborted fork', work, { signal: controller.signal }),
      );
      assert.ok(Exit.isFailure(forkExit));
      assert.equal(Cause.hasInterruptsOnly(forkExit.cause), true);
      assert.equal(evaluations, 0);
      assert.equal(supervisor.activeCount, 0);
    } finally {
      await closeRoot(rootScope);
    }
  });

  test('quiesce closes admission synchronously and shares join/close identities', async () => {
    const { rootScope, supervisor } = await makeFixture();
    let evaluations = 0;
    const work = Effect.sync(() => {
      evaluations++;
      return 'must-not-run';
    });

    try {
      supervisor.quiesce();
      assert.equal(supervisor.state, 'quiescing');

      const promiseError = await supervisor.runPromise('refused promise', work).then<null, unknown>(
        () => null,
        (error: unknown) => error,
      );
      assert.ok(promiseError instanceof ApplicationQuiescingError);
      assert.equal(promiseError.operation, 'refused promise');

      const callbackExit = await new Promise<Exit.Exit<string, ApplicationQuiescingError>>(
        (resolve) => {
          supervisor.runCallback('refused callback', work, { onExit: resolve });
        },
      );
      const callbackError = typedFailure(callbackExit);
      assert.ok(callbackError instanceof ApplicationQuiescingError);
      assert.equal(callbackError.operation, 'refused callback');

      const forkExit = await observe(supervisor.runFork('refused fork', work));
      const forkError = typedFailure(forkExit);
      assert.ok(forkError instanceof ApplicationQuiescingError);
      assert.equal(forkError.operation, 'refused fork');
      assert.equal(evaluations, 0);

      const firstJoin = supervisor.join();
      assert.equal(firstJoin, supervisor.join());
      await firstJoin;

      const firstClose = supervisor.close();
      assert.equal(firstClose, supervisor.close());
      await firstClose;
      assert.equal(supervisor.state, 'closed');
    } finally {
      await closeRoot(rootScope);
    }
  });

  test('join drains admitted work naturally without interrupting it', async () => {
    const { rootScope, supervisor } = await makeFixture();
    let release: (value: string) => void = () => undefined;
    const gate = new Promise<string>((resolve) => {
      release = resolve;
    });
    let starts = 0;
    const work = supervisor.runPromise(
      'graceful request',
      Effect.promise(() => {
        starts++;
        return gate;
      }),
    );

    try {
      assert.equal(starts, 1);
      assert.equal(supervisor.activeCount, 1);
      const joined = supervisor.join();
      assert.equal(supervisor.state, 'quiescing');
      assert.equal(joined, supervisor.join());

      let joinSettled = false;
      void joined.then(() => {
        joinSettled = true;
      });
      await Bun.sleep(0);
      assert.equal(joinSettled, false);

      await assert.rejects(
        supervisor.runPromise('late request', Effect.succeed('no')),
        ApplicationQuiescingError,
      );
      release('done');
      assert.equal(await work, 'done');
      await joined;
      assert.equal(joinSettled, true);
      assert.equal(supervisor.activeCount, 0);
    } finally {
      await closeRoot(rootScope);
    }
  });

  test('close interrupts and joins callback and fork fibers', async () => {
    const { rootScope, supervisor } = await makeFixture();
    let callbackFinalized = 0;
    let forkFinalized = 0;
    const callbackExit = new Promise<Exit.Exit<void, ApplicationQuiescingError>>((resolve) => {
      supervisor.runCallback(
        'callback request',
        Effect.never.pipe(
          Effect.ensuring(
            Effect.sync(() => {
              callbackFinalized++;
            }),
          ),
        ),
        { onExit: resolve },
      );
    });
    const fork = supervisor.runFork(
      'fork request',
      Effect.never.pipe(
        Effect.ensuring(
          Effect.sync(() => {
            forkFinalized++;
          }),
        ),
      ),
    );

    try {
      assert.equal(supervisor.activeCount, 2);
      const closing = supervisor.close();
      const [callbackResult, forkResult] = await Promise.all([callbackExit, observe(fork)]);
      await closing;

      assert.ok(Exit.isFailure(callbackResult));
      assert.equal(Cause.hasInterruptsOnly(callbackResult.cause), true);
      assert.ok(Exit.isFailure(forkResult));
      assert.equal(Cause.hasInterruptsOnly(forkResult.cause), true);
      assert.equal(callbackFinalized, 1);
      assert.equal(forkFinalized, 1);
      assert.equal(supervisor.activeCount, 0);
      assert.equal(supervisor.state, 'closed');
    } finally {
      await closeRoot(rootScope);
    }
  });

  test('re-entrant close during runner startup interrupts and joins the late-published fiber', async () => {
    const { rootScope, supervisor } = await makeFixture();
    let releaseCleanup: () => void = () => undefined;
    const cleanupGate = new Promise<void>((resolve) => {
      releaseCleanup = resolve;
    });
    let markCleanupStarted: () => void = () => undefined;
    const cleanupStarted = new Promise<void>((resolve) => {
      markCleanupStarted = resolve;
    });
    const reentrant = { close: null as Promise<void> | null };

    const work = supervisor.runPromise(
      're-entrant close',
      Effect.flatMap(
        Effect.sync(() => {
          reentrant.close = supervisor.close();
        }),
        () =>
          Effect.callback<string>(() =>
            Effect.promise(async () => {
              markCleanupStarted();
              await cleanupGate;
            }),
          ),
      ),
    );
    const workSettled = work.then(
      () => false,
      () => true,
    );

    try {
      await cleanupStarted;
      const closing = reentrant.close;
      assert.ok(closing);
      assert.equal(supervisor.state, 'quiescing');
      assert.equal(supervisor.activeCount, 1);
      assert.equal(closing, supervisor.close());

      let closeSettled = false;
      void closing.then(() => {
        closeSettled = true;
      });
      await Bun.sleep(0);
      assert.equal(closeSettled, false, 'close must join asynchronous interruption cleanup');

      releaseCleanup();
      await closing;
      assert.equal(await workSettled, true);
      assert.equal(supervisor.activeCount, 0);
      assert.equal(supervisor.state, 'closed');
    } finally {
      releaseCleanup();
      await closeRoot(rootScope);
    }
  });

  test('root Scope fallback interrupts but does not install a second unbounded join', async () => {
    const { rootScope, supervisor } = await makeFixture();
    let releaseCleanup: () => void = () => undefined;
    const cleanupGate = new Promise<void>((resolve) => {
      releaseCleanup = resolve;
    });
    let markCleanupStarted: () => void = () => undefined;
    const cleanupStarted = new Promise<void>((resolve) => {
      markCleanupStarted = resolve;
    });
    const work = supervisor.runPromise(
      'root-owned request',
      Effect.callback<void>(() =>
        Effect.promise(async () => {
          markCleanupStarted();
          await cleanupGate;
        }),
      ),
    );
    const workSettled = work.then(
      () => false,
      () => true,
    );

    assert.equal(supervisor.activeCount, 1);
    supervisor.interrupt();
    await cleanupStarted;
    assert.equal(supervisor.state, 'quiescing');
    assert.equal(supervisor.activeCount, 1);

    await within(closeRoot(rootScope), 'root Scope fallback behind stuck ingress cleanup');
    assert.equal(
      supervisor.activeCount,
      1,
      'the explicit registry retains ownership after bounded root fallback',
    );

    const closing = supervisor.close();
    assert.equal(closing, supervisor.close());
    let closeSettled = false;
    void closing.then(() => {
      closeSettled = true;
    });
    await Bun.sleep(0);
    assert.equal(closeSettled, false);

    releaseCleanup();
    await closing;
    assert.equal(await workSettled, true);
    assert.equal(supervisor.activeCount, 0);
    assert.equal(supervisor.state, 'closed');
  });

  test('an ingress deadline loss skips store close and root Scope never re-awaits stuck cleanup', async () => {
    const { rootScope, supervisor } = await makeFixture();
    let releaseCleanup: () => void = () => undefined;
    const cleanupGate = new Promise<void>((resolve) => {
      releaseCleanup = resolve;
    });
    let markCleanupStarted: () => void = () => undefined;
    const cleanupStarted = new Promise<void>((resolve) => {
      markCleanupStarted = resolve;
    });
    let storeCloses = 0;
    const work = supervisor.runPromise(
      'deadline-stuck request',
      Effect.callback<void>(() =>
        Effect.promise(async () => {
          markCleanupStarted();
          await cleanupGate;
        }),
      ),
    );
    const workSettled = work.then(
      () => false,
      () => true,
    );

    const noOp = { run: () => undefined } as const;
    const owner: LifecycleOwner = {
      quiescing: noOp,
      'stopping-producers': noOp,
      withdrawing: noOp,
      'releasing-holds': noOp,
      'closing-clients': { run: () => supervisor.close() },
      'closing-http': noOp,
      'closing-store': {
        run: () => {
          storeCloses++;
        },
      },
      'releasing-process': noOp,
    };
    const coordinator = new LifecycleCoordinator(owner, { timeoutMs: 30 });

    try {
      const outcome = await within(
        runTestPromise(coordinator.close({ _tag: 'Requested', reason: 'deadline fixture' })),
        'absolute ingress deadline',
      );
      await cleanupStarted;

      assert.equal(outcome.deadlineExpired, true);
      assert.equal(
        outcome.phases.find((phase) => phase.phase === 'closing-clients')?._tag,
        'TimedOut',
      );
      const storePhase = outcome.phases.find((phase) => phase.phase === 'closing-store');
      assert.ok(storePhase);
      assert.equal(storePhase._tag, 'Skipped');
      assert.equal(storePhase._tag === 'Skipped' ? storePhase.reason : null, 'store-unsafe');
      assert.equal(storePhase.remainingMs, 0);
      assert.equal(storeCloses, 0, 'a live ingress fiber invalidates normal store close');
      assert.equal(supervisor.activeCount, 1);

      await within(closeRoot(rootScope), 'root Scope close after ingress deadline');
      assert.equal(
        supervisor.activeCount,
        1,
        'Scope closure does not conceal or re-await the stranded tracked fiber',
      );
    } finally {
      releaseCleanup();
    }

    await within(supervisor.close(), 'late ingress cleanup settlement');
    assert.equal(await workSettled, true);
    assert.equal(supervisor.activeCount, 0);
    assert.equal(supervisor.state, 'closed');
  });

  test('runPromiseExit preserves typed failure, defect, interruption, and refusal', async () => {
    const { rootScope, supervisor } = await makeFixture();
    const typed = new Error('typed failure');
    const defect = new Error('defect');
    try {
      const typedExit = await supervisor.runPromiseExit('typed', Effect.fail(typed));
      assert.equal(typedFailure(typedExit), typed);

      const defectExit = await supervisor.runPromiseExit('defect', Effect.die(defect));
      assert.ok(Exit.isFailure(defectExit));
      assert.equal(defectExit.cause.reasons.find(Cause.isDieReason)?.defect, defect);

      const interruptedExit = await supervisor.runPromiseExit('interrupt', Effect.interrupt);
      assert.ok(Exit.isFailure(interruptedExit));
      assert.equal(Cause.hasInterruptsOnly(interruptedExit.cause), true);

      supervisor.quiesce();
      const refusedExit = await supervisor.runPromiseExit('refused exit', Effect.succeed('no'));
      const refusal = typedFailure(refusedExit);
      assert.ok(refusal instanceof ApplicationQuiescingError);
      assert.equal(refusal.operation, 'refused exit');
    } finally {
      await closeRoot(rootScope);
    }
  });

  test('Context.Service tag and scoped Layer capture and finalize the provided root', async () => {
    const captured = { service: null as IngressSupervisorService<unknown> | null };
    const probeLayer = Layer.succeed(Probe, { value: 'layer-context' });
    const ingressLayer = makeIngressSupervisorLayer<Probe>().pipe(Layer.provide(probeLayer));
    const program = Effect.gen(function* () {
      const supervisor = yield* IngressSupervisor;
      captured.service = supervisor;
      return yield* Effect.promise(() =>
        supervisor.runPromise(
          'layer probe',
          Effect.map(Probe, (probe) => probe.value),
        ),
      );
    });

    assert.equal(await runTestPromise(Effect.provide(program, ingressLayer)), 'layer-context');
    assert.ok(captured.service);
    assert.equal(captured.service.state, 'quiescing');
    await captured.service.close();
    assert.equal(captured.service.state, 'closed');
  });

  test('platform implementation contains the only runners and no nested runtime/Layer build', () => {
    const source = readFileSync(
      path.resolve(import.meta.dir, '../../src/daemon/platform/bun/ingress-supervisor-live.ts'),
      'utf8',
    );

    assert.equal(source.match(/Effect\.runForkWith\(/g)?.length, 1);
    assert.equal(source.match(/Effect\.runCallbackWith\(/g)?.length, 1);
    assert.equal(source.match(/Effect\.runPromiseWith\(/g)?.length, 1);
    assert.doesNotMatch(source, /ManagedRuntime(?:\.make)?/);
    assert.doesNotMatch(source, /Layer\.(?:build|provide)/);
    assert.doesNotMatch(source, /^\s*Fiber\.runIn\(/m);
    assert.equal(source.match(/Layer\.effect\(/g)?.length, 1);
  });

  test('BunRuntime exits nonzero after the bounded root closes despite a stuck ingress fiber', async () => {
    const child = Bun.spawn([process.execPath, '--no-env-file', DEADLINE_PROCESS_FIXTURE], {
      stdin: 'ignore',
      stdout: 'pipe',
      stderr: 'pipe',
    });

    try {
      const [exitCode, stdout, stderr] = await within(
        Promise.all([
          child.exited,
          new Response(child.stdout).text(),
          new Response(child.stderr).text(),
        ]),
        'ingress deadline subprocess exit',
        2_000,
      );
      const observations = stdout
        .split('\n')
        .filter(Boolean)
        .map((line): Record<string, unknown> => JSON.parse(line) as Record<string, unknown>);

      assert.equal(exitCode, 1);
      assert.equal(stderr, '');
      assert.deepEqual(observations, [
        { event: 'ingress-started' },
        {
          event: 'deadline-outcome',
          clients: 'TimedOut',
          store: 'Skipped',
          storeCloses: 0,
          active: 1,
        },
        { event: 'root-scope-closed' },
        { event: 'teardown', code: 1 },
      ]);
    } finally {
      if (child.exitCode === null) child.kill('SIGKILL');
      await child.exited;
    }
  });
});
