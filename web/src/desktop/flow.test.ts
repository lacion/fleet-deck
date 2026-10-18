import { describe, expect, test } from 'bun:test';
import type { CaptureStatus, DesktopActResult } from '../gen';
import { DeckError } from '../settings/http';
import type { TakeResult } from './capture';
import {
  CaptureFlow,
  type FlowState,
  type KeyStore,
  POLL_MS,
  POLL_WINDOW_MS,
  readPending,
  storageKey,
} from './flow';
import { FakeTime, flush } from './faketime.test';

const published = {
  artifact: { artifact_id: 'A1', station_id: 'st-1', title: 'Desktop capture' },
  version: { version: 1, files: ['capture.png'] },
  created: true,
} as never;

const done = (): TakeResult => ({
  kind: 'done',
  result: {
    address: null,
    info: {},
    capture: { capture_id: 'C1', published },
  } as never as DesktopActResult,
});

class MemoryStore implements KeyStore {
  readonly map = new Map<string, string>();
  getItem = (k: string) => this.map.get(k) ?? null;
  setItem = (k: string, v: string) => {
    this.map.set(k, v);
  };
  removeItem = (k: string) => {
    this.map.delete(k);
  };
}

const throwing: KeyStore = {
  getItem: () => {
    throw new Error('blocked');
  },
  setItem: () => {
    throw new Error('blocked');
  },
  removeItem: () => {
    throw new Error('blocked');
  },
};

type Setup = {
  take?: () => Promise<TakeResult>;
  status?: () => Promise<CaptureStatus>;
  storage?: KeyStore | null;
};

function setup(over: Setup = {}) {
  const time = new FakeTime();
  const counts = { take: 0, status: 0 };
  const ids: string[] = [];
  const storage = over.storage === undefined ? new MemoryStore() : over.storage;
  const flow = new CaptureFlow({
    stationName: 'omarchy box',
    stationId: 'st-1',
    actions: {
      take: async (_s, _m, id) => {
        counts.take += 1;
        ids.push(id);
        return over.take ? over.take() : done();
      },
      status: async () => {
        counts.status += 1;
        return over.status ? over.status() : { state: 'pending', capture_id: 'C1' };
      },
    },
    storage,
    now: time.clock,
    timers: time.timers,
    newId: () => 'C1',
  });
  flow.attach();
  return { flow, time, counts, ids, storage };
}

const kind = (f: CaptureFlow): FlowState['kind'] => f.getSnapshot().kind;

