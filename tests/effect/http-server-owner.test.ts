import assert from 'node:assert/strict';
import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Scope from 'effect/Scope';
import { ApplicationQuiescingError } from '../../src/daemon/app/errors.ts';
import {
  type HttpServerTransport,
  makeHttpServerOwner,
} from '../../src/daemon/app/http-server-owner.ts';
import type { RootIngressSupervisorService } from '../../src/daemon/app/services/ingress-supervisor.ts';
import type { HttpBound } from '../../src/daemon/http.ts';
import { makeIngressSupervisor } from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';

const runTestPromise = Effect.runPromiseWith(Context.empty());

interface RootIngress {
  readonly rootScope: Scope.Closeable;
  readonly ingress: RootIngressSupervisorService;
}

// The owner only ever submits requirement-free Effects (readiness is
// `Effect.void`, the tests submit `Effect.sync`), so an empty root Context is a
// faithful runtime. The cast to the existential `unknown` union mirrors the tag
// boundary `makeIngressSupervisorLayer` crosses in production.
async function makeRootIngress(): Promise<RootIngress> {
  const rootScope = Scope.makeUnsafe('sequential');
  const context = Context.empty() as Context.Context<unknown>;
  const ingress = await runTestPromise(makeIngressSupervisor(context, rootScope));
  return { rootScope, ingress };
}

async function closeRoot(rootScope: Scope.Closeable): Promise<void> {
  await runTestPromise(Scope.close(rootScope, Exit.void));
}

function typedFailure(exit: Exit.Exit<unknown, unknown>): unknown {
  assert.ok(Exit.isFailure(exit));
  return exit.cause.reasons.find(Cause.isFailReason)?.error;
}

interface FakeTransport {
  readonly transport: HttpServerTransport;
  readonly bindCalls: () => number;
  readonly closeRawCalls: () => number;
  readonly closeWorkRuns: () => number;
}

/**
 * A controllable stand-in for the frozen `createHttp` transport slice. `close`
 * memoizes its work exactly like the real `lifecycle.close` (`closePromise`), so
 * repeated invocations count as raw calls but run the retirement once.
 */
function makeFakeTransport(): FakeTransport {
  let quiescing = false;
  let bindCalls = 0;
  let closeRawCalls = 0;
  let closeWorkRuns = 0;
  let closePromise: Promise<void> | null = null;
  const bound: HttpBound = { _tag: 'Bound', hostname: '127.0.0.1', port: 4711 };
  const transport: HttpServerTransport = {
    bind: async () => {
      bindCalls++;
      return bound;
    },
    lifecycle: {
      isQuiescing: () => quiescing,
      close: () => {
        closeRawCalls++;
        if (!closePromise) {
          closeWorkRuns++;
          quiescing = true;
          closePromise = Promise.resolve();
        }
        return closePromise;
      },
    },
  };
  return {
    transport,
    bindCalls: () => bindCalls,
    closeRawCalls: () => closeRawCalls,
    closeWorkRuns: () => closeWorkRuns,
  };
}

