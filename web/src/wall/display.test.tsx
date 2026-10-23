// R13 P2c: the wall for a display login: its own timeline events, its change summary, its artifact
// pane and the reduced station resources. A display renders no control and no owner data.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { wallDefaults, type WallRoute } from '../app/routes';
import { changesPath } from '../display/api';
import type {
  BoardServerMessage,
  DeckSession,
  DisplayChanges,
  DisplayEvent,
  DisplayItem,
} from '../gen';
import { ApiError } from '../runtime/api';
import { emptyState, reduceMessage, sessionKey } from '../runtime/state';
import { card, station } from '../testing/fleet';
import { SideArea } from './Areas';
import {
  CARD_TEXT,
  DisplayArtifactPane,
  DisplayMarkdown,
  failureState,
  successState,
} from './DisplayArtifactPane';
import {
  ChangesPane,
  type ChangesState,
  type DisplayEnv,
  joinPatches,
  readChanges,
} from './displayChanges';
import { displayChatLines, displayTileLines, lastDisplayMessage } from './displayLines';
import { WallPage } from './WallPage';

const ev = (seq: number, item: DisplayItem): DisplayEvent => ({
  station_seq: seq,
  session_seq: seq,
  at: seq,
  item,
});

const events: DisplayEvent[] = [
  ev(1, { item: 'run_started', run_id: 'r', summary: 'Fix the login bug' }),
  ev(2, { item: 'turn_started', run_id: 'r', turn_id: 't' }),
  ev(3, { item: 'message', turn_id: 't', text: 'I will read the file.\nThen edit it.' }),
  ev(4, { item: 'tool', node_id: 'n1', name: 'Bash', status: 'failed' }),
  ev(5, { item: 'tool', node_id: 'n2', name: 'Read', status: 'ok' }),
  ev(6, {
    item: 'request',
    request_id: 'q',
    kind: 'command',
    tool_name: 'Bash',
    state: 'opened',
  }),
  ev(7, { item: 'request', request_id: 'q', kind: 'command', tool_name: null, state: 'resolved' }),
  ev(8, { item: 'run_ended', run_id: 'r', status: 'done' }),
];

describe('the lines of a display timeline', () => {
  test('the chat shows the fixed fields only', () => {
    const lines = displayChatLines(events);
    expect(lines.map((l) => [l.kind, l.text])).toEqual([
      ['prompt', 'Fix the login bug'],
      ['message', 'I will read the file.\nThen edit it.'],
      ['tool', 'Bash (failed)'],
      ['tool', 'Read'],
      ['ask', 'Waiting for an answer · Bash'],
      ['tool', 'Approve a command · answered'],
      ['tool', 'Run ended · done'],
    ]);
  });

  test('a tile shows the last text and tool lines, one line each, without the run end', () => {
    const lines = displayTileLines(events);
    expect(lines.map((l) => l.text)).toEqual([
      'I will read the file.',
      'Bash (failed)',
      'Read',
      'Approve a command · answered',
    ]);
    expect(displayTileLines(events, 2)).toHaveLength(2);
  });

  test('one tool call is one line: its end replaces its start', () => {
    const call: DisplayEvent[] = [
      ev(1, { item: 'tool', node_id: 'n1', name: 'Bash', status: null }),
      ev(2, { item: 'tool', node_id: 'n1', name: 'Bash', status: 'failed' }),
      ev(3, { item: 'tool', node_id: 'n2', name: 'Read', status: null }),
    ];
    expect(displayTileLines(call).map((l) => [l.key, l.text])).toEqual([
      ['1', 'Bash (failed)'],
      ['3', 'Read'],
    ]);
    expect(displayChatLines(call).map((l) => l.text)).toEqual(['Bash (failed)', 'Read']);
  });

  test('a message with only Markdown marks draws no blank tile row', () => {
    const marks: DisplayEvent[] = [
      ev(1, { item: 'message', turn_id: 't', text: 'First' }),
      ev(2, { item: 'message', turn_id: 't', text: '``' }),
      ev(3, { item: 'message', turn_id: 't', text: '**Second**' }),
    ];
    expect(displayTileLines(marks).map((l) => l.text)).toEqual(['First', 'Second']);
  });

  test('the last message and an expired request', () => {
    expect(lastDisplayMessage(events)).toContain('I will read the file.');
    expect(lastDisplayMessage([])).toBeNull();
    const expired = displayChatLines([
      ev(1, {
        item: 'request',
        request_id: 'q',
        kind: 'question',
        tool_name: null,
        state: 'expired',
      }),
    ]);
    expect(expired[0]?.text).toBe('Answer a question · expired');
  });
});