describe('the screenshot flow', () => {
  test('200: done at once, the key is cleared', async () => {
    const { flow, counts, storage } = setup();
    await flow.start('DP-1');
    expect(kind(flow)).toBe('committed');
    expect(counts.take).toBe(1);
    expect(counts.status).toBe(0);
    expect((storage as MemoryStore).map.size).toBe(0);
  });

  test('the ID is stored before the POST', async () => {
    let seen: string | null = null;
    const store = new MemoryStore();
    const { flow } = setup({
      storage: store,
      take: async () => {
        seen = store.getItem(storageKey('st-1'));
        return done();
      },
    });
    await flow.start('DP-1');
    expect(JSON.parse(seen ?? 'null')).toMatchObject({ captureId: 'C1', monitor: 'DP-1' });
  });

  test('202 then committed by polling shows the saved note flag', async () => {
    let calls = 0;
    const { flow, time, counts } = setup({
      take: async () => ({ kind: 'pending', captureId: 'C1' }),
      status: async () => {
        calls += 1;
        return calls < 3
          ? { state: 'pending', capture_id: 'C1' }
          : { state: 'committed', capture_id: 'C1', published };
      },
    });
    await flow.start('DP-1');
    expect(kind(flow)).toBe('waiting');
    await time.advance(POLL_MS * 3);
    expect(flow.getSnapshot()).toMatchObject({ kind: 'committed', after202: true });
    expect(counts.take).toBe(1);
  });

  test('a POST timeout then committed by polling: no second POST', async () => {
    const { flow, time, counts } = setup({
      take: async () => {
        throw new DOMException('aborted', 'AbortError');
      },
      status: async () => ({ state: 'committed', capture_id: 'C1', published }),
    });
    await flow.start('DP-1');
    expect(kind(flow)).toBe('waiting');
    await time.advance(POLL_MS);
    expect(flow.getSnapshot()).toMatchObject({ kind: 'committed', after202: false });
    await time.advance(POLL_MS * 10);
    expect(counts.take).toBe(1);
  });

  test('the deck timeout code only leads to polling', async () => {
    const { flow } = setup({
      take: async () => {
        throw new DeckError(504, 'The station did not answer in time.', 'desktop_timeout', {});
      },
    });
    await flow.start('DP-1');
    expect(kind(flow)).toBe('waiting');
  });

  test('a network error then 60 s of pending ends in unknown with Check result', async () => {
    const { flow, time, counts } = setup({
      take: async () => {
        throw new TypeError('network');
      },
    });
    await flow.start('DP-1');
    await time.advance(POLL_WINDOW_MS - 1);
    expect(kind(flow)).toBe('waiting');
    await time.advance(1);
    expect(kind(flow)).toBe('unknown');
    expect(counts.status).toBe(POLL_WINDOW_MS / POLL_MS);
    expect(counts.take).toBe(1);
    // The polling stopped.
    expect(time.pending).toBe(0);
    await time.advance(POLL_MS * 5);
    expect(counts.status).toBe(POLL_WINDOW_MS / POLL_MS);
  });

  test('Check result is one more lookup; Dismiss clears the key', async () => {
    let answer: CaptureStatus = { state: 'pending', capture_id: 'C1' };
    const { flow, time, counts, storage } = setup({
      take: async () => ({ kind: 'unknown', captureId: 'C1' }),
      status: async () => answer,
    });
    await flow.start('DP-1');
    expect(kind(flow)).toBe('unknown');
    await flow.check();
    expect(counts.status).toBe(1);
    expect(kind(flow)).toBe('unknown');
    answer = { state: 'committed', capture_id: 'C1', published };
    await flow.check();
    expect(kind(flow)).toBe('committed');
    expect(time.pending).toBe(0);

    const second = setup({ take: async () => ({ kind: 'unknown', captureId: 'C1' }) });
    await second.flow.start('DP-1');
    expect((second.storage as MemoryStore).map.size).toBe(1);
    second.flow.dismiss();
    expect(kind(second.flow)).toBe('idle');
    expect((second.storage as MemoryStore).map.size).toBe(0);
    expect((storage as MemoryStore).map.size).toBe(0);
  });

  test('failed shows the deck message; deleted its text; the key is cleared', async () => {
    const failed = setup({
      take: async () => ({ kind: 'pending', captureId: 'C1' }),
      status: async () => ({
        state: 'failed',
        capture_id: 'C1',
        error: 'capture_failed',
        message: 'The screenshot tool failed.',
      }),
    });
    await failed.flow.start('DP-1');
    await failed.time.advance(POLL_MS);
    expect(failed.flow.getSnapshot()).toEqual({
      kind: 'failed',
      text: 'The screenshot tool failed.',
    });
    expect((failed.storage as MemoryStore).map.size).toBe(0);

    const deleted = setup({
      take: async () => ({ kind: 'pending', captureId: 'C1' }),
      status: async () => ({ state: 'deleted', capture_id: 'C1' }),
    });
    await deleted.flow.start('DP-1');
    await deleted.time.advance(POLL_MS);
    expect(kind(deleted.flow)).toBe('deleted');
    expect((deleted.storage as MemoryStore).map.size).toBe(0);

    const body = setup({ take: async () => ({ kind: 'deleted', captureId: 'C1' }) });
    await body.flow.start('DP-1');
    expect(kind(body.flow)).toBe('deleted');
  });

  test('a fixed deck error on the POST fails at once and clears the key', async () => {
    const { flow, storage, time } = setup({
      take: async () => {
        throw new DeckError(409, 'A capture is already running.', 'busy', {});
      },
    });
    await flow.start('DP-1');
    expect(flow.getSnapshot()).toEqual({ kind: 'failed', text: 'A capture is already running.' });
    expect((storage as MemoryStore).map.size).toBe(0);
    expect(time.pending).toBe(0);
  });

  test('a reload with a stored ID resumes polling and does not post', async () => {
    const store = new MemoryStore();
    store.setItem(storageKey('st-1'), JSON.stringify({ captureId: 'C1', monitor: 'DP-1', at: 1 }));
    const { flow, time, counts } = setup({
      storage: store,
      status: async () => ({ state: 'committed', capture_id: 'C1', published }),
    });
    expect(kind(flow)).toBe('waiting');
    await time.advance(POLL_MS);
    expect(kind(flow)).toBe('committed');
    expect(counts.take).toBe(0);
    expect(store.map.size).toBe(0);
  });

  test('storage that throws: the flow works for the page', async () => {
    expect(readPending(throwing, 'st-1')).toBeNull();
    const { flow, time, counts } = setup({
      storage: throwing,
      take: async () => ({ kind: 'pending', captureId: 'C1' }),
      status: async () => ({ state: 'committed', capture_id: 'C1', published }),
    });
    await flow.start('DP-1');
    await time.advance(POLL_MS);
    expect(kind(flow)).toBe('committed');
    expect(counts.take).toBe(1);
    const none = setup({ storage: null });
    await none.flow.start('DP-1');
    expect(kind(none.flow)).toBe('committed');
  });

  test('a capture that is active is not started again; detach stops the polling', async () => {
    const { flow, time, counts } = setup({
      take: async () => ({ kind: 'pending', captureId: 'C1' }),
    });
    await flow.start('DP-1');
    await flow.start('DP-1');
    expect(counts.take).toBe(1);
    flow.detach();
    await time.advance(POLL_MS * 5);
    expect(counts.status).toBe(0);
    flow.attach();
    await time.advance(POLL_MS);
    expect(counts.status).toBe(1);
    await flush();
  });

  test('a corrupt stored value is ignored', () => {
    const store = new MemoryStore();
    store.setItem(storageKey('st-1'), '{"captureId":5}');
    expect(readPending(store, 'st-1')).toBeNull();
    store.setItem(storageKey('st-1'), 'not json');
    expect(readPending(store, 'st-1')).toBeNull();
  });
});
