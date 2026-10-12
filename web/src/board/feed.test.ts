import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { createElement } from 'react';
import { App } from '../App';
import type { DeckSession, OpenRequest } from '../gen';
import { emptyState, reduceMessage, type RuntimeState } from '../runtime/state';
import { summary } from '../testing/summary';
import { appendFeed, clock, FEED_MAX, type FeedItem, feedItems } from './feed';

const ST = 'station-a';

function card(id: string, seq: number, extra: Partial<DeckSession['summary']> = {}): DeckSession {
  return {
    station_id: ST,
    status: 'working',
    summary: summary(id, 'working', 1_760_000_000_000 + seq, {
      session_seq: seq,
      cwd: '/w/app',
      ...extra,
    }),
  };
}

const ask = (id: string): OpenRequest => ({
  request_id: id,
  node_id: null,
  request_kind: 'command',
  title: `Run ${id}`,
  details: 'ls',
  input: null,
  options: [{ option_id: 'allow', label: 'Allow' }],
  response: 'rpc_reply',
  requested_at: 1,
});

const texts = (items: readonly FeedItem[]) => items.map((i) => `${i.kind}: ${i.text}`);

describe('event feed items (DL-095)', () => {
  test('a new card joins and lists its open asks', () => {
    const items = feedItems(undefined, card('s1', 3, { open_requests: [ask('r1')] }));
    expect(texts(items)).toEqual(['join: joined in /w/app', 'ask: asks: Run r1']);
    expect(items[0]?.key).toEqual({ station_id: ST, session_id: 's1' });
    expect(new Set(items.map((i) => i.id)).size).toBe(2);
  });

  test('each change of a card gives one item; an unchanged card gives none', () => {
    const before = card('s1', 3, {
      title: 'Old',
      current_turn: 't1',
      open_requests: [ask('r1')],
    });
    const after = card('s1', 9, {
      title: 'New',
      current_turn: null,
      open_requests: [ask('r1'), ask('r2')],
      running_tools: [{ node_id: 'n1', tool_kind: 'shell', title: 'Bash', input_summary: 'ls' }],
      revert: { revert_id: 'rv', turn_id: 't1', state: 'failed', reason: 'rollback_refused' },
      last_error: { code: 'usage_limit', message: 'try later', retry_at: null },
      state: 'exited',
    });
    expect(texts(feedItems(before, after))).toEqual([
      'rename: renamed from Old',
      'ask: asks: Run r2',
      'tool: Bash: ls',
      'turn: turn ended',
      'revert: revert failed (rollback_refused)',
      'error: usage_limit: try later',
      'end: ended',
    ]);
    expect(feedItems(after, after)).toEqual([]);
    const cleared = card('s1', 10, { title: null });
    expect(texts(feedItems(card('s1', 9, { title: 'New' }), cleared))).toEqual([
      'rename: title cleared (was New)',
    ]);
  });

  test('the feed keeps the newest items and the clock has seconds', () => {
    const many = Array.from({ length: FEED_MAX + 5 }, (_, n) =>
      feedItems(undefined, card(`s${n}`, n)),
    ).flat();
    const feed = appendFeed([], many);
    expect(feed.length).toBe(FEED_MAX);
    expect(feed[0]?.key.session_id).toBe('s5');
    expect(clock(new Date(2026, 9, 12, 7, 5, 9).getTime())).toBe('07:05:09');
  });
});

describe('event feed in the runtime state', () => {
  const live = (): RuntimeState =>
    reduceMessage(emptyState(), {
      type: 'snapshot',
      version: 1,
      stations: [],
      sessions: [card('s1', 1)],
    });

  test('the first snapshot adds nothing; deltas add their changes', () => {
    let state = live();
    expect(state.feed).toEqual([]);
    state = reduceMessage(state, {
      type: 'delta',
      version: 2,
      stations: null,
      sessions: [card('s1', 2, { title: 'Docs' }), card('s2', 1)],
      removed: [],
    });
    expect(texts(state.feed)).toEqual(['rename: renamed from s1', 'join: joined in /w/app']);
    // An old delta is ignored, with its items.
    const same = reduceMessage(state, {
      type: 'delta',
      version: 2,
      stations: null,
      sessions: [card('s3', 1)],
      removed: [],
    });
    expect(same.feed).toBe(state.feed);
  });

  test('a snapshot after a reconnect adds what the board missed', () => {
    const stale = { ...live(), freshness: 'stale' as const };
    const state = reduceMessage(stale, {
      type: 'snapshot',
      version: 7,
      stations: [],
      sessions: [card('s1', 4, { state: 'lost' })],
    });
    expect(texts(state.feed)).toEqual(['end: lost']);
  });

  test('the fleet screen shows the feed newest first, or a quiet note', () => {
    const quiet = renderToStaticMarkup(createElement(App, { freshness: 'live' }));
    expect(quiet).toContain('aria-label="Event feed"');
    expect(quiet).toContain('Quiet so far.');
    const feed = [
      ...feedItems(undefined, card('s1', 1)),
      ...feedItems(card('s1', 1), card('s1', 2, { title: 'Docs' })),
    ];
    const html = renderToStaticMarkup(createElement(App, { freshness: 'live', feed }));
    expect(html.indexOf('renamed from s1')).toBeLessThan(html.indexOf('joined in /w/app'));
    expect(html).toContain('data-feed-kind="rename"');
    expect(html).toMatch(/aria-pressed="true"[^>]*>All</);
  });
});
