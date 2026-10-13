// R6.2 (DL-169): the watch page model and a server render of the wall.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, Event, OpenRequest } from '../gen';
import { sessionActions } from '../session/commands';
import { claudeCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { envelopes } from '../testing/timeline';
import {
  emptyWatchFilter,
  filterChoices,
  focusOrder,
  LINE_CHARS,
  nextFocus,
  TILE_LINES,
  tileLines,
  watchSessions,
} from './tiles';
import { WatchPage } from './WatchPage';

const refs = {};
const started = (id: string, title: string, input: string): Event => ({
  kind: 'tool.started',
  node_id: id,
  parent_node_id: null,
  tool_kind: 'shell',
  title,
  input_summary: input,
  files: [],
  provider_refs: refs,
});
const said = (text: string): Event => ({
  kind: 'message.completed',
  turn_id: 't1',
  parent_node_id: null,
  text,
});

const request = (at: number, extra: Partial<OpenRequest> = {}): OpenRequest => ({
  request_id: `r${at}`,
  node_id: null,
  request_kind: 'tool',
  title: 'Run tests',
  details: 'cargo test',
  input: null,
  options: [
    { option_id: 'allow', label: 'Allow' },
    { option_id: 'deny', label: 'Deny' },
  ],
  response: 'rpc_reply',
  requested_at: at,
  ...extra,
});

const waiting = (id: string, at: number, extra: Partial<DeckSession['summary']> = {}) =>
  card(id, 'needs_you', 'station-a', {
    title: `Session ${id}`,
    capabilities: claudeCaps(),
    open_requests: [request(at)],
    ...extra,
  });

describe('tile lines', () => {
  test('the last four lines of agent text and tool calls, oldest first, one line each', () => {
    const lines = tileLines(
      envelopes('s1', [
        said('first'),
        started('n1', 'Bash', 'cargo build'),
        said('second line one\nsecond line two'),
        started('n2', 'Read', 'src/main.rs'),
        said('third'),
        started('n3', 'Bash', 'cargo test'),
      ]),
    );
    expect(lines).toHaveLength(TILE_LINES);
    expect(lines.map((l) => l.text)).toEqual([
      'second line one',
      'Read: src/main.rs',
      'third',
      'Bash: cargo test',
    ]);
    expect(lines.map((l) => l.kind)).toEqual(['message', 'tool', 'message', 'tool']);
  });

  test('a long line is cut and a failed tool says so', () => {
    const lines = tileLines(
      envelopes('s1', [
        said('x'.repeat(500)),
        started('n1', 'Bash', 'false'),
        { kind: 'tool.completed', node_id: 'n1', status: 'failed', files: [], exit_code: 1 },
      ]),
    );
    expect(lines[0]?.text).toHaveLength(LINE_CHARS);
    expect(lines[0]?.text.endsWith('…')).toBe(true);
    expect(lines[1]?.text).toBe('Bash: false (failed)');
  });

  test('chat lines are plain text: no emphasis, code ticks or heading marks (DL-171)', () => {
    const lines = tileLines(
      envelopes('s1', [
        said('**Finished:** Nothing yet.'),
        said('## Plan'),
        said('Current model: `Opus 5.5` (effort: high)'),
        started('n1', 'Bash', 'echo `date`'),
      ]),
    );
    expect(lines.map((l) => l.text)).toEqual([
      'Finished: Nothing yet.',
      'Plan',
      'Current model: Opus 5.5 (effort: high)',
      'Bash: echo `date`',
    ]);
  });

  test('no events, no lines', () => {
    expect(tileLines([])).toEqual([]);
  });
});

describe('the wall and Focus', () => {
  const sessions = [
    card('idle-a', 'idle', 'station-a', { updated_at: 5 }),
    waiting('late', 300),
    waiting('early', 100, { repo: { root: '/w/app', origin: 'github.com/acme/web.git' } }),
    card('done', 'ended'),
    card('asleep', 'ended', 'station-a', { state: 'exited', hibernated: true }),
    card('on-b', 'working', 'station-b', { updated_at: 9 }),
  ];

  test('only live sessions show, the ones that need you first', () => {
    const shown = watchSessions(sessions, emptyWatchFilter).map((c) => c.summary.session_id);
    expect(shown.slice(0, 2).sort()).toEqual(['early', 'late']);
    expect(shown).toContain('on-b');
    expect(shown).not.toContain('done');
    expect(shown).not.toContain('asleep');
  });

  test('the filters narrow by station, repository and needs-you', () => {
    const ids = (f: Partial<typeof emptyWatchFilter>) =>
      watchSessions(sessions, { ...emptyWatchFilter, ...f })
        .map((c) => c.summary.session_id)
        .sort();
    expect(ids({ station: 'station-b' })).toEqual(['on-b']);
    expect(ids({ repo: 'acme/web' })).toEqual(['early']);
    expect(ids({ needsOnly: true })).toEqual(['early', 'late']);
    expect(filterChoices(sessions).stations).toEqual(['station-a', 'station-b']);
  });

  test('Focus goes through the sessions that need you, oldest request first, and stays on one', () => {
    const order = focusOrder(sessions, emptyWatchFilter);
    expect(order).toEqual(['station-a/early', 'station-a/late']);
    expect(nextFocus(order, null)).toBe('station-a/early');
    expect(nextFocus(order, 'station-a/early')).toBe('station-a/late');
    expect(nextFocus(order, 'station-a/late')).toBe('station-a/early');
    expect(nextFocus(['station-a/late'], 'station-a/late')).toBe('station-a/late');
    expect(nextFocus(order, 'station-a/gone')).toBe('station-a/early');
    expect(nextFocus([], 'station-a/late')).toBeNull();
  });
});

describe('the watch page', () => {
  const page = (
    sessions: DeckSession[],
    props: Partial<Parameters<typeof WatchPage>[0]> = {},
    scopes: string[] = ['read', 'drive', 'approve'],
  ) =>
    renderToStaticMarkup(
      <WatchPage
        sessions={sessions}
        stations={[station()]}
        freshness="live"
        timelines={{
          [JSON.stringify(['station-a', 'busy'])]: {
            incarnation: 'i',
            cursor: 3,
            events: envelopes('busy', [
              said('Fixed the test'),
              started('n1', 'Bash', 'cargo test'),
            ]),
            freshness: 'live',
            error: null,
            ready: true,
          },
        }}
        sessionActions={(key) => sessionActions(async () => ({}), key, scopes)}
        {...props}
      />,
    );

  test('a tile shows the title, the chat lines, the current tool and the focus line', () => {
    const html = page([
      card('busy', 'working', 'station-a', {
        title: 'Fix login',
        focus: 'repairing the login test',
        running_tools: [
          {
            node_id: 'n1',
            tool_kind: 'shell',
            title: 'Bash',
            input_summary: 'cargo test',
          },
        ],
      }),
    ]);
    expect(html).toContain('Fix login');
    expect(html).toContain('<span class="fd-wtile-line" data-kind="message">Fixed the test</span>');
    expect(html).toContain('data-kind="tool">Bash: cargo test</span>');
    expect(html).toContain(
      '<span class="fd-wtile-tool" title="Running now">Bash: cargo test</span>',
    );
    expect(html).toContain('class="fd-focus" title="repairing the login test"');
    expect(html).toContain('href="#/s/station-a/busy"');
  });

  test('a session that needs you has the answer buttons in the tile', () => {
    const html = page([waiting('w1', 10)]);
    expect(html).toMatch(/aria-label="Allow: Run tests in Session w1"/);
    expect(html).toMatch(/aria-label="Deny: Run tests in Session w1"/);
  });

  test('without the approve scope the tile sends you to the session', () => {
    const html = page([waiting('w1', 10)], {}, ['read', 'drive']);
    expect(html).not.toContain('aria-label="Allow:');
    expect(html).toContain('Open the session to answer.');
  });

  test('a question has no buttons in the tile', () => {
    const q = card('q1', 'needs_you', 'station-a', {
      title: 'Ask',
      capabilities: claudeCaps(),
      open_requests: [
        request(5, {
          request_kind: 'question',
          input: { questions: [{ question: 'Which?', options: [{ label: 'A' }] }] },
        }),
      ],
    });
    const html = page([q]);
    expect(html).not.toContain('aria-label="Allow:');
  });

  test('Focus shows only the focused session, or says that nothing needs you', () => {
    const sessions = [waiting('a', 100), waiting('b', 200), card('c', 'working')];
    const first = page(sessions, { initialFocus: true });
    expect(first).toContain('data-session="a"');
    expect(first).not.toContain('data-session="b"');
    expect(first).not.toContain('data-session="c"');
    const second = page(sessions, { initialFocus: true, initialFocused: 'station-a/b' });
    expect(second).toContain('data-session="b"');
    expect(second).not.toContain('data-session="a"');
    const none = page([card('c', 'working')], { initialFocus: true });
    expect(none).toContain('Nothing needs you right now');
  });

  test('the page counts the live sessions and states an empty board', () => {
    expect(page([card('a', 'working'), card('b', 'idle')])).toContain('2 live');
    expect(page([])).toContain('No sessions yet');
  });
});
