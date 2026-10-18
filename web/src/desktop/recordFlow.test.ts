import { describe, expect, test } from 'bun:test';
import type { ActiveRecording, DesktopActResult, RecordingCaptureStatus } from '../gen';
import { DeckError } from '../settings/http';
import type { TakeResult } from './capture';
import { FakeTime, flush } from './faketime.test';
import type { KeyStore } from './flow';
import {
  isActive,
  POLL_MS,
  RecordFlow,
  type RecState,
  readStored,
  recordingId,
} from './recordFlow';
import { recordingKey, windowMs } from './recording';

const published = {
  artifact: { artifact_id: 'A1', station_id: 'st-1', title: 'Desktop recording' },
  version: { version: 1, files: ['recording.mp4'] },
  created: true,
} as never;

const done = (): TakeResult => ({
  kind: 'done',
  result: {
    address: null,
    info: {},
    capture: { capture_id: 'R1', published },
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

const pending: RecordingCaptureStatus = { state: 'pending', capture_id: 'R1' };
const committed: RecordingCaptureStatus = { state: 'committed', capture_id: 'R1', published };
const progress = (phase: 'preparing' | 'recording' | 'stopping' | 'finalizing') =>
  ({
    state: 'pending',
    capture_id: 'R1',
    recording: { phase, requested_seconds: 30, started_at: 1, elapsed_ms: 2 },
  }) as RecordingCaptureStatus;

const idle: ActiveRecording = { state: 'idle' };
const station409 = () =>
  new DeckError(409, 'The station is not connected.', 'station_unreachable', {});

type Setup = {
  record?: () => Promise<TakeResult>;
  status?: () => Promise<RecordingCaptureStatus>;
  active?: () => Promise<ActiveRecording>;
  stop?: () => Promise<RecordingCaptureStatus>;
  storage?: KeyStore | null;
  attach?: boolean;
};

function setup(over: Setup = {}) {
  const time = new FakeTime();
  const counts = { record: 0, status: 0, active: 0, stop: 0 };
  const posted: { monitor: string; seconds: number; id: string }[] = [];
  const storage = over.storage === undefined ? new MemoryStore() : over.storage;
  const flow = new RecordFlow({
    stationName: 'omarchy box',
    stationId: 'st-1',
    actions: {
      record: async (_s, monitor, seconds, id) => {
        counts.record += 1;
        posted.push({ monitor, seconds, id });
        return over.record ? over.record() : done();
      },
      status: async () => {
        counts.status += 1;
        return over.status ? over.status() : pending;
      },
      active: async () => {
        counts.active += 1;
        return over.active ? over.active() : idle;
      },
      stop: async () => {
        counts.stop += 1;
        return over.stop ? over.stop() : committed;
      },
    },
    storage,
    now: time.clock,
    timers: time.timers,
    newId: () => 'R1',
  });
  return { flow, time, counts, posted, storage };
}

/** A flow after `attach` and the first discovery. */
async function attached(over: Setup = {}) {
  const s = setup(over);
  s.flow.attach();
  await flush();
  return s;
}

const kind = (f: RecordFlow): RecState['kind'] => f.getSnapshot().kind;

describe('the recording flow: start', () => {
  test('200: committed at once, the ID is stored before the POST and cleared after', async () => {
    const store = new MemoryStore();
    let seen: string | null = null;
    const { flow, counts, posted } = await attached({
      storage: store,
      record: async () => {
        seen = store.getItem(recordingKey('st-1'));
        return done();
      },
    });
    await flow.start('DP-1', 30);
    expect(JSON.parse(seen ?? 'null')).toMatchObject({
      captureId: 'R1',
      monitor: 'DP-1',
      seconds: 30,
    });
    expect(kind(flow)).toBe('committed');
    expect(counts.record).toBe(1);
    expect(posted[0]).toEqual({ monitor: 'DP-1', seconds: 30, id: 'R1' });
    expect(store.map.size).toBe(0);
  });

  test('start, poll the live phase while the POST runs, then committed', async () => {
    let release: (t: TakeResult) => void = () => {};
    let n = 0;
    const phases = ['preparing', 'recording', 'stopping', 'finalizing'] as const;
    const { flow, time, counts } = await attached({
      record: () => new Promise<TakeResult>((resolve) => (release = resolve)),
      status: async () => progress(phases[Math.min(n++, phases.length - 1)] ?? 'recording'),
    });
    const started = flow.start('DP-1', 30);
    expect(flow.getSnapshot()).toMatchObject({ kind: 'running', phase: null, posting: true });
    await time.advance(POLL_MS);
    expect(flow.getSnapshot()).toMatchObject({ phase: 'preparing' });
    await time.advance(POLL_MS);
    expect(flow.getSnapshot()).toMatchObject({ phase: 'recording' });
    await time.advance(POLL_MS * 2);
    expect(flow.getSnapshot()).toMatchObject({ phase: 'finalizing' });
    release(done());
    await started;
    expect(kind(flow)).toBe('committed');
    expect(counts.record).toBe(1);
    expect(time.pending).toBe(0);
  });

  test('start, poll, committed by the lookup', async () => {
    let calls = 0;
    const { flow, time, counts } = await attached({
      record: async () => ({ kind: 'pending', captureId: 'R1' }),
      status: async () => {
        calls += 1;
        return calls < 3 ? pending : committed;
      },
    });
    await flow.start('DP-1', 10);
    expect(flow.getSnapshot()).toMatchObject({ kind: 'running', after202: true, posting: false });
    await time.advance(POLL_MS * 3);
    expect(flow.getSnapshot()).toMatchObject({ kind: 'committed', after202: true });
    expect(counts.record).toBe(1);
  });

  test('the POST is sent once, also after a timeout or a network error', async () => {
    for (const error of [
      new DOMException('aborted', 'AbortError'),
      new TypeError('network'),
      new DeckError(504, 'The station did not answer in time.', 'desktop_timeout', {}),
    ]) {
      const { flow, time, counts } = await attached({
        record: async () => {
          throw error;
        },
        status: async () => committed,
      });
      await flow.start('DP-1', 30);
      expect(kind(flow)).toBe('running');
      await time.advance(POLL_MS * 20);
      expect(kind(flow)).toBe('committed');
      expect(counts.record).toBe(1);
    }
  });

  test('a definite deck error fails with its message and clears the ID', async () => {
    const { flow, storage, time } = await attached({
      record: async () => {
        throw new DeckError(409, 'A capture is already running.', 'busy', {});
      },
    });
    await flow.start('DP-1', 30);
    expect(flow.getSnapshot()).toEqual({ kind: 'failed', text: 'A capture is already running.' });
    expect((storage as MemoryStore).map.size).toBe(0);
    expect(time.pending).toBe(0);
  });

  test('the window ends at seconds x 1000 + 45000 ms with Check result; no more polling', async () => {
    const { flow, time, counts } = await attached({
      record: async () => {
        throw new TypeError('network');
      },
    });
    await flow.start('DP-1', 30);
    const window = windowMs(30);
    expect(window).toBe(75_000);
    await time.advance(window - 1);
    expect(kind(flow)).toBe('running');
    await time.advance(1);
    expect(kind(flow)).toBe('expired');
    expect(counts.status).toBe(Math.ceil(window / POLL_MS));
    expect(counts.record).toBe(1);
    expect(time.pending).toBe(0);
    await time.advance(POLL_MS * 5);
    expect(counts.status).toBe(Math.ceil(window / POLL_MS));
    expect(isActive(flow.getSnapshot())).toBe(false);
    expect(recordingId(flow.getSnapshot())).toBe('R1');
  });

  test('Check result is one more lookup; the POST is never repeated', async () => {
    let answer: RecordingCaptureStatus = pending;
    const { flow, time, counts, storage } = await attached({
      record: async () => {
        throw new TypeError('network');
      },
      status: async () => answer,
    });
    await flow.start('DP-1', 1);
    await time.advance(windowMs(1));
    expect(kind(flow)).toBe('expired');
    const before = counts.status;
    await flow.check();
    expect(counts.status).toBe(before + 1);
    expect(kind(flow)).toBe('expired');
    answer = committed;
    await flow.check();
    expect(counts.status).toBe(before + 2);
    expect(kind(flow)).toBe('committed');
    expect(counts.record).toBe(1);
    expect((storage as MemoryStore).map.size).toBe(0);
  });

  test('failed and deleted lookups end the wait with the message', async () => {
    const failed = await attached({
      record: async () => {
        throw new TypeError('network');
      },
      status: async () => ({
        state: 'failed',
        capture_id: 'R1',
        error: 'recording_interrupted',
        message: 'The recording was interrupted before it was saved.',
      }),
    });
    await failed.flow.start('DP-1', 30);
    await failed.time.advance(POLL_MS);
    expect(failed.flow.getSnapshot()).toEqual({
      kind: 'failed',
      text: 'The recording was interrupted before it was saved.',
    });
    const deleted = await attached({
      record: async () => {
        throw new TypeError('network');
      },
      status: async () => ({ state: 'deleted', capture_id: 'R1' }),
    });
    await deleted.flow.start('DP-1', 30);
    await deleted.time.advance(POLL_MS);
    expect(kind(deleted.flow)).toBe('deleted');
  });

  test('a recording that runs is not started again; a bad duration sends nothing', async () => {
    const { flow, counts } = await attached({
      record: () => new Promise<TakeResult>(() => {}),
    });
    void flow.start('DP-1', 30);
    await flow.start('DP-1', 30);
    expect(counts.record).toBe(1);
    const other = await attached();
    await other.flow.start('DP-1', 0);
    await other.flow.start('DP-1', 121);
    await other.flow.start('DP-1', 1.5);
    expect(other.counts.record).toBe(0);
  });

  test('the status "unknown" during the POST does not end the wait; after it, it does', async () => {
    let release: (t: TakeResult) => void = () => {};
    const { flow, time } = await attached({
      record: () => new Promise<TakeResult>((resolve) => (release = resolve)),
      status: async () => ({ state: 'unknown', capture_id: 'R1' }),
    });
    const started = flow.start('DP-1', 30);
    await time.advance(POLL_MS * 2);
    expect(kind(flow)).toBe('running');
    release({ kind: 'pending', captureId: 'R1' });
    await started;
    await time.advance(POLL_MS);
    expect(kind(flow)).toBe('expired');
  });
});

describe('the recording flow: open and reload', () => {
  test('an idle station with nothing stored stays idle and shows nothing', async () => {
    const { flow, counts } = await attached();
    expect(kind(flow)).toBe('idle');
    expect(counts.active).toBe(1);
    expect(counts.status).toBe(0);
  });

  test('an active recording shows its phase and resumes polling for that ID', async () => {
    let calls = 0;
    const { flow, time, counts } = await attached({
      active: async () => ({ state: 'active', capture_id: 'R9', status: progress('recording') }),
      status: async () => (++calls < 3 ? progress('recording') : progress('finalizing')),
    });
    expect(flow.getSnapshot()).toMatchObject({
      kind: 'running',
      captureId: 'R9',
      phase: 'recording',
      posting: false,
    });
    expect(isActive(flow.getSnapshot())).toBe(true);
    await time.advance(POLL_MS * 3);
    expect(flow.getSnapshot()).toMatchObject({ phase: 'finalizing' });
    expect(counts.record).toBe(0);
  });

  test('a reload finds the stored recording again and does not POST', async () => {
    const store = new MemoryStore();
    store.setItem(
      recordingKey('st-1'),
      JSON.stringify({ captureId: 'R1', monitor: 'DP-1', seconds: 30, at: 1_700_000_000_000 }),
    );
    const { flow, counts } = await attached({
      storage: store,
      active: async () => ({ state: 'active', capture_id: 'R1', status: progress('recording') }),
    });
    expect(flow.getSnapshot()).toMatchObject({
      kind: 'running',
      captureId: 'R1',
      monitor: 'DP-1',
      phase: 'recording',
    });
    expect(counts.record).toBe(0);
    expect(store.map.size).toBe(1);
  });

  test('idle with a stored ID: one lookup; a terminal state clears it and shows the result', async () => {
    const store = new MemoryStore();
    store.setItem(
      recordingKey('st-1'),
      JSON.stringify({ captureId: 'R1', monitor: 'DP-1', seconds: 30, at: 1 }),
    );
    const { flow, counts } = await attached({ storage: store, status: async () => committed });
    expect(counts.status).toBe(1);
    expect(kind(flow)).toBe('committed');
    expect(store.map.size).toBe(0);
  });

  test('idle with a stored ID that is not terminal: the ID stays, Check result is offered', async () => {
    const store = new MemoryStore();
    store.setItem(
      recordingKey('st-1'),
      JSON.stringify({ captureId: 'R1', monitor: 'DP-1', seconds: 30, at: 1 }),
    );
    const { flow, counts, time } = await attached({ storage: store });
    expect(counts.status).toBe(1);
    expect(kind(flow)).toBe('expired');
    expect(store.map.size).toBe(1);
    expect(time.pending).toBe(0);
  });

  test('a lookup that fails keeps the stored ID', async () => {
    const store = new MemoryStore();
    store.setItem(
      recordingKey('st-1'),
      JSON.stringify({ captureId: 'R1', monitor: 'DP-1', seconds: 30, at: 1 }),
    );
    const { flow } = await attached({
      storage: store,
      status: async () => {
        throw new TypeError('network');
      },
    });
    expect(kind(flow)).toBe('expired');
    expect(store.map.size).toBe(1);
  });

  test('409 station_unreachable: the station is not connected, the stored ID stays', async () => {
    const store = new MemoryStore();
    store.setItem(
      recordingKey('st-1'),
      JSON.stringify({ captureId: 'R1', monitor: 'DP-1', seconds: 30, at: 1 }),
    );
    const { flow, counts } = await attached({
      storage: store,
      active: async () => {
        throw station409();
      },
    });
    expect(flow.getSnapshot()).toEqual({ kind: 'unreachable', captureId: 'R1', error: null });
    expect(store.map.size).toBe(1);
    expect(counts.status).toBe(0);
    expect(recordingId(flow.getSnapshot())).toBe('R1');
  });

  test('409 with nothing stored: the text only, no ID, nothing is blocked', async () => {
    const { flow } = await attached({
      active: async () => {
        throw station409();
      },
    });
    expect(flow.getSnapshot()).toEqual({ kind: 'unreachable', captureId: null, error: null });
    expect(recordingId(flow.getSnapshot())).toBeNull();
    expect(isActive(flow.getSnapshot())).toBe(false);
  });

  test('detach drops a pending discovery; attach asks again', async () => {
    let release: (a: ActiveRecording) => void = () => {};
    const { flow, counts } = setup({
      active: () => new Promise<ActiveRecording>((resolve) => (release = resolve)),
    });
    flow.attach();
    expect(kind(flow)).toBe('discovering');
    expect(isActive(flow.getSnapshot())).toBe(true);
    flow.detach();
    expect(kind(flow)).toBe('idle');
    release({ state: 'active', capture_id: 'R7', status: progress('recording') });
    await flush();
    expect(kind(flow)).toBe('idle');
    flow.attach();
    await flush();
    expect(counts.active).toBe(2);
  });
});

describe('the recording flow: stop', () => {
  test('200: the terminal result is shown and the ID is cleared', async () => {
    const { flow, counts, storage } = await attached({
      active: async () => ({ state: 'active', capture_id: 'R1', status: progress('recording') }),
      stop: async () => committed,
    });
    await flow.stop();
    expect(counts.stop).toBe(1);
    expect(kind(flow)).toBe('committed');
    expect((storage as MemoryStore).map.size).toBe(0);
  });

  test('202: the polling goes on and shows the stopping phase', async () => {
    let polls = 0;
    const { flow, time, counts } = await attached({
      active: async () => ({ state: 'active', capture_id: 'R1', status: progress('recording') }),
      stop: async () => pending,
      status: async () => (++polls < 2 ? progress('stopping') : committed),
    });
    await flow.stop();
    expect(flow.getSnapshot()).toMatchObject({ kind: 'running', phase: 'stopping' });
    await time.advance(POLL_MS * 2);
    expect(kind(flow)).toBe('committed');
    expect(counts.stop).toBe(1);
    expect(counts.record).toBe(0);
  });

  test('a stop that fails shows the deck text and keeps the recording', async () => {
    const { flow } = await attached({
      active: async () => ({ state: 'active', capture_id: 'R1', status: progress('recording') }),
      stop: async () => {
        throw new DeckError(409, 'This capture is not a recording.', 'not_recording', {});
      },
    });
    await flow.stop();
    expect(flow.getSnapshot()).toMatchObject({
      kind: 'running',
      error: 'This capture is not a recording.',
    });
  });

  test('Stop works when the POST is in flight and from the window end', async () => {
    const posting = await attached({ record: () => new Promise<TakeResult>(() => {}) });
    void posting.flow.start('DP-1', 30);
    await posting.flow.stop();
    expect(posting.counts.stop).toBe(1);
    expect(kind(posting.flow)).toBe('committed');

    const expired = await attached({
      record: async () => {
        throw new TypeError('network');
      },
      stop: async () => pending,
    });
    await expired.flow.start('DP-1', 1);
    await expired.time.advance(windowMs(1));
    expect(kind(expired.flow)).toBe('expired');
    await expired.flow.stop();
    expect(expired.flow.getSnapshot()).toMatchObject({ kind: 'running', phase: 'stopping' });
  });

  test('with no known ID, Stop sends nothing', async () => {
    const { flow, counts } = await attached();
    await flow.stop();
    expect(counts.stop).toBe(0);
  });
});

describe('storage failures', () => {
  test('a blocked storage does not break start, polling or discovery', async () => {
    const { flow, time, counts } = await attached({
      storage: throwing,
      record: async () => {
        throw new TypeError('network');
      },
      status: async () => committed,
    });
    await flow.start('DP-1', 30);
    await time.advance(POLL_MS);
    expect(kind(flow)).toBe('committed');
    expect(counts.record).toBe(1);
  });

  test('no storage at all works for the page', async () => {
    const { flow } = await attached({ storage: null });
    await flow.start('DP-1', 30);
    expect(kind(flow)).toBe('committed');
  });

  test('readStored never throws and rejects bad values', () => {
    expect(readStored(throwing, 'st-1')).toBeNull();
    expect(readStored(null, 'st-1')).toBeNull();
    const store = new MemoryStore();
    store.setItem(recordingKey('st-1'), 'not json');
    expect(readStored(store, 'st-1')).toBeNull();
    store.setItem(recordingKey('st-1'), JSON.stringify({ captureId: 1 }));
    expect(readStored(store, 'st-1')).toBeNull();
    store.setItem(
      recordingKey('st-1'),
      JSON.stringify({ captureId: 'R1', monitor: 'DP-1', seconds: 30, at: 5 }),
    );
    expect(readStored(store, 'st-1')).toEqual({
      captureId: 'R1',
      monitor: 'DP-1',
      seconds: 30,
      at: 5,
    });
  });
});