describe('the reducer of a display timeline', () => {
  const key = { station_id: 'st', session_id: 'se' };
  const id = sessionKey(key);
  const live = () =>
    [
      { type: 'hello', protocol: 1, deck_incarnation: 'd' },
      { type: 'timeline_start', ...key, incarnation: 'i', reset: true },
    ] as BoardServerMessage[];
  const start = () => {
    let state = emptyState();
    // The board adds the timeline at `watch`; the reducer needs its row.
    state = { ...state, timelines: { [id]: { ...emptyTimelineRow() } } };
    return live().reduce(reduceMessage, state);
  };
  const frame = (list: DisplayEvent[], high: number): BoardServerMessage => ({
    type: 'display_timeline_events',
    ...key,
    events: list,
    high_water: high,
  });

  function emptyTimelineRow() {
    return {
      incarnation: null,
      cursor: 0,
      events: [],
      freshness: 'stale' as const,
      error: null,
      ready: false,
    };
  }

  test('events are kept and the cursor follows the high-water mark', () => {
    const state = reduceMessage(start(), frame(events.slice(0, 3), 9));
    const t = state.timelines[id];
    expect(t?.display?.map((e) => e.station_seq)).toEqual([1, 2, 3]);
    expect(t?.cursor).toBe(9);
    expect(t?.events).toEqual([]);
  });

  test('an event at or below the cursor is not kept twice, and a frame with none still moves it', () => {
    let state = reduceMessage(start(), frame(events.slice(0, 2), 2));
    state = reduceMessage(state, frame(events.slice(0, 3), 3));
    expect(state.timelines[id]?.display?.map((e) => e.station_seq)).toEqual([1, 2, 3]);
    state = reduceMessage(state, frame([], 50));
    expect(state.timelines[id]?.cursor).toBe(50);
  });

  test('a frame before the timeline is ready, or for an unknown session, changes nothing', () => {
    const fresh = emptyState();
    expect(reduceMessage(fresh, frame(events, 8))).toBe(fresh);
    const before = {
      ...emptyState(),
      timelines: { [id]: emptyTimelineRow() },
    };
    expect(reduceMessage(before, frame(events, 8))).toBe(before);
  });

  test('a broken frame is refused', () => {
    const bad = frame([ev(1.5, { item: 'message', turn_id: 't', text: 'x' })], 2);
    expect(() => reduceMessage(start(), bad)).toThrow('Invalid timeline envelope');
  });

  test('a reset drops the display events', () => {
    let state = reduceMessage(start(), frame(events.slice(0, 2), 2));
    state = reduceMessage(state, {
      type: 'timeline_start',
      ...key,
      incarnation: 'other',
      reset: true,
    });
    expect(state.timelines[id]?.display ?? []).toEqual([]);
    expect(state.timelines[id]?.cursor).toBe(0);
  });
});

describe('the change summary', () => {
  const changes = (patch: string | null): DisplayChanges => ({
    session_id: 'busy',
    scope: 'worktree',
    truncated: false,
    summary: { files_changed: 3, insertions: 10, deletions: 2 } as DisplayChanges['summary'],
    files: [
      {
        path: 'src/a.ts',
        hidden: false,
        added: 8,
        removed: 2,
        binary: false,
        patch,
        too_large: false,
      },
      {
        path: null,
        hidden: true,
        added: null,
        removed: null,
        binary: false,
        patch: null,
        too_large: false,
      },
      {
        path: 'logo.png',
        hidden: false,
        added: null,
        removed: null,
        binary: true,
        patch: null,
        too_large: false,
      },
      {
        path: 'big.rs',
        hidden: false,
        added: 1,
        removed: 0,
        binary: false,
        patch: null,
        too_large: true,
      },
    ],
  });

  test('a read asks for the patches first and falls back on diffs_off', async () => {
    const paths: string[] = [];
    const off = async (path: string) => {
      paths.push(path);
      if (path.endsWith('patch=1')) throw new ApiError(403, 'off', 'diffs_off');
      return changes(null);
    };
    const got = await readChanges(off, 'st', 'se');
    expect(got.patches).toBe(false);
    expect(paths).toEqual([changesPath('st', 'se', true), changesPath('st', 'se', false)]);
    const on = await readChanges(async () => changes('diff'), 'st', 'se');
    expect(on.patches).toBe(true);
  });

  test('another failure is not hidden', async () => {
    const busy = async () => {
      throw new ApiError(429, 'busy', 'busy');
    };
    await expect(readChanges(busy, 'st', 'se')).rejects.toBeInstanceOf(ApiError);
  });

  const pane = (state: ChangesState) =>
    renderToStaticMarkup(<ChangesPane state={state} title="Changes" />);

  test('the pane lists the files, a hidden marker and a binary file', () => {
    const html = pane({ kind: 'ready', changes: changes(null), patches: false });
    expect(html).toContain('3 files');
    expect(html).toContain('src/a.ts · +8 −2');
    expect(html).toContain('A protected file (hidden)');
    expect(html).toContain('logo.png · binary');
    expect(html).toContain('too large to show');
    expect(html).toContain('Patches are off for this display.');
    expect(html).not.toMatch(/<(button|input|textarea|select|form|a)\b/);
  });

  test('with patches the pane draws the diff', () => {
    const patch = [
      'diff --git a/src/a.ts b/src/a.ts',
      '--- a/src/a.ts',
      '+++ b/src/a.ts',
      '@@ -1 +1 @@',
      '-old',
      '+new',
    ].join('\n');
    const html = pane({ kind: 'ready', changes: changes(patch), patches: true });
    expect(html).toContain('fd-wall-scroll');
    expect(html).not.toContain('Patches are off');
    expect(joinPatches(changes(patch))).toBe(patch);
  });

  test('loading and error states', () => {
    expect(pane({ kind: 'loading' })).toContain('Loading the changes…');
    expect(pane({ kind: 'error', text: 'The station is not connected.' })).toContain(
      'The station is not connected.',
    );
  });
});