describe('makeHttpServerOwner', () => {
  test('acquires listening state and exercises the readiness bridge to settlement', async () => {
    const { rootScope, ingress } = await makeRootIngress();
    const fake = makeFakeTransport();
    const owner = makeHttpServerOwner({ name: 'test-http', ingress, transport: fake.transport });
    try {
      assert.equal(owner.service.state(), 'unbound');
      assert.equal(owner.service.address(), null);
      assert.equal(owner.readyExit(), null);

      const result = await owner.bind(4711, '127.0.0.1');

      assert.equal(result._tag, 'Bound');
      assert.equal(owner.service.state(), 'listening');
      assert.deepEqual(owner.service.address(), { hostname: '127.0.0.1', port: 4711 });

      const readyExit = owner.readyExit();
      assert.ok(readyExit && Exit.isSuccess(readyExit));
      // The bridged readiness Effect ran through IngressSupervisor and settled;
      // nothing lingers in the supervisor registry.
      assert.equal(ingress.activeCount, 0);
    } finally {
      await closeRoot(rootScope);
    }
  });

  test('runRequest bridges request Effects into the root runtime through the supervisor', async () => {
    const { rootScope, ingress } = await makeRootIngress();
    const fake = makeFakeTransport();
    const owner = makeHttpServerOwner({ name: 'test-http', ingress, transport: fake.transport });
    try {
      let ran = 0;
      const exit = await owner.service.runRequest(
        'probe',
        Effect.sync(() => {
          ran++;
          return 'bridged';
        }),
      );

      assert.ok(Exit.isSuccess(exit));
      assert.equal(exit.value, 'bridged');
      assert.equal(ran, 1);
      assert.equal(ingress.activeCount, 0);
    } finally {
      await closeRoot(rootScope);
    }
  });

  test('a repeated bind re-binds the transport but never re-runs readiness', async () => {
    const { rootScope, ingress } = await makeRootIngress();
    const fake = makeFakeTransport();
    const owner = makeHttpServerOwner({ name: 'test-http', ingress, transport: fake.transport });
    try {
      await owner.bind(4711, '127.0.0.1');
      const first = owner.readyExit();
      await owner.bind(4711, '127.0.0.1');
      const second = owner.readyExit();

      assert.ok(first !== null);
      assert.equal(first, second); // same Exit instance — readiness fired once
      assert.equal(fake.bindCalls(), 2); // the transport bind itself is not gated
      assert.equal(owner.service.state(), 'listening');
      assert.equal(ingress.activeCount, 0);
    } finally {
      await closeRoot(rootScope);
    }
  });

  test('runRequest after quiesce is refused with ApplicationQuiescingError, not a new runtime', async () => {
    const { rootScope, ingress } = await makeRootIngress();
    const fake = makeFakeTransport();
    const owner = makeHttpServerOwner({ name: 'test-http', ingress, transport: fake.transport });
    try {
      await owner.bind(4711, '127.0.0.1');
      ingress.quiesce();

      let ran = 0;
      const exit = await owner.service.runRequest(
        'late-request',
        Effect.sync(() => {
          ran++;
          return 'must-not-run';
        }),
      );

      const refusal = typedFailure(exit);
      assert.ok(refusal instanceof ApplicationQuiescingError);
      assert.equal(refusal.operation, 'late-request');
      assert.equal(ran, 0); // refused before the caller Effect evaluated
    } finally {
      await closeRoot(rootScope);
    }
  });

  test('shutdownFallback drives transport close idempotently and reports quiescing', async () => {
    const { rootScope, ingress } = await makeRootIngress();
    const fake = makeFakeTransport();
    const owner = makeHttpServerOwner({ name: 'test-http', ingress, transport: fake.transport });
    try {
      await owner.bind(4711, '127.0.0.1');
      assert.equal(owner.service.state(), 'listening');

      owner.shutdownFallback();
      owner.shutdownFallback();

      assert.ok(fake.closeRawCalls() >= 2); // both fallbacks reached transport.close
      assert.equal(fake.closeWorkRuns(), 1); // memoized closePromise → work runs once
      assert.equal(owner.service.state(), 'quiescing');
    } finally {
      await closeRoot(rootScope);
    }
  });

  test('root Scope closure retires the listener through the registered fallback', async () => {
    const rootScope = Scope.makeUnsafe('sequential');
    const context = Context.empty() as Context.Context<unknown>;
    const ingress = await runTestPromise(makeIngressSupervisor(context, rootScope));
    const fake = makeFakeTransport();
    const owner = makeHttpServerOwner({ name: 'test-http', ingress, transport: fake.transport });

    await owner.bind(4711, '127.0.0.1');
    // Mirror live-layer.ts: register the owner's fallback on the owning Scope.
    await runTestPromise(
      Scope.addFinalizer(
        rootScope,
        Effect.sync(() => owner.shutdownFallback()),
      ),
    );
    assert.equal(fake.closeWorkRuns(), 0);

    await closeRoot(rootScope);

    assert.equal(fake.closeWorkRuns(), 1); // Scope close retired the listener once
    assert.equal(owner.service.state(), 'quiescing');
    assert.notEqual(ingress.state, 'open'); // ingress finalizer on the same Scope also fired
  });

  test('the root-Scope fallback runs AFTER the coordinator release by finalizer LIFO', async () => {
    const { rootScope: ingressScope, ingress } = await makeRootIngress();
    const fake = makeFakeTransport();
    const owner = makeHttpServerOwner({ name: 'test-http', ingress, transport: fake.transport });
    await owner.bind(4711, '127.0.0.1');

    const order: string[] = [];

    // Mirror live-layer.ts on a real Effect Scope: an uninterruptible acquire
    // registers the owner's shutdownFallback as a Scope finalizer, and the
    // acquireRelease release is coordinator work — it records 'coordinator.close'
    // and never touches transport.close. Finalizer LIFO must therefore retire the
    // coordinator first, then the fallback.
    const scopedLifecycle = Effect.acquireRelease(
      Effect.uninterruptible(
        Effect.gen(function* () {
          const scope = yield* Effect.scope;
          yield* Scope.addFinalizer(
            scope,
            Effect.sync(() => {
              order.push('lifecycle.close');
              owner.shutdownFallback();
            }),
          );
          return owner;
        }),
      ),
      () => Effect.sync(() => order.push('coordinator.close')),
      { interruptible: true },
    );

    await runTestPromise(
      Effect.gen(function* () {
        const scope = yield* Scope.make();
        yield* Scope.provide(scope)(scopedLifecycle);
        yield* Scope.close(scope, Exit.void);
      }),
    );

    assert.deepEqual(order, ['coordinator.close', 'lifecycle.close']);
    assert.equal(fake.closeWorkRuns(), 1); // the fallback ran the owner's close work exactly once
    assert.equal(fake.closeRawCalls(), 1); // only the fallback reached transport.close; the release never did
    assert.equal(owner.service.state(), 'quiescing');

    await closeRoot(ingressScope);
  });
});
