// R13 P1: the wall in each mode renders no write control; the owner login only; the pure parts.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { wallDefaults, type WallRoute } from '../app/routes';
import type { ActivityItem, DeckSession, Event, OpenRequest, ReviewComment } from '../gen';
import type { ReviewState } from '../review/useReview';
import { sessionActions } from '../session/commands';
import { claudeCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { envelopes } from '../testing/timeline';
import { NeedsArea, SideArea, StreamArea } from './Areas';
import { ChatPane } from './ChatPane';
import { stationStats } from './stationStats';
import { WallPage } from './WallPage';
import {
  ambientCounts,
  asksOf,
  needsList,
  sentOnly,
  stepMode,
  toolParts,
  wallSessions,
  withMode,
} from './model';

const request = (at: number): OpenRequest => ({
  request_id: `r${at}`,
  node_id: null,
  request_kind: 'tool',
  title: 'Run tests',
  details: 'cargo test\nsecond line',
  input: null,
  options: [
    { option_id: 'allow', label: 'Allow' },
    { option_id: 'deny', label: 'Deny' },
  ],
  response: 'rpc_reply',
  requested_at: at,
});

const waiting = (id: string, at: number, stationId = 'station-a') =>
  card(id, 'needs_you', stationId, {
    title: `Session ${id}`,
    callsign: `call-${id}`,
    capabilities: claudeCaps(),
    open_requests: [request(at)],
  });

const said: Event = {
  kind: 'message.completed',
  turn_id: 't1',
  parent_node_id: null,
  text: 'Hello',
};

const sessions: DeckSession[] = [
  waiting('late', 300),
  waiting('early', 100),
  card('busy', 'working', 'station-a', {
    title: 'Busy one',
    batch: { batch_id: 'B1', index: 1, size: 2 },
  }),
  card('twin', 'working', 'station-a', { batch: { batch_id: 'B1', index: 2, size: 2 } }),
  card('done', 'ended'),
];

const timelines = {
  [JSON.stringify(['station-a', 'busy'])]: {
    incarnation: 'i',
    cursor: 1,
    events: envelopes('busy', [said]),
    freshness: 'live' as const,
    error: null,
    ready: true,
  },
};

const wall = (route: Partial<WallRoute>, owner = true, size = { w: 3440, h: 1440 }) =>
  renderToStaticMarkup(
    <WallPage
      route={{ ...wallDefaults, ...route }}
      navigate={() => {}}
      owner={owner}
      sessions={sessions}
      stations={[station()]}
      freshness="live"
      timelines={timelines}
      sessionActions={(key) => sessionActions(async () => ({}), key, ['read', 'drive', 'approve'])}
      initialSize={size}
      now={1_000_000}
      origin="https://deck.example"
    />,
  );

const here = { station: 'station-a', id: 'busy' };
const MODES: Partial<WallRoute>[] = [
  { mode: 'mission' },
  { mode: 'focus', session: here },
  { mode: 'review', session: here },
  { mode: 'race', batch: 'B1' },
  { mode: 'artifact', artifact: { station: 'station-a', id: 'art' } },
  { mode: 'ambient' },
];

describe('the wall renders no write control', () => {
  for (const route of MODES) {
    test(`mode ${route.mode}`, () => {
      const html = wall(route);
      expect(html).not.toMatch(/<(button|textarea|input|select|form)\b/);
      expect(html).not.toContain('<a ');
      expect(html).toContain(`data-mode="${route.mode}"`);
    });
  }

  test('the mission mode has the four areas and a QR for each needs-you item', () => {
    const html = wall({});
    for (const area of ['needs', 'tiles', 'side', 'stream']) {
      expect(html).toContain(`data-area="${area}"`);
    }
    expect(html.match(/Scan to answer on your phone/g)).toHaveLength(2);
    expect(html).toContain(
      'aria-label="QR code for https://deck.example/#/s/station-a/early?request=r100"',
    );
    expect(html).not.toContain('Allow');
    expect(html).toContain('Fleet Deck wall');
    expect(html).toContain('>Live<');
  });

  test('with no request the needs area says the calm words of the board, in green', () => {
    const html = renderToStaticMarkup(
      <NeedsArea needs={[]} stations={[station()]} origin="https://deck.example" now={1} />,
    );
    expect(html).toContain('<p class="fd-wall-note fd-wall-clear">All clear.</p>');
    expect(html).not.toContain('Nothing needs you.');
    const calm = wall({ mode: 'ambient' }, true, { w: 3440, h: 1440 });
    expect(calm).not.toContain('Nothing needs you.');
  });

  test('the needs list is oldest first', () => {
    const html = wall({});
    expect(html.indexOf('call-early')).toBeLessThan(html.indexOf('call-late'));
  });

  test('the focus mode shows the chat of the session', () => {
    const html = wall({ mode: 'focus', session: here });
    expect(html).toContain('Hello');
    expect(html).toContain('Since the session started');
  });

  test('a session that is not on the board is said so', () => {
    expect(wall({ mode: 'focus', session: { station: 'x', id: 'nope' } })).toContain(
      'This session is not on the board.',
    );
  });
});

describe('the owner login only', () => {
  test('another login sees the notice and no data', () => {
    const html = wall({}, false);
    expect(html).toContain('The wall needs the owner login.');
    expect(html).not.toContain('Session early');
    expect(html).not.toContain('data-area');
    expect(html).not.toContain('Fleet Deck wall');
  });
});

describe('the shape of the grid', () => {
  test('the root carries the shape class from the size, or from the override', () => {
    expect(wall({}, true, { w: 3440, h: 1440 })).toContain('data-shape="ultra"');
    expect(wall({}, true, { w: 2560, h: 1440 })).toContain('data-shape="wide"');
    expect(wall({}, true, { w: 1600, h: 1200 })).toContain('data-shape="standard"');
    expect(wall({}, true, { w: 1080, h: 1920 })).toContain('data-shape="portrait"');
    expect(wall({ shape: 'portrait' }, true, { w: 3440, h: 1440 })).toContain(
      'data-shape="portrait"',
    );
  });

  test('the scale is a custom property of the root', () => {
    expect(wall({ scale: 1.5 })).toContain('--fd-wall-scale:1.5');
  });

  test('portrait has no side area and standard has no stream', () => {
    const portrait = wall({}, true, { w: 1080, h: 1920 });
    expect(portrait).not.toContain('data-area="side"');
    expect(portrait).toContain('data-area="stream"');
    const standard = wall({}, true, { w: 1600, h: 1200 });
    expect(standard).toContain('data-area="side"');
    expect(standard).not.toContain('data-area="stream"');
  });
});

describe('the pure parts', () => {
  test('the filter keeps the stations and the repository of the route', () => {
    const all = [...sessions, card('on-b', 'working', 'station-b')];
    expect(wallSessions(all, { stations: ['station-b'], repo: '' })).toHaveLength(1);
    expect(wallSessions(all, { stations: [], repo: '' })).toHaveLength(all.length);
  });

  test('the needs list skips an ended session and sorts by age', () => {
    const ended = card('gone', 'ended', 'station-a', { open_requests: [request(1)] });
    const list = needsList([...sessions, ended]);
    expect(list.map((n) => n.request.request_id)).toEqual(['r100', 'r300']);
    expect(asksOf(list).map((a) => a.ref)).toEqual(['station-a/early', 'station-a/late']);
  });

  test('the mode cycle skips the modes without a target', () => {
    const fallback = { station: 'station-a', id: 'busy' };
    const start: WallRoute = { ...wallDefaults };
    const next = stepMode(start, 1, fallback);
    expect(next.mode).toBe('focus');
    expect(next.session).toEqual(fallback);
    const review = stepMode(next, 1, fallback);
    expect(review.mode).toBe('review');
    // race and artifact have no target: the next stop is ambient
    expect(stepMode(review, 1, fallback).mode).toBe('ambient');
    expect(stepMode(stepMode(review, 1, fallback), 1, fallback).mode).toBe('mission');
    expect(stepMode(start, -1, fallback).mode).toBe('ambient');
    // without a fallback session, focus and review are skipped too
    expect(stepMode(start, 1, null).mode).toBe('ambient');
    // with the target in the route, race is a stop
    expect(stepMode(review, 1, fallback).mode).toBe('ambient');
    expect(stepMode({ ...review, batch: 'B1' }, 1, fallback).mode).toBe('race');
  });

  test('a mode without its target is null', () => {
    expect(withMode(wallDefaults, 'race', null)).toBeNull();
    expect(withMode(wallDefaults, 'artifact', null)).toBeNull();
    expect(withMode(wallDefaults, 'focus', null)).toBeNull();
    expect(withMode(wallDefaults, 'ambient', null)?.mode).toBe('ambient');
  });

  test('the ambient counts', () => {
    const now = Date.UTC(2026, 9, 16, 12);
    const today = card('d1', 'ended', 'station-a', { updated_at: now - 1000 });
    const old = card('d2', 'ended', 'station-a', { updated_at: now - 3 * 86_400_000 });
    expect(ambientCounts([...sessions, today, old], now)).toEqual({
      running: 2,
      needs: 2,
      done: 1,
    });
  });
});

const comment = (id: string, state: 'draft' | 'sent', body: string): ReviewComment => ({
  comment_id: id,
  short_id: `c-${id}`,
  thread_id: 't',
  snapshot: 'spawn:abc',
  path: 'src/a.ts',
  side: 'new',
  line_start: 1,
  line_end: 1,
  anchor_text: 'x',
  anchor_kind: 'diff',
  body,
  author_kind: 'owner',
  state,
  resolved: false,
  created_at: 1,
  updated_at: 1,
});

const reviewState = (threads: { thread_id: string; comments: ReviewComment[] }[]): ReviewState => ({
  data: { threads, reviews: [], rev: 1, more_reviews: false },
  error: null,
  reload: () => {},
  byShort: new Map(),
  known: new Set(),
  moreReviews: false,
  loadOlder: () => {},
});

describe('the review pane shows sent comments only', () => {
  test('a sent thread with a draft reply keeps the sent comment and drops the reply', () => {
    const state = sentOnly(
      reviewState([
        {
          thread_id: 't',
          comments: [comment('1', 'sent', 'Sent note'), comment('2', 'draft', 'Draft reply')],
        },
      ]),
    );
    const text = JSON.stringify(state.data?.threads);
    expect(text).toContain('Sent note');
    expect(text).not.toContain('Draft reply');
  });

  test('a thread that starts with a draft is dropped, and so is a resolved one', () => {
    const resolved = { ...comment('4', 'sent', 'Resolved note'), resolved: true };
    const state = sentOnly(
      reviewState([
        { thread_id: 'a', comments: [comment('3', 'draft', 'Draft top')] },
        { thread_id: 'b', comments: [resolved] },
        { thread_id: 'c', comments: [comment('5', 'sent', 'Kept')] },
      ]),
    );
    expect(state.data?.threads.map((t) => t.thread_id)).toEqual(['c']);
  });
});

describe('the stream follows the wall filter', () => {
  const item = (station: string, id: string, text: string): ActivityItem => ({
    station_id: station,
    session_id: id,
    session_title: id,
    repo: null,
    kind: 'message',
    text,
    at: 1,
    station_seq: 1,
  });
  const items = [
    item('station-a', 'busy', 'Alpha said hi'),
    item('station-b', 'on-b', 'Beta said hi'),
  ];
  const board = [...sessions, card('on-b', 'working', 'station-b')];
  const stream = (stations: string[]) =>
    renderToStaticMarkup(
      <StreamArea
        views={undefined}
        sessions={board}
        filter={{ stations, repo: '' }}
        live={false}
        now={1_000_000}
        initialItems={items}
      />,
    );

  test('a wall with one station shows no activity of another', () => {
    const html = stream(['station-a']);
    expect(html).toContain('Alpha said hi');
    expect(html).not.toContain('Beta said hi');
  });

  test('a wall without a station filter shows all the activity', () => {
    const html = stream([]);
    expect(html).toContain('Alpha said hi');
    expect(html).toContain('Beta said hi');
  });

  test('the list keeps the deck order: the newest row is the first in the DOM', () => {
    const html = stream([]);
    expect(html.indexOf('Alpha said hi')).toBeLessThan(html.indexOf('Beta said hi'));
  });

  test('the session is a column with its whole name in the title, then the event', () => {
    const html = stream(['station-a']);
    expect(html).toContain('<strong title="busy">busy</strong> <span class="fd-wall-event">');
  });

  test('a provider error in a message row reads as its cause, with the raw words in the title', () => {
    const html = renderToStaticMarkup(
      <StreamArea
        views={undefined}
        sessions={board}
        filter={{ stations: [], repo: '' }}
        live={false}
        now={1_000_000}
        initialItems={[
          item(
            'station-a',
            'busy',
            'API Error: Request rejected (429) · All credentials are cooling down',
          ),
        ]}
      />,
    );
    expect(html).toContain('The provider&#x27;s rate limit stopped it. Try again later.');
    expect(html).not.toContain('>API Error');
    expect(html).toContain(
      'title="API Error: Request rejected (429) · All credentials are cooling down"',
    );
  });

  test('prose is one plain line: no Markdown marks, the first line only', () => {
    const html = renderToStaticMarkup(
      <StreamArea
        views={undefined}
        sessions={board}
        filter={{ stations: [], repo: '' }}
        live={false}
        now={1_000_000}
        initialItems={[
          item(
            'station-a',
            'busy',
            'The fix workflow (`wnzrhrv6c`) is **running**. It works in this order:\n1. Read',
          ),
        ]}
      />,
    );
    expect(html).toContain('The fix workflow (wnzrhrv6c) is running. It works in this order:');
    expect(html).not.toContain('`');
    expect(html).not.toContain('1. Read');
  });
});

describe('a tool row of the stream draws its target as code', () => {
  test('the tool name and a request label stay words; the command or the path is the target', () => {
    expect(toolParts('tool', 'Edit src/checkout/cart.ts')).toEqual({
      lead: 'Edit',
      target: 'src/checkout/cart.ts',
    });
    expect(toolParts('tool', 'Bash: cargo test')).toEqual({ lead: 'Bash:', target: 'cargo test' });
    expect(toolParts('approval', 'Permission: Bash cargo nextest run')).toEqual({
      lead: 'Permission: Bash',
      target: 'cargo nextest run',
    });
  });

  test('prose, an unknown tool and the other kinds stay words', () => {
    expect(toolParts('approval', 'Question: Use the test key?')).toBeNull();
    expect(toolParts('tool', 'Tool call (ok)')).toBeNull();
    expect(toolParts('tool', 'Task: write the docs')).toBeNull();
    expect(toolParts('tool', 'Bash')).toBeNull();
    expect(toolParts('message', 'Read the file first')).toBeNull();
  });

  test('the stream renders the target in a code element and keeps the text', () => {
    const html = renderToStaticMarkup(
      <StreamArea
        views={undefined}
        sessions={sessions}
        filter={{ stations: [], repo: '' }}
        live={false}
        now={1_000_000}
        initialItems={[
          {
            station_id: 'station-a',
            session_id: 'busy',
            session_title: 'busy',
            repo: null,
            kind: 'tool',
            text: 'Edit src/a.ts',
            at: 1,
            station_seq: 1,
          },
        ]}
      />,
    );
    // The tool name in words (weight 600), the target in mono: the Activity page reading.
    expect(html).toContain('<b class="fd-wall-tool">Edit</b> <code>src/a.ts</code>');
  });
});

describe('the focus chat pane', () => {
  test('a ready chat without lines says so', () => {
    const html = renderToStaticMarkup(<ChatPane events={[]} ready unavailable={false} />);
    expect(html).toContain('No chat yet.');
  });

  test('a chat that loads says so', () => {
    const html = renderToStaticMarkup(<ChatPane events={[]} ready={false} unavailable={false} />);
    expect(html).toContain('Loading…');
    expect(html).not.toContain('No chat yet.');
  });

  test('a provider error in an agent message reads as its cause, with the raw words in the title', () => {
    const raw = 'API Error: Request rejected (429) · All credentials are cooling down';
    const html = renderToStaticMarkup(
      <ChatPane
        events={envelopes('s', [
          { kind: 'message.completed', turn_id: 't1', parent_node_id: null, text: raw },
        ])}
        ready
        unavailable={false}
      />,
    );
    expect(html).toContain('The provider&#x27;s rate limit stopped it. Try again later.');
    expect(html).not.toContain('>API Error');
    expect(html).toContain(`title="${raw}"`);
  });

  test('a call that is not done says its state to a screen reader', () => {
    const started: Event = {
      kind: 'tool.started',
      node_id: 'n1',
      parent_node_id: null,
      tool_kind: 'shell',
      title: 'Bash',
      input_summary: 'cargo test',
      files: [],
      provider_refs: {},
    };
    const running = renderToStaticMarkup(
      <ChatPane events={envelopes('s', [started])} ready unavailable={false} />,
    );
    expect(running).toContain('data-state="running"');
    expect(running).toContain('<span class="fd-vh"> · Running</span>');
    const ended = renderToStaticMarkup(
      <ChatPane
        events={envelopes('s', [
          started,
          { kind: 'session.ended', reason: 'process_exit', exit_code: 0 },
        ])}
        ready
        unavailable={false}
      />,
    );
    expect(ended).toContain('data-state="ended"');
    expect(ended).toContain('<span class="fd-vh"> · Ended</span>');
  });
});

describe('the figures of a station row', () => {
  test('the CPU is a share of all cores, the memory whole GB, the disk ends in free', () => {
    expect(
      stationStats({
        cpu: { cores: 32, load1_x100: 0, load5_x100: 30 },
        memory: { total_bytes: 46.8e9, available_bytes: 40.8e9 },
        disks: [
          { total_bytes: 2000e9, free_bytes: 1204.7e9 },
          { total_bytes: 500e9, free_bytes: 216.9e9 },
        ],
      }),
    ).toEqual(['CPU 1%', 'Memory 6 of 47 GB', 'Disk 217 GB free']);
    expect(
      stationStats({ cpu: { cores: 4, load1_x100: 0, load5_x100: 600 }, memory: null, disks: [] }),
    ).toEqual(['CPU 150%']);
  });

  test('the glyph says online (the word is for a screen reader); offline shows in words', () => {
    const html = renderToStaticMarkup(
      <SideArea
        sessions={[]}
        stations={[station('station-a'), station('station-b', { reachable: false })]}
        now={1}
      />,
    );
    expect(html).toContain(
      '<span class="fd-wall-station-name">Alpha</span><span class="fd-vh"> online</span>',
    );
    expect(html).toContain(
      '<span class="fd-wall-station-name">Beta</span><span class="fd-wall-note">Offline</span>',
    );
    expect(html).not.toContain('Online');
  });

  test('a part that the station does not report is left out', () => {
    expect(stationStats(null)).toEqual([]);
    expect(
      stationStats({ cpu: { cores: 0, load1_x100: 0, load5_x100: 50 }, memory: null, disks: [] }),
    ).toEqual([]);
  });
});
