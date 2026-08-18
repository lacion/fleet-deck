import assert from 'node:assert/strict';
import test from './helpers/harness-test.ts';
import {
  startFleetConnection,
  type FleetConnectionCallbacks,
  type FleetConnectionEnvironment,
  type FleetSocket,
} from '../board/src/fleetConnection.ts';

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

class FakeSocket implements FleetSocket {
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  closes = 0;

  close(): void {
    this.closes += 1;
  }
}

test('retired poll and socket callbacks cannot cross a token/effect generation', async () => {
  const polls: Array<{
    signal: AbortSignal;
    result: ReturnType<typeof deferred<unknown>>;
  }> = [];
  const sockets: FakeSocket[] = [];
  const timers = new Map<number, () => void>();
  let nextTimer = 1;
  const env: FleetConnectionEnvironment = {
    fetchState: (_timeoutMs, signal) => {
      const result = deferred<unknown>();
      polls.push({ signal, result });
      return result.promise;
    },
    openSocket: () => {
      const socket = new FakeSocket();
      sockets.push(socket);
      return socket;
    },
    socketUrl: () => 'ws://fleet.test/ws',
    schedule: (callback) => {
      const id = nextTimer++;
      timers.set(id, callback);
      return id;
    },
    cancel: (handle) => {
      timers.delete(handle as number);
    },
    random: () => 0.5,
  };
  const snapshots: unknown[] = [];
  const lans: unknown[] = [];
  const statuses: string[] = [];
  const callbacks: FleetConnectionCallbacks = {
    onSnapshot: (snapshot) => snapshots.push(snapshot),
    onLan: (lan) => lans.push(lan),
    onStatus: (status) => statuses.push(status),
  };
  const owner = { current: null as object | null };

  const stopOld = startFleetConnection(owner, callbacks, env);
  const oldPoll = polls[0];
  const oldSocket = sockets[0];
  assert.ok(oldPoll);
  assert.ok(oldSocket);
  const oldOpen = oldSocket.onopen;
  const oldMessage = oldSocket.onmessage;
  const oldClose = oldSocket.onclose;
  const oldError = oldSocket.onerror;

  // React runs exactly this cleanup/setup sequence for a token dependency
  // change and for the StrictMode effect replay in development.
  stopOld();
  assert.equal(oldPoll.signal.aborted, true, 'cleanup aborts the retired /state request');
  assert.equal(oldSocket.closes, 1, 'cleanup closes the retired socket once');
  const stopCurrent = startFleetConnection(owner, callbacks, env);
  assert.equal(polls.length, 2);
  assert.equal(sockets.length, 2);

  // Even a fetcher that resolves after ignoring AbortSignal cannot repaint the
  // new generation or queue a timer from its stale finally continuation.
  oldPoll.result.resolve({ marker: 'old-poll', lan: { urls: ['old'] } });
  await Promise.resolve();
  await Promise.resolve();
  oldOpen?.();
  oldMessage?.({ data: JSON.stringify({ type: 'snapshot', marker: 'old-ws' }) });
  oldClose?.();
  oldError?.();
  assert.deepEqual(snapshots, []);
  assert.deepEqual(lans, []);
  assert.equal(oldSocket.closes, 1, 'a stale onerror cannot act on its retired socket');
  assert.equal(timers.size, 0, 'stale close/finally callbacks cannot schedule retries or polls');

  const currentSocket = sockets[1];
  assert.ok(currentSocket);
  currentSocket.onopen?.();
  currentSocket.onmessage?.({
    data: JSON.stringify({ type: 'snapshot', marker: 'current-ws' }),
  });
  assert.deepEqual(snapshots, [{ type: 'snapshot', marker: 'current-ws' }]);
  assert.equal(statuses.at(-1), 'live');

  stopCurrent();
  polls[1]?.result.resolve(null);
});

test('a retired state request cannot latch its stale-token 401', async () => {
  // api.ts computes its browser base at module evaluation. Supply only the
  // location shape that pure transport test needs, then restore the host.
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { location: { origin: 'http://fleet.test' } },
  });
  try {
    const { fetchStateWithTransport } = await import('../board/src/api.ts');
    const response = deferred<Response>();
    const oldOwner = new AbortController();
    let unauthorized = 0;
    const oldRequest = fetchStateWithTransport(15_000, oldOwner.signal, {
      fetcher: () => response.promise,
      unauthorized: () => {
        unauthorized += 1;
      },
    });

    oldOwner.abort();
    response.resolve({ status: 401, ok: false } as Response);
    assert.equal(await oldRequest, null);
    assert.equal(unauthorized, 0, 'a 401 belonging to the old token generation is ignored');

    const currentOwner = new AbortController();
    assert.equal(
      await fetchStateWithTransport(15_000, currentOwner.signal, {
        fetcher: async () => ({ status: 401, ok: false }) as Response,
        unauthorized: () => {
          unauthorized += 1;
        },
      }),
      null,
    );
    assert.equal(unauthorized, 1, 'the current generation still gates a genuinely bad token');
  } finally {
    if (previousWindow) Object.defineProperty(globalThis, 'window', previousWindow);
    else Reflect.deleteProperty(globalThis, 'window');
  }
});
