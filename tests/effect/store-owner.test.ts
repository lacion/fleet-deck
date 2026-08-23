import assert from 'node:assert/strict';
import { describe, test } from 'bun:test';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Scope from 'effect/Scope';
import { makeStoreOwner, makeUnboundStore } from '../../src/daemon/app/store-owner.ts';
import { type SqliteHandle, openDatabase } from '../../src/daemon/sqlite.ts';

const runTestPromise = Effect.runPromiseWith(Context.empty());

interface FakeHandle {
  readonly handle: SqliteHandle;
  readonly calls: () => string[];
}

/**
 * A recording stand-in for the wrapped bun:sqlite handle. It captures the exact
 * order of the two release-path driver calls (finalizeStatements, then
 * close(<throwOnError>)) so the tests can pin the P8.4 finalize-before-close(true)
 * sequence and the owner's memoization without a real connection.
 */
function makeFakeHandle(): FakeHandle {
  const calls: string[] = [];
  const handle: SqliteHandle = {
    exec: () => {},
    prepare: () => {
      throw new Error('fake handle prepare() is not exercised by these tests');
    },
    finalizeStatements: () => {
      calls.push('finalizeStatements');
    },
    close: (throwOnError?: boolean) => {
      calls.push(`close(${throwOnError})`);
    },
  };
  return { handle, calls: () => calls };
}