describe('the artifact pane', () => {
  const answer = (type: string, status = 200, body: BodyInit = '') =>
    new Response(body, { status, headers: { 'content-type': type } });

  test('HTML gets a frame, an image gets an img, other kinds get the card', async () => {
    let stopped = 0;
    const stop = () => {
      stopped += 1;
    };
    expect(await successState(answer('text/html; charset=utf-8'), '/u', stop)).toEqual({
      kind: 'frame',
      url: '/u',
    });
    expect(await successState(answer('image/svg+xml'), '/u', stop)).toEqual({
      kind: 'image',
      url: '/u',
    });
    expect(await successState(answer('image/png'), '/u', stop)).toEqual({
      kind: 'image',
      url: '/u',
    });
    expect(await successState(answer('application/pdf'), '/u', stop)).toEqual({ kind: 'card' });
    expect(stopped).toBe(4);
  });

  test('Markdown is read as text', async () => {
    const state = await successState(
      answer('text/markdown; charset=utf-8', 200, '# Title'),
      '/u',
      () => {},
    );
    expect(state).toEqual({ kind: 'markdown', text: '# Title' });
  });

  test('the refusals map to the card or a note', async () => {
    const json = (status: number, error: string) =>
      answer('application/json', status, JSON.stringify({ error, message: 'x' }));
    expect(await failureState(json(409, 'not_displayable'))).toEqual({ kind: 'card' });
    expect(await failureState(json(404, 'unknown_artifact'))).toEqual({
      kind: 'note',
      text: 'This artifact is not available.',
    });
    expect(await failureState(json(409, 'station_unreachable'))).toEqual({
      kind: 'note',
      text: 'The station is not connected.',
    });
    expect(await failureState(json(401, 'login_required'))).toEqual({
      kind: 'note',
      text: 'The display login ended.',
    });
    expect(await failureState(json(504, 'timeout'))).toEqual({
      kind: 'note',
      text: 'The artifact did not load.',
    });
  });

  test('Markdown renders as text with no link, image, input or HTML', () => {
    const text = [
      '# Title',
      '[a link](https://evil.example/x) and ![pic](/api/displays/self/artifact/x)',
      '- [x] done',
      '<script>alert(1)</script><b>raw</b>',
      '```',
      'code',
      '```',
    ].join('\n\n');
    const html = renderToStaticMarkup(<DisplayMarkdown text={text} />);
    expect(html).toContain('<h1>Title</h1>');
    expect(html).toContain('a link');
    expect(html).toContain('pic');
    expect(html).toContain('[x] ');
    expect(html).not.toMatch(/<(a|img|input|button|script|b)\b/);
    expect(html).not.toContain('evil.example');
  });

  test('the first render waits and has no control', () => {
    const env: DisplayEnv = { get: async () => ({}) };
    const html = renderToStaticMarkup(
      <DisplayArtifactPane station="st" id="ar" version={2} env={env} />,
    );
    expect(html).toContain('Loading…');
    expect(html).not.toMatch(/<(button|input|textarea|select|form|a)\b/);
    expect(CARD_TEXT).toBe('Open this artifact on your own device');
  });
});

