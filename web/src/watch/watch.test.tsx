// R6.2 (DL-169): the watch page model and a server render of the wall.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { NO_REPO } from '../board/fleet';
import type { DeckSession, Event, OpenRequest, ToolKind } from '../gen';
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
  watchColumns,
  watchSessions,
  withoutCall,
} from './tiles';
import { Tile, WatchPage } from './WatchPage';

const refs = {};
const started = (id: string, title: string, input: string, tool = 'shell' as ToolKind): Event => ({
  kind: 'tool.started',
  node_id: id,
  parent_node_id: null,
  tool_kind: tool,
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
    // A tool call keeps its parts: the name (sans) and the target (mono).
    expect(lines[3]).toMatchObject({ name: 'Bash', target: 'cargo test', note: '' });
    expect(lines[0]?.name).toBeUndefined();
  });

  test('a provider error reads as its cause, with the provider words in the title', () => {
    const lines = tileLines(
      envelopes('s1', [
        said('API Error: Request rejected (429) · All credentials are cooling down'),
        said('Plain text'),
      ]),
    );
    expect(lines.map((l) => l.text)).toEqual([
      "The provider's rate limit stopped it. Try again later.",
      'Plain text',
    ]);
    expect(lines[0]?.text).not.toContain('API Error');
    expect(lines[0]?.label).toBe(
      'API Error: Request rejected (429) · All credentials are cooling down',
    );
    expect(lines[1]?.label).toBeUndefined();
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
    expect(lines[1]).toMatchObject({ name: 'Bash', target: 'false', outcome: 'failed' });
  });

  test("a tool line has the owner's words: no MCP id, no JSON, a path keeps its file name", () => {
    const lines = tileLines(
      envelopes('s1', [
        started('n1', 'mcp__fleetdeck__publish_artifact', '/Users/ana/report.html'),
        started('n2', 'Workflow', '{"script":"export const meta = {\\n name'),
        started('n3', 'Agent', '{"description":"Review the diff","prompt":"Read it all"}'),
        started('n4', 'Read', '/home/ana/briefs/a.png', 'read'),
      ]),
    );
    expect(lines.map((l) => [l.name, l.target, l.form])).toEqual([
      ['Publish artifact', '/Users/ana/report.html', 'path'],
      ['Workflow', '', 'words'],
      ['Agent', 'Review the diff', 'words'],
      ['Read', '/home/ana/briefs/a.png', 'path'],
    ]);
    // A call without a target says how it stands: never a lone name.
    expect(lines.map((l) => l.note)).toEqual(['', 'running', '', '']);
    expect(lines[1]?.text).toBe('Workflow running');
  });

  test('an MCP tool has no server on the line; its title keeps the whole name', () => {
    const [line] = tileLines(
      envelopes('s1', [started('n1', 'mcp__chrome-devtools__take_screenshot', '{}', 'other')]),
    );
    expect(line).toMatchObject({
      name: 'Take screenshot',
      label: 'Take screenshot · chrome-devtools',
    });
  });

  test('the same line several times in a row is one line with its count', () => {
    const lines = tileLines(
      envelopes('s1', [
        started('n1', 'Bash', 'cargo test'),
        started('n2', 'Bash', 'cargo test'),
        started('n3', 'Bash', 'cargo test'),
        said('done'),
        said('done'),
      ]),
    );
    expect(lines.map((l) => [l.text, l.repeat])).toEqual([
      ['Bash: cargo test', 3],
      ['done', 2],
    ]);
    // The line keeps the key of its newest copy (the third call of the same timeline).
    const apart = tileLines(
      envelopes('s1', [
        started('n1', 'Bash', 'cargo a'),
        started('n2', 'Bash', 'cargo b'),
        started('n3', 'Bash', 'cargo c'),
      ]),
    );
    expect(lines[0]?.key).toBe(apart[2]?.key ?? '');
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

  test('the filters narrow by station, repository and the status of the fleet', () => {
    const ids = (f: Partial<typeof emptyWatchFilter>) =>
      watchSessions(sessions, { ...emptyWatchFilter, ...f })
        .map((c) => c.summary.session_id)
        .sort();
    expect(ids({ station: 'station-b' })).toEqual(['on-b']);
    expect(ids({ repo: 'acme/web' })).toEqual(['early']);
    expect(ids({ status: 'needs_you' })).toEqual(['early', 'late']);
    expect(ids({ status: 'working' })).toEqual(['on-b']);
    expect(ids({ status: 'idle' })).toEqual(['idle-a']);
    expect(filterChoices(sessions).stations).toEqual(['station-a', 'station-b']);
  });

  test('the wall balances its rows: no lone tile on the last row when a column less avoids it', () => {
    // A 1144px column fits two 420px tiles, a 1548px column three, a 1756px column four.
    expect(watchColumns(6, 1144)).toBe(2);
    expect(watchColumns(6, 1548)).toBe(3);
    expect(watchColumns(6, 1756)).toBe(3);
    expect(watchColumns(4, 1548)).toBe(2);
    expect(watchColumns(7, 1756)).toBe(4);
    // One row: the tiles keep at most 720px, the rest of the row stays empty.
    expect(watchColumns(1, 1548)).toBe(3);
    expect(watchColumns(2, 1548)).toBe(3);
    expect(watchColumns(1, 600)).toBe(1);
    // A phone: one column.
    expect(watchColumns(6, 358)).toBe(1);
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
    // The chat line of the call that runs now folds into its running line: it reads once.
    expect(html).not.toContain('data-kind="tool"><b class="fd-wtile-toolname">Bash</b>');
    // The call that runs now: the working glyph and its name tell it from the past calls.
    expect(html).toContain(
      '<span class="fd-wtile-tool"><span class="fd-dot" data-status="working" aria-hidden="true"></span><span class="fd-vh">Running now: </span><b class="fd-wtile-toolname">Bash</b><code>cargo test</code></span>',
    );
    expect(html).toContain('class="fd-focus" title="repairing the login test"');
    // The age of the tile, at the right of its head (the strip's age).
    expect(html).toMatch(/<time class="fd-wtile-age" dateTime="[^"]+">[^<]+<\/time>/);
    expect(html).toContain('href="#/s/station-a/busy"');
  });

  test('a past call is a chat line: the tool name in sans weight 600, the target in mono', () => {
    const html = page([
      card('busy', 'working', 'station-a', {
        running_tools: [{ node_id: 'n2', tool_kind: 'shell', title: 'Bash', input_summary: 'ls' }],
      }),
    ]);
    // DESIGN-SYSTEM §4.1, §4.5.
    expect(html).toContain(
      'data-kind="tool"><b class="fd-wtile-toolname">Bash</b><code>cargo test</code></span>',
    );
    expect(html).toContain(
      '<span class="fd-vh">Running now: </span><b class="fd-wtile-toolname">Bash</b><code>ls</code>',
    );
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

  test('a tile names the station and the repository, never "no repository", then the branch', () => {
    const html = page([
      card('busy', 'working', 'station-a', { title: 'Loose', cwd: '/w/reggio' }),
      card('b2', 'working', 'station-a', {
        title: 'In a repo',
        branch: 'v2',
        repo: { root: '/w/web', origin: 'github.com/acme/web' },
      }),
    ]);
    expect(html).not.toContain(`Alpha · ${NO_REPO}`);
    // The station is a part of its own (never cut); the repository follows it and gives way
    // first; the branch keeps its first 18 characters (`--fd-wtile-keep`).
    expect(html).toContain(
      '<span class="fd-wtile-where"><span class="fd-wtile-station">Alpha</span><span class="fd-wtile-branch"',
    );
    expect(html).toContain(
      '<span class="fd-wtile-station">Alpha</span><span class="fd-wtile-place" title="acme/web"><span aria-hidden="true">· </span>acme/web</span>',
    );
    expect(html).toContain('style="--fd-wtile-keep:2ch"');
    expect(html).toContain('style="--fd-wtile-keep:6ch"');
    expect(html).toContain('title="Folder: reggio"');
    expect(html).toContain('lucide-folder');
    expect(html).toContain('title="Branch: v2"');
    expect(html).toContain('lucide-git-branch');
  });

  test('the header filters are ghost selects, the status segments of the fleet and the Focus toggle', () => {
    const html = page([card('a', 'working')], {
      initialFilter: { station: 'station-a', repo: '', status: 'needs_you' },
      initialFocus: true,
    });
    expect(html).toMatch(
      /<select class="fd-ghost fd-sm" data-active="true"[^>]*aria-label="Station"/,
    );
    expect(html).toMatch(/<select class="fd-ghost fd-sm" aria-label="Repository"/);
    // One control for one filter: the fleet's status segments (§4.3), not a "Needs you only" toggle.
    expect(html).toContain('<div class="fd-seg fd-watch-status" role="group" aria-label="Status">');
    expect(html).toContain('<button type="button" aria-pressed="true">Needs you</button>');
    expect(html).toContain('<button type="button" aria-pressed="false">All</button>');
    expect(html).not.toContain('Needs you only');
    expect(html).not.toContain('lucide-list-filter');
    expect(html).toMatch(/<button type="button" class="fd-ghost fd-sm" aria-pressed="true"[^>]*>/);
    expect(html).toContain('lucide-circle-dot');
    expect(html).not.toContain('fd-chip');
  });

  test('the header says the fleet sentence: the live sessions, their stations and who needs you', () => {
    const calm = page([card('a', 'working')]);
    expect(calm).toContain('<span>1 live session</span>');
    expect(calm).toContain('<span class="fd-home-on"> on <span>1 station</span></span>');
    // The calm state is green, as on the fleet header.
    expect(calm).toContain('<span class="fd-home-needs" data-clear="true">All clear</span>');
    expect(page([waiting('w1', 10), card('b', 'idle')])).toContain(
      '<span class="fd-home-needs">1 needs you</span>',
    );
  });

  test('Watch passes the matching cards', () => {
    const cards = [
      card('a', 'working', 'station-a', { running_agents: 2 }),
      card('b', 'idle', 'station-a', { activity: 'idle', running_agents: 1 }),
      // not live: left out of the wall and of the total
      card('u', 'unreachable', 'station-a', { running_agents: 9 }),
      // another station: filtered out
      card('c', 'working', 'station-b', { running_agents: 4 }),
    ];
    // one working session plus the three running agents of the live cards on station-a
    const html = page(cards, { initialFilter: { ...emptyWatchFilter, station: 'station-a' } });
    expect(html).toContain('>4 at work</span>');
    expect(html).not.toContain('>9 at work');
    expect(page([card('i', 'idle', 'station-a', { activity: 'idle' })])).not.toContain('at work');
  });

  test('the page counts the live sessions and states an empty board', () => {
    expect(page([card('a', 'working'), card('b', 'idle')])).toContain('2 live');
    expect(page([])).toContain('No sessions yet');
  });
});

describe('the read-only tile (R13 P1)', () => {
  const tile = (over: Partial<Parameters<typeof Tile>[0]> = {}) =>
    renderToStaticMarkup(
      <ul>
        <Tile
          card={waiting('w1', 10)}
          stations={[station()]}
          timeline={undefined}
          actions={sessionActions(async () => ({}), { station_id: 'station-a', session_id: 'w1' }, [
            'read',
            'drive',
            'approve',
          ])}
          freshness="live"
          focused={false}
          readOnly
          {...over}
        />
      </ul>,
    );

  test('on the wall the call that runs now is the newest chat line, with the working glyph', () => {
    const html = tile({
      card: card('busy', 'working', 'station-a', {
        running_tools: [{ node_id: 'n9', tool_kind: 'shell', title: 'Bash', input_summary: 'ls' }],
      }),
      timeline: {
        incarnation: 'i',
        cursor: 2,
        events: envelopes('busy', [said('Fixed the test'), started('n9', 'Bash', 'ls')]),
        freshness: 'live',
        error: null,
        ready: true,
      },
    });
    expect(html).toContain(
      '<span class="fd-wtile-lines"><span class="fd-wtile-line" data-kind="message">Fixed the test</span><span class="fd-wtile-line" data-kind="tool" data-running=""><span class="fd-dot" data-status="working" aria-hidden="true"></span><span class="fd-vh">Running now: </span><b class="fd-wtile-toolname">Bash</b><code>ls</code></span></span>',
    );
    expect(html).not.toContain('fd-wtile-tool"');
  });

  test('it has no button and no link, and the request shows as text', () => {
    const html = tile();
    expect(html).not.toContain('<button');
    expect(html).not.toContain('<a ');
    expect(html).not.toContain('Open the session to answer.');
    expect(html).toContain('Run tests');
    expect(html).toContain('tabindex="0"');
    expect(html).toContain('data-ref="station-a/w1"');
  });

  test('the normal tile keeps its buttons and its link', () => {
    const html = tile({ readOnly: false });
    expect(html).toContain('<button');
    expect(html).toContain('href="#/s/station-a/w1"');
    expect(html).not.toContain('tabindex');
  });

  test('an unavailable tile says so and the others are not touched', () => {
    expect(tile({ unavailable: true })).toContain('Unavailable');
    expect(tile()).not.toContain('Unavailable');
  });

  test('a tile without chat shows Loading… until the timeline is ready, then No chat yet.', () => {
    const ready = {
      incarnation: 'i',
      cursor: 0,
      events: [],
      freshness: 'live' as const,
      error: null,
      ready: true,
    };
    expect(tile()).toContain('<span class="fd-wtile-empty">Loading…</span>');
    const html = tile({ timeline: ready });
    expect(html).toContain('<span class="fd-wtile-empty">No chat yet.</span>');
    expect(html).not.toContain('Loading…');
  });

  test('a running tool or a focus line replaces No chat yet.', () => {
    const ready = {
      incarnation: 'i',
      cursor: 0,
      events: [],
      freshness: 'live' as const,
      error: null,
      ready: true,
    };
    const running = tile({
      timeline: ready,
      card: waiting('w1', 10, {
        running_tools: [
          { node_id: 'n1', tool_kind: 'shell', title: 'Bash', input_summary: 'xcodebuild' },
        ],
      }),
    });
    expect(running).not.toContain('No chat yet.');
    expect(running).toContain('<code>xcodebuild</code>');
    const focused = tile({
      timeline: ready,
      card: waiting('w1', 10, { focus: 'Writing the brief' } as Partial<DeckSession['summary']>),
    });
    expect(focused).not.toContain('No chat yet.');
    expect(focused).toContain('Writing the brief');
    // Loading stays while the timeline loads, with or without a tool.
    expect(
      tile({
        card: waiting('w1', 10, {
          running_tools: [
            { node_id: 'n1', tool_kind: 'shell', title: 'Bash', input_summary: 'xcodebuild' },
          ],
        }),
      }),
    ).toContain('Loading…');
  });

  test('a request has the needs-you icon of the fleet: a shield, or a bubble for a question', () => {
    const permission = tile();
    expect(permission).toContain('lucide-shield');
    expect(permission).not.toContain('lucide-triangle-alert');
    const question = tile({
      card: waiting('w1', 10, { open_requests: [request(10, { request_kind: 'question' })] }),
    });
    expect(question).toContain('lucide-message-square');
    expect(question).not.toContain('lucide-shield');
  });

  test('the answers have the ApprovalDock look: Allow decides, Deny is a danger button', () => {
    const options = [
      { option_id: 'allow', label: 'Allow', kind: 'allow_once' as const },
      { option_id: 'session', label: 'Allow for this session', kind: 'allow_session' as const },
      { option_id: 'deny', label: 'Deny', kind: 'deny' as const },
    ];
    const html = tile({
      readOnly: false,
      card: waiting('w1', 10, { open_requests: [request(10, { options })] }),
    });
    expect(html).toMatch(/class="fd-decide fd-sm"[^>]*aria-label="Allow: /);
    expect(html).toMatch(/class="fd-sm"[^>]*aria-label="Allow for this session: /);
    expect(html).toMatch(/class="fd-danger fd-sm"[^>]*aria-label="Deny: /);
  });
});

describe('the call that runs now', () => {
  const line = (key: string, node: string, repeat?: number) => ({
    key,
    kind: 'tool' as const,
    text: 'Bash: cargo test',
    node,
    ...(repeat === undefined ? {} : { repeat }),
  });

  test('its chat line folds into it; a folded run keeps the other copies', () => {
    expect(withoutCall([line('1', 'a'), line('2', 'b')], 'b')).toEqual([line('1', 'a')]);
    expect(withoutCall([line('1', 'a', 3)], 'a')).toEqual([line('1', 'a', 2)]);
    // Another call, or the call further up the chat, stays.
    expect(withoutCall([line('1', 'a'), line('2', 'b')], 'a')).toHaveLength(2);
    expect(withoutCall([], 'a')).toEqual([]);
  });
});