describe('makeStoreOwner', () => {
  test('close finalizes the owned statements first, then closes immediately (finalize before close(true))', () => {
    const fake = makeFakeHandle();
    const owner = makeStoreOwner({ name: 'sqlite', handle: fake.handle });

    assert.equal(owner.state(), 'open');
    assert.equal(owner.service.state(), 'open');
    assert.equal(owner.service.handle, fake.handle); // the service publishes the exact handle

    owner.close();

    // The P8.4 ordering: every owned/cached statement is finalized, THEN the
    // connection is closed immediately (close(true) = sqlite3_close), so the
    // real close cannot be deferred behind — or throw on — a live statement.
    assert.deepEqual(fake.calls(), ['finalizeStatements', 'close(true)']);
    assert.equal(owner.state(), 'closed');
    assert.equal(owner.service.state(), 'closed');
  });

  test('the finalize gap: an immediate close(true) throws on a live statement; the owner finalizes first so it completes', () => {
    // Arm 1 — prove the gap exists. A live prepared statement makes an immediate
    // close(true) throw (bun surfaces sqlite3_close's SQLITE_BUSY: "database is
    // locked"): sqlite3_close refuses to close a connection that still owns an
    // unfinalized statement. This is exactly the hazard the owner closes.
    const bare = openDatabase(':memory:');
    bare.exec('CREATE TABLE t(id INTEGER PRIMARY KEY)');
    bare.prepare('SELECT id FROM t WHERE id = ?').get(1); // compile a live sqlite3_stmt
    assert.throws(() => bare.close(true));
    // After finalizing the owned statements the immediate close completes.
    bare.finalizeStatements();
    assert.doesNotThrow(() => bare.close(true));

    // Arm 2 — the owner does finalize-then-close(true) for us, so close() over a
    // handle that still holds a live cached statement completes without throwing
    // and cannot hang the connection open.
    const owned = openDatabase(':memory:');
    owned.exec('CREATE TABLE t(id INTEGER PRIMARY KEY)');
    owned.prepare('SELECT id FROM t WHERE id = ?').get(1); // a live cached statement
    const owner = makeStoreOwner({ name: 'sqlite', handle: owned });

    assert.doesNotThrow(() => owner.close());
    assert.equal(owner.state(), 'closed');
  });

  test('close is memoized — a repeated close, and a fallback after it, run the finalize/close work exactly once', () => {
    const fake = makeFakeHandle();
    const owner = makeStoreOwner({ name: 'sqlite', handle: fake.handle });

    owner.close();
    owner.close(); // memoized: retired === true short-circuits
    owner.shutdownFallback(); // retired === true → completes an already-done close → no-op

    assert.deepEqual(fake.calls(), ['finalizeStatements', 'close(true)']);
    assert.equal(owner.state(), 'closed');
  });

  test('the fallback never initiates a close — a storeSafe=false decline leaves the handle open for the OS', () => {
    // Simulate closeOnce declining the store close (storeSafe=false): the
    // coordinator never calls owner.close(). The root-Scope fallback must NOT
    // override that gate by force-closing a possibly-still-referenced handle —
    // that is the byte-identical "leave SQLite for OS teardown at exit" behavior
    // daemon-resources.ts's closeOnce deliberately chooses.
    const fake = makeFakeHandle();
    const owner = makeStoreOwner({ name: 'sqlite', handle: fake.handle });

    owner.shutdownFallback(); // retired === false → no-op

    assert.deepEqual(fake.calls(), []); // neither finalizeStatements nor close ran
    assert.equal(owner.state(), 'open'); // handle left open for the OS
  });

  test('the root-Scope fallback runs AFTER the coordinator release by finalizer LIFO, and only completes it', async () => {
    const fake = makeFakeHandle();
    const owner = makeStoreOwner({ name: 'sqlite', handle: fake.handle });

    const order: string[] = [];

    // Mirror live-layer.ts on a real Effect Scope: an uninterruptible acquire
    // registers the owner's shutdownFallback as a Scope finalizer, and the
    // acquireRelease release is coordinator work — it records 'coordinator.close'
    // and drives owner.close(). Finalizer LIFO must therefore retire the
    // coordinator first, then run the fallback (which finds the store already
    // retired and no-ops).
    const scopedLifecycle = Effect.acquireRelease(
      Effect.uninterruptible(
        Effect.gen(function* () {
          const scope = yield* Effect.scope;
          yield* Scope.addFinalizer(
            scope,
            Effect.sync(() => {
              order.push('fallback');
              owner.shutdownFallback();
            }),
          );
          return owner;
        }),
      ),
      () =>
        Effect.sync(() => {
          order.push('coordinator.close');
          owner.close();
        }),
      { interruptible: true },
    );

    await runTestPromise(
      Effect.gen(function* () {
        const scope = yield* Scope.make();
        yield* Scope.provide(scope)(scopedLifecycle);
        yield* Scope.close(scope, Exit.void);
      }),
    );

    assert.deepEqual(order, ['coordinator.close', 'fallback']);
    // The coordinator did the finalize-then-close(true) exactly once; the
    // fallback saw retired === true and completed nothing further.
    assert.deepEqual(fake.calls(), ['finalizeStatements', 'close(true)']);
    assert.equal(owner.state(), 'closed');
  });

  test('two root-Scope fallbacks close by LIFO: coordinator.close, then the HttpServer fallback, then the store fallback', async () => {
    const fake = makeFakeHandle();
    const store = makeStoreOwner({ name: 'sqlite', handle: fake.handle });

    const order: string[] = [];

    // Mirror live-layer.ts's TWO root-Scope finalizers, registered during the
    // same uninterruptible acquire and in the SAME source order: the store
    // fallback FIRST (live-layer.ts ~:494) and the HttpServer-style fallback
    // SECOND (~:510). Finalizer LIFO therefore closes them in reverse — the
    // HttpServer fallback before the store fallback — and both run AFTER the
    // acquireRelease release (coordinator.close). That reverse order is the
    // frozen reverse-finalization contract: the listener reads the store, so the
    // dependent (HTTP) fallback retires ahead of its dependency (the store).
    // Swapping the two registrations (here or in live-layer.ts) inverts the tail
    // and fails the assertion below.
    const scopedLifecycle = Effect.acquireRelease(
      Effect.uninterruptible(
        Effect.gen(function* () {
          const scope = yield* Effect.scope;
          // Store finalizer — live-layer.ts ~:494, registered first.
          yield* Scope.addFinalizer(
            scope,
            Effect.sync(() => {
              order.push('store-fallback');
              store.shutdownFallback();
            }),
          );
          // HttpServer-style finalizer — live-layer.ts ~:510, registered second.
          // This owner only holds the store, so the listener fallback is a
          // faithful label stand-in for httpServer.shutdownFallback().
          yield* Scope.addFinalizer(
            scope,
            Effect.sync(() => {
              order.push('http-fallback');
            }),
          );
          return store;
        }),
      ),
      () =>
        Effect.sync(() => {
          order.push('coordinator.close');
          store.close();
        }),
      { interruptible: true },
    );

    await runTestPromise(
      Effect.gen(function* () {
        const scope = yield* Scope.make();
        yield* Scope.provide(scope)(scopedLifecycle);
        yield* Scope.close(scope, Exit.void);
      }),
    );

    assert.deepEqual(order, ['coordinator.close', 'http-fallback', 'store-fallback']);
    // The coordinator did the finalize-then-close(true) exactly once; both
    // fallbacks ran after it and found retired === true, completing nothing more.
    assert.deepEqual(fake.calls(), ['finalizeStatements', 'close(true)']);
    assert.equal(store.state(), 'closed');
  });
});

describe('makeUnboundStore', () => {
  test('is a truthful no-op owner: closed state, no-op close/fallback, and no handle to hand out', () => {
    const owner = makeUnboundStore();

    assert.equal(owner.state(), 'closed');
    assert.equal(owner.service.state(), 'closed');
    assert.doesNotThrow(() => owner.close());
    assert.doesNotThrow(() => owner.shutdownFallback());
    // Reading the handle is a fixture bug, not a supported path — it throws
    // rather than hand out a fake connection.
    assert.throws(() => owner.service.handle, /unbound store owner has no SQLite handle/);
  });
});