describe('the wall for a display login', () => {
  const sessions: DeckSession[] = [
    card('busy', 'working', 'station-a', {
      title: 'Busy one',
      batch: { batch_id: 'B1', index: 1, size: 2 },
    }),
    card('twin', 'working', 'station-a', { batch: { batch_id: 'B1', index: 2, size: 2 } }),
  ];
  const timelines = {
    [JSON.stringify(['station-a', 'busy'])]: {
      incarnation: 'i',
      cursor: 8,
      events: [],
      display: events,
      freshness: 'live' as const,
      error: null,
      ready: true,
    },
  };
  const env: DisplayEnv = { get: async () => ({}), generation: 1 };
  const wall = (route: Partial<WallRoute>, owner = false) =>
    renderToStaticMarkup(
      <WallPage
        route={{ ...wallDefaults, ...route }}
        navigate={() => {}}
        owner={owner}
        display={env}
        sessions={sessions}
        stations={[
          station('station-a', {
            display_resources: {
              at: 1,
              disks: [{ total_bytes: 100e9, free_bytes: 40e9 }],
              memory: { total_bytes: 16e9, available_bytes: 8e9 },
              cpu: { cores: 8, load1_x100: 100, load5_x100: 250 },
              sessions: [],
            },
          }),
        ]}
        freshness="live"
        timelines={timelines}
        initialSize={{ w: 3440, h: 1440 }}
        now={1_000_000}
        origin="https://deck.example"
      />,
    );
  const here = { station: 'station-a', id: 'busy' };
  const modes: Partial<WallRoute>[] = [
    { mode: 'mission' },
    { mode: 'focus', session: here },
    { mode: 'review', session: here },
    { mode: 'race', batch: 'B1' },
    { mode: 'artifact', artifact: { station: 'station-a', id: 'art', version: 2 } },
    { mode: 'ambient' },
  ];

  for (const route of modes) {
    test(`mode ${route.mode} renders and has no write control`, () => {
      const html = wall(route);
      expect(html).not.toMatch(/<(button|textarea|input|select|form)\b/);
      expect(html).not.toContain('<a ');
      expect(html).toContain(`data-mode="${route.mode}"`);
    });
  }

  test('a display login is not denied, and another login still is', () => {
    expect(wall({})).toContain('data-area="tiles"');
    expect(
      renderToStaticMarkup(
        <WallPage
          route={wallDefaults}
          navigate={() => {}}
          owner={false}
          sessions={sessions}
          stations={[]}
          freshness="live"
        />,
      ),
    ).toContain('The wall needs the owner login.');
  });

  test('the tile and the focus chat read the display events', () => {
    expect(wall({})).toContain('Bash (failed)');
    const focus = wall({ mode: 'focus', session: here });
    expect(focus).toContain('Fix the login bug');
    expect(focus).toContain('Waiting for an answer · Bash');
    expect(focus).toContain('Loading the changes…');
    expect(focus).not.toContain('Since the session started from');
  });

  test('the review mode is the change summary with no comment', () => {
    const html = wall({ mode: 'review', session: here });
    expect(html).toContain('aria-label="Review"');
    expect(html).toContain('Loading the changes…');
    expect(html).not.toContain('comment');
  });

  test('the race columns read the display routes', () => {
    const html = wall({ mode: 'race', batch: 'B1' });
    expect(html).toContain('1 of 2');
    expect(html).toContain('Loading the changes…');
    expect(html).toContain('I will read the file.');
  });

  test('an artifact without a version is not available', () => {
    const html = wall({ mode: 'artifact', artifact: { station: 'station-a', id: 'art' } });
    expect(html).toContain('This artifact is not available.');
  });

  test('the station row shows the CPU share, the memory and the fullest disk in short figures', () => {
    const html = wall({});
    expect(html).toContain('CPU 31% · Mem 8/16 GB · Disk 40 GB free');
  });

  test('the side area reads the same resources', () => {
    const html = renderToStaticMarkup(
      <SideArea
        sessions={sessions}
        stations={[
          station('station-a', {
            display_resources: {
              at: 1,
              disks: [],
              memory: null,
              cpu: { cores: 4, load1_x100: 1, load5_x100: 50 },
              sessions: [],
            },
          }),
        ]}
        now={1}
      />,
    );
    expect(html).toContain('CPU 13%');
  });
});

describe('a video artifact on the wall', () => {
  test('it is the card and its bytes are never read', async () => {
    let pulls = 0;
    let stopped = 0;
    const body = new ReadableStream<Uint8Array>(
      {
        pull() {
          pulls += 1;
        },
      },
      { highWaterMark: 0 },
    );
    const res = new Response(body, { status: 200, headers: { 'content-type': 'video/mp4' } });
    const state = await successState(res, '/u', () => {
      stopped += 1;
    });
    expect(state).toEqual({ kind: 'card' });
    expect(stopped).toBe(1);
    expect(pulls).toBe(0);
    expect(CARD_TEXT).toBe('Open this artifact on your own device');
  });

  test('the deck refusal of a video is the card too', async () => {
    const refusal = new Response(JSON.stringify({ error: 'not_displayable', message: 'x' }), {
      status: 409,
      headers: { 'content-type': 'application/json' },
    });
    expect(await failureState(refusal)).toEqual({ kind: 'card' });
  });
});
