import { describe, expect, test } from 'bun:test';
import type { HistoryItem, HistoryUnavailable, SessionHistory } from '../gen';
import { ApiError } from '../runtime/api';
import { HistoryLoader, historyErrorLine, unavailableText } from './history';

const item = (id: string): HistoryItem => ({ id, at: 0, kind: 'user', text: id, tool: null });
const page = (
  ids: string[],
  before: string | null,
  state: SessionHistory['state'] = { kind: 'complete' },
): SessionHistory => ({ items: ids.map(item), before, state });
const ids = (loader: HistoryLoader) => loader.getSnapshot().items.map((i) => i.id);
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** A fake `load`: it records the `before` of each call and answers from a queue. */
function fake(...answers: (SessionHistory | Error)[]) {
  const calls: (string | null)[] = [];
  const load = async (before: string | null): Promise<SessionHistory> => {
    calls.push(before);
    const next = answers.shift();
    if (next === undefined) throw new Error('no answer');
    if (next instanceof Error) throw next;
    return next;
  };
  return { calls, load };
}

describe('the texts of the history (DL-189)', () => {
  test('unavailableText gives the words of each reason', () => {
    const gone = 'The earlier history is not available: ';
    const want: Record<HistoryUnavailable, string> = {
      no_native_id: `${gone}the session has no provider session id.`,
      not_found: `${gone}the transcript file was not found.`,
      ambiguous: `${gone}more than one transcript file matches.`,
      not_trusted: `${gone}the transcript file failed a safety check.`,
      too_large: `${gone}the transcript file is too large.`,
      unsupported_format: `${gone}the transcript format is not known.`,
      not_captured: 'The earlier history of this session is shown to the owner only.',
      busy: 'The station is busy. Try again in a moment.',
    };
    for (const [reason, text] of Object.entries(want)) {
      const line = unavailableText(reason as HistoryUnavailable);
      expect(line.text).toBe(text);
      expect(line.action).toBe(reason === 'busy' ? 'retry' : null);
    }
  });

  test('historyErrorLine gives the line of each error code', () => {
    expect(historyErrorLine(new ApiError(403, 'x', 'history_not_shared'))).toBeNull();
    expect(historyErrorLine(new ApiError(403, 'x', 'owner_only'))).toBeNull();
    expect(historyErrorLine(new ApiError(409, 'x', 'history_unsupported'))).toEqual({
      text: 'This station runs an older Fleet Deck. Update it to see the earlier history.',
      action: null,
    });
    expect(historyErrorLine(new ApiError(409, 'x', 'stale_cursor'))).toEqual({
      text: 'The history changed. Load it again from the start.',
      action: 'retry',
    });
    expect(historyErrorLine(new ApiError(503, 'x', 'busy'))).toEqual({
      text: 'The station is busy. Try again in a moment.',
      action: 'retry',
    });
    expect(historyErrorLine(new ApiError(504, 'timed out', 'timeout'))).toEqual({
      text: 'The earlier history could not be loaded: timed out',
      action: 'retry',
    });
    expect(historyErrorLine(new Error('boom'))?.text).toBe(
      'The earlier history could not be loaded: boom',
    );
  });
});

describe('HistoryLoader (DL-189)', () => {
  test('the first page is asked once, and only after start', async () => {
    const { calls, load } = fake(page(['a'], null));
    const loader = new HistoryLoader(load);
    await settle();
    expect(calls).toEqual([]);
    loader.start();
    loader.start();
    await settle();
    loader.start();
    await settle();
    expect(calls).toEqual([null]);
    expect(ids(loader)).toEqual(['a']);
    expect(loader.getSnapshot().more).toBe(false);
  });

  test('loadEarlier sends the cursor and puts the older items first', async () => {
    const { calls, load } = fake(page(['c', 'd'], 'k1'), page(['a', 'b'], null));
    const loader = new HistoryLoader(load);
    loader.start();
    await settle();
    expect(loader.getSnapshot().more).toBe(true);
    loader.loadEarlier();
    expect(loader.getSnapshot().loading).toBe(true);
    // A second call while a page loads does nothing.
    loader.loadEarlier();
    await settle();
    expect(calls).toEqual([null, 'k1']);
    expect(ids(loader)).toEqual(['a', 'b', 'c', 'd']);
    expect(loader.getSnapshot().more).toBe(false);
    loader.loadEarlier();
    await settle();
    expect(calls).toHaveLength(2);
  });

  test('the state of the last answer decides the line', async () => {
    const { load } = fake(
      page(['c'], 'k1', { kind: 'partial', skipped: 2 }),
      page(['b'], null, { kind: 'complete' }),
    );
    const loader = new HistoryLoader(load);
    loader.start();
    await settle();
    expect(loader.getSnapshot().line?.text).toBe('Some lines of the transcript could not be read.');
    loader.loadEarlier();
    await settle();
    expect(loader.getSnapshot().line).toBeNull();
  });

  test('history_not_shared leaves no block and no line', async () => {
    const { load } = fake(new ApiError(403, 'no', 'history_not_shared'));
    const loader = new HistoryLoader(load);
    loader.start();
    await settle();
    expect(loader.getSnapshot()).toMatchObject({
      items: [],
      more: false,
      loading: false,
      line: null,
    });
  });

  test('retry after stale_cursor loads from the start and replaces the items', async () => {
    const { calls, load } = fake(
      page(['c'], 'k1'),
      new ApiError(409, 'stale', 'stale_cursor'),
      page(['x', 'c'], 'k2'),
    );
    const loader = new HistoryLoader(load);
    loader.start();
    await settle();
    loader.loadEarlier();
    await settle();
    expect(loader.getSnapshot().line?.action).toBe('retry');
    expect(ids(loader)).toEqual(['c']);
    loader.retry();
    await settle();
    expect(calls).toEqual([null, 'k1', null]);
    expect(ids(loader)).toEqual(['x', 'c']);
    expect(loader.getSnapshot().line).toBeNull();
  });

  test('a late answer after dispose or after a new loader is dropped', async () => {
    let release: (v: SessionHistory) => void = () => {};
    const slow = new Promise<SessionHistory>((resolve) => {
      release = resolve;
    });
    const old = new HistoryLoader(() => slow);
    old.start();
    old.dispose();
    const next = new HistoryLoader(fake(page(['new'], null)).load);
    next.start();
    release(page(['old'], null));
    await settle();
    expect(ids(old)).toEqual([]);
    expect(ids(next)).toEqual(['new']);
  });

  test('a retry drops the answer of the request that it replaced', async () => {
    const resolvers: ((v: SessionHistory) => void)[] = [];
    const loader = new HistoryLoader(
      () => new Promise<SessionHistory>((resolve) => resolvers.push(resolve)),
    );
    loader.start();
    loader.retry();
    resolvers[0]?.(page(['stale'], null));
    resolvers[1]?.(page(['fresh'], null));
    await settle();
    expect(ids(loader)).toEqual(['fresh']);
  });

  test('the snapshot is the same object until a change, and listeners hear a change', async () => {
    const { load } = fake(page(['a'], null));
    const loader = new HistoryLoader(load);
    let heard = 0;
    const off = loader.subscribe(() => {
      heard += 1;
    });
    const first = loader.getSnapshot();
    expect(loader.getSnapshot()).toBe(first);
    loader.start();
    await settle();
    expect(loader.getSnapshot()).not.toBe(first);
    expect(heard).toBe(2);
    off();
  });
});
