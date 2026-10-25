// R6.5 (DL-169): the channels and the page of the activity stream.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { FleetActions } from '../fleet/fleet';
import type { ActivityItem } from '../gen';
import type { ViewsActions } from '../runtime/views';
import { NO_REPO } from '../board/fleet';
import { card, station } from '../testing/fleet';
import {
  CircleStop,
  FileText,
  MessageSquare,
  Moon,
  Pencil,
  Play,
  Plug,
  Shield,
  SquareTerminal,
  Wrench,
} from '../ui/icons';
import { ActivityPage, rowIcon } from './ActivityPage';
import { ALL_CHANNEL, channelList, channelQuery, inChannel, mailTargets } from './channels';

const NOW = 1_760_000_000_000;
const web = { root: '/w/web', origin: 'github.com/acme/web' };
const docs = { root: '/w/docs', origin: 'github.com/acme/docs.git' };

const sessions = [
  card('a', 'working', 'station-a', { title: 'Login', repo: web, updated_at: 30 }),
  card('b', 'idle', 'station-a', { title: 'Cache', repo: web, updated_at: 20 }),
  card('c', 'ended', 'station-a', { title: 'Old', repo: web, updated_at: 5 }),
  card('d', 'idle', 'station-b', { title: 'Docs', repo: docs, updated_at: 10 }),
];

const item = (over: Partial<ActivityItem>): ActivityItem => ({
  station_id: 'station-a',
  session_id: 'a',
  session_title: 'Login',
  repo: 'github.com/acme/web',
  kind: 'message',
  text: 'Fixed it',
  at: NOW - 120_000,
  station_seq: 1,
  ...over,
});

describe('channels', () => {
  test('the list has a repository channel with its session count and a channel per session', () => {
    const list = channelList(sessions);
    expect(list.repos.map((r) => [r.label, r.count])).toEqual([
      ['acme/docs', 1],
      ['acme/web', 3],
    ]);
    expect(list.live.map((s) => s.label)).toEqual(['Login', 'Cache', 'Docs']);
    expect(list.ended.map((s) => s.label)).toEqual(['Old']);
    expect(list.live.map((s) => s.status)).toEqual(['working', 'idle', 'idle']);
  });

  test('the sessions outside a repository come after every repository', () => {
    const list = channelList([
      card('n', 'idle', 'station-a', { title: 'Loose' }),
      ...sessions,
      card('z', 'idle', 'station-a', { repo: { root: '/w/zeta', origin: null } }),
    ]);
    expect(list.repos.map((r) => r.label)).toEqual(['acme/docs', 'acme/web', 'zeta', NO_REPO]);
  });

  test('an untitled session is its callsign; a namesake is told apart by station or age', () => {
    const list = channelList([
      card('u', 'idle', 'station-a', { callsign: 'falcon-2d9s', updated_at: 9 }),
      card('g1', 'idle', 'station-a', { title: 'Greeting', updated_at: 8 }),
      card('g2', 'idle', 'station-a', { title: 'Greeting', updated_at: 7 }),
      card('f1', 'idle', 'station-a', { title: 'Fleet check', updated_at: 6 }),
      card('f2', 'idle', 'station-b', { title: 'Fleet check', updated_at: 5 }),
    ]);
    expect(list.live.map((e) => [e.label, e.callsign, e.namesake])).toEqual([
      ['falcon-2d9s', true, null],
      ['Greeting', false, 'age'],
      ['Greeting', false, 'age'],
      ['Fleet check', false, 'station'],
      ['Fleet check', false, 'station'],
    ]);
  });

  test('the deck filter: a session by id, a repository by origin, All by nothing', () => {
    expect(channelQuery(sessions, ALL_CHANNEL, 50)).toEqual({ limit: 50 });
    expect(
      channelQuery(sessions, { kind: 'session', station: 'station-a', session: 'a' }, 50),
    ).toEqual({
      session: 'a',
      limit: 50,
    });
    expect(channelQuery(sessions, { kind: 'repo', name: 'acme/web' }, 50)).toEqual({
      repo: 'github.com/acme/web',
      limit: 50,
    });
  });

  test('a repository whose sessions disagree on the origin asks for all and filters on the board', () => {
    const mixed = [
      card('x', 'idle', 'station-a', { repo: { root: '/a/app', origin: null } }),
      card('y', 'idle', 'station-b', { repo: { root: '/b/app', origin: null } }),
    ];
    const channel = { kind: 'repo', name: 'app' } as const;
    expect(channelQuery(mixed, channel, 10)).toEqual({ limit: 10 });
    const rows = [item({ session_id: 'x' }), item({ session_id: 'other' })];
    expect(inChannel(rows, mixed, channel).map((r) => r.session_id)).toEqual(['x']);
    expect(inChannel(rows, mixed, ALL_CHANNEL)).toHaveLength(2);
  });

  test('mail goes to one session, or to the live sessions of a repository, never to All', () => {
    expect(mailTargets(sessions, ALL_CHANNEL)).toEqual([]);
    expect(mailTargets(sessions, { kind: 'session', station: 'station-a', session: 'c' })).toEqual([
      { stationId: 'station-a', sessionId: 'c' },
    ]);
    expect(mailTargets(sessions, { kind: 'repo', name: 'acme/web' })).toEqual([
      { stationId: 'station-a', sessionId: 'a' },
      { stationId: 'station-a', sessionId: 'b' },
    ]);
  });
});

describe('the activity page', () => {
  const views: ViewsActions = {
    markRead: async () => {},
    activity: async () => ({ items: [] }),
  };
  const fleet = {
    mail: async () => ({ ok: true, text: 'Mail queued.' }),
  } as unknown as FleetActions;
  const page = (props: Partial<Parameters<typeof ActivityPage>[0]> = {}) =>
    renderToStaticMarkup(
      <ActivityPage
        sessions={sessions}
        stations={[station()]}
        live
        views={views}
        fleet={fleet}
        now={NOW}
        {...props}
      />,
    );

  test('rows come newest first as the deck sent them, with the session, the text and the age', () => {
    const html = page({
      initialItems: [
        item({ text: 'Newest', at: NOW - 60_000, station_seq: 9 }),
        item({
          kind: 'mail',
          text: 'Mail: please add a changelog',
          station_seq: null,
          session_id: 'b',
          session_title: 'Cache',
        }),
        item({ kind: 'approval', text: 'Needs you: Run tests', station_seq: 3 }),
      ],
    });
    expect(html.indexOf('Newest')).toBeLessThan(html.indexOf('Mail: please add a changelog'));
    expect(html).toContain('href="#/s/station-a/a"');
    expect(html).toContain('data-kind="mail"');
    expect(html).toContain('data-kind="approval"');
    expect(html).toContain('>1 min</time>');
    expect(html).toContain('aria-label="Channels"');
    expect(html).toContain('aria-label="Activity, newest first"');
  });

  test('a repository channel shows only its sessions and the mail form says how many get it', () => {
    const html = page({
      initialChannel: { kind: 'repo', name: 'acme/web' },
      initialItems: [
        item({ text: 'Web one' }),
        item({ session_id: 'd', station_id: 'station-b', text: 'Docs one' }),
      ],
    });
    expect(html).toContain('Web one');
    expect(html).not.toContain('Docs one');
    expect(html).toContain('Mail to 2 live in acme/web');
  });

  test('All has no mail form; no fleet actions, no form for a session either', () => {
    expect(page({ initialItems: [] })).not.toContain('fd-apost');
    const none = page({
      initialChannel: { kind: 'session', station: 'station-a', session: 'a' },
      fleet: undefined,
      initialItems: [],
    });
    expect(none).not.toContain('fd-apost');
    expect(none).toContain('Nothing happened here yet');
  });

  test('a tool or approval row of a known tool shows the tool in 600 and its target; other texts stay plain', () => {
    const html = page({
      initialItems: [
        item({ kind: 'approval', text: 'Permission: Bash cargo nextest run -p x', station_seq: 1 }),
        item({ kind: 'tool', text: 'Edit src/checkout/cart.ts', station_seq: 2 }),
        item({ kind: 'approval', text: 'Question: Which port should it use?', station_seq: 3 }),
        item({ kind: 'message', text: 'Write the docs next', station_seq: 4 }),
      ],
    });
    expect(html).toContain(
      '<span class="fd-arow-lead">Permission:</span><b class="fd-arow-tool">Bash</b><code class="fd-arow-target">cargo nextest run -p x</code>',
    );
    expect(html).toContain(
      '<b class="fd-arow-tool">Edit</b><code class="fd-arow-target" data-path=""><bdi>src/checkout/cart.ts</bdi></code>',
    );
    expect(html).toContain('<span class="fd-arow-text">Question: Which port should it use?</span>');
    expect(html).toContain('<span class="fd-arow-text">Write the docs next</span>');
  });

  test('a provider error in a message row reads as its cause, with the raw words in the title', () => {
    const html = page({
      initialItems: [
        item({
          kind: 'message',
          text: 'API Error: Request rejected (429) · All credentials are cooling down',
          station_seq: 1,
        }),
      ],
    });
    expect(html).toContain('The provider&#x27;s rate limit stopped it. Try again later.');
    expect(html).toContain(
      'title="API Error: Request rejected (429) · All credentials are cooling down"',
    );
    expect(html).not.toContain('>API Error');
  });

  test('a finished call loses its "(ok)" and its JSON; a failed call ends in its result', () => {
    const html = page({
      initialItems: [
        item({ kind: 'tool', text: 'Read: /home/ana/briefs/a.png (ok)', station_seq: 1 }),
        item({ kind: 'tool', text: 'TaskStop: {"task_id":"bv93ogg6d"} (ok)', station_seq: 2 }),
        item({ kind: 'tool', text: 'Bash: head -40 (failed)', station_seq: 3 }),
        item({ kind: 'tool', text: 'mcp__fleetdeck__publish_artifact (ok)', station_seq: 4 }),
        item({ kind: 'tool', text: 'Monitor: sleep 1700; echo tick (ok)', station_seq: 5 }),
      ],
    });
    expect(html).not.toContain('(ok)');
    expect(html).not.toContain('task_id');
    expect(html).toContain('<bdi>~/briefs/a.png</bdi>');
    expect(html).toContain(
      '<b class="fd-arow-tool">TaskStop</b><code class="fd-arow-target">bv93ogg6d</code>',
    );
    // The result column of the chat (§4.5): a hazard word at the right, not a badge in the text.
    expect(html).toContain('<span class="fd-arow-result">failed</span>');
    expect(html).not.toContain('data-tone="hazard"');
    expect(html).toContain('data-outcome="failed"');
    // The deck's own tools need no server name.
    expect(html).toContain('<b class="fd-arow-tool">Publish artifact</b>');
    // A Monitor command is mono, the same as a Bash command (principle 5).
    expect(html).toContain(
      '<b class="fd-arow-tool">Monitor</b><code class="fd-arow-target">sleep 1700; echo tick</code>',
    );
  });

  test('a message is one plain line: no code ticks, emphasis or link marks', () => {
    const html = page({
      initialItems: [
        item({
          text: 'The fix workflow (`wnzrhrv6c`) is **running**, see [the log](https://x.test/l).',
        }),
      ],
    });
    expect(html).toContain(
      '<span class="fd-arow-text">The fix workflow (wnzrhrv6c) is running, see the log.</span>',
    );
    expect(html).not.toContain('`');
    expect(html).not.toContain('**');
    expect(html).not.toContain('](');
  });

  test('the same event several times in a row is one row with its count', () => {
    const html = page({
      initialItems: [
        item({ kind: 'tool', text: 'Bash: cargo test (ok)', station_seq: 4, at: NOW - 60_000 }),
        item({ kind: 'tool', text: 'Bash: cargo test (ok)', station_seq: 3 }),
        item({ kind: 'tool', text: 'Bash: cargo test (ok)', station_seq: 2 }),
        item({ kind: 'tool', text: 'Bash: cargo build (ok)', station_seq: 1 }),
      ],
    });
    expect(html.match(/<li class="fd-arow"/g)).toHaveLength(2);
    expect(html).toContain(
      '<span class="fd-count fd-arow-repeat">3<span class="fd-vh"> times</span></span>',
    );
    // The row keeps the newest of its copies.
    expect(html).toContain('>1 min</time>');
  });

  test('every row names its session, repository and station in its title', () => {
    const html = page({
      initialItems: [
        item({ text: 'One', station_seq: 2 }),
        item({ session_id: 'n', session_title: 'Loose', repo: null, text: 'Two', station_seq: 1 }),
      ],
    });
    expect(html).toContain('title="Login · acme/web on Alpha\nOne"');
    expect(html).toContain('title="Loose on Alpha\nTwo"');
  });

  test('a run of one session names it on its first row only; every row opens the session', () => {
    const html = page({
      initialItems: [
        item({ text: 'One', station_seq: 3 }),
        item({ text: 'Two', station_seq: 2 }),
        item({ session_id: 'b', session_title: 'Cache', text: 'Three', station_seq: 1 }),
      ],
    });
    expect(html.match(/data-run="start"/g)).toHaveLength(2);
    expect(html).toMatch(
      /<a class="fd-arow-session" href="#\/s\/station-a\/a" title="[^"]*">Login<\/a><span class="fd-arow-repo"> · acme\/web<\/span>/,
    );
    expect(html).toMatch(
      /<a class="fd-arow-session" href="#\/s\/station-a\/a" title="[^"]*"><span class="fd-vh">Login<\/span><\/a>/,
    );
    // The repository follows the name in the session cell, on the first row of a run only.
    expect(html.match(/class="fd-arow-repo"/g)).toHaveLength(2);
  });

  test('a session channel has its glyph; ended sessions fold under Ended with a count', () => {
    const html = page({ initialItems: [] });
    expect(html).toContain('Live <span class="fd-count">3</span>');
    expect(html).toContain('<span class="fd-dot" data-status="working" aria-hidden="true"></span>');
    expect(html).toContain('aria-expanded="false"');
    expect(html).toContain('Ended <span class="fd-count">1</span>');
    expect(html).not.toContain('>Old<');
    const chosen = page({
      initialItems: [],
      initialChannel: { kind: 'session', station: 'station-a', session: 'c' },
    });
    expect(chosen).toContain('aria-expanded="true"');
    expect(chosen).toContain('>Old<');
  });

  test('a tool row has the icon of its tool', () => {
    const icon = (text: string) => rowIcon({ kind: 'tool', text });
    expect(icon('Read: /a (ok)')).toBe(FileText);
    expect(icon('Bash: ls (ok)')).toBe(SquareTerminal);
    expect(icon('Write: /a (ok)')).toBe(Pencil);
    expect(icon('mcp__fleetdeck__fleet_view (ok)')).toBe(Plug);
    expect(icon('TaskStop (ok)')).toBe(Wrench);
  });

  test('a question row has the message-square of the fleet strips; a permission keeps the shield', () => {
    const icon = (kind: ActivityItem['kind'], text: string) => rowIcon({ kind, text });
    expect(icon('approval', 'Question: Which port should it use?')).toBe(MessageSquare);
    expect(icon('approval', 'Needs you: AskUserQuestion')).toBe(MessageSquare);
    expect(icon('approval', 'Needs you: Answer a question')).toBe(MessageSquare);
    expect(icon('approval', 'Permission: Bash cargo test')).toBe(Shield);
    expect(icon('approval', 'Needs you: Bash')).toBe(Shield);
    expect(icon('approval', 'Needs you: AskUserQuestionnaire')).toBe(Shield);
    expect(icon('message', 'Question: is not a request')).toBe(MessageSquare);
    expect(icon('tool', 'Question: in a tool row')).toBe(Wrench);
  });

  test('the request kind of a row decides the icon; the text is the fallback without it', () => {
    const icon = (request_kind: ActivityItem['request_kind'], text: string) =>
      rowIcon({ kind: 'approval', text, ...(request_kind === undefined ? {} : { request_kind }) });
    expect(icon('question', 'Needs you: Pick a database')).toBe(MessageSquare);
    expect(icon('command', 'Question: Run this?')).toBe(Shield);
    expect(icon('tool', 'Needs you: AskUserQuestion')).toBe(Shield);
    expect(icon(undefined, 'Needs you: AskUserQuestion')).toBe(MessageSquare);
    expect(icon(undefined, 'Needs you: Pick a database')).toBe(Shield);
    expect(rowIcon({ kind: 'message', text: 'x', request_kind: 'question' })).toBe(MessageSquare);
    expect(rowIcon({ kind: 'tool', text: 'x', request_kind: 'question' })).toBe(Wrench);
  });

  test('the header has its summary sentence', () => {
    expect(page()).toContain('<p class="fd-page-desc">What the sessions did, newest first.</p>');
  });

  test('before the first answer the page says it loads, and offline it says so', () => {
    expect(page()).toContain('Loading…');
    const offline = page({ live: false });
    // An empty state of the page (§4.7), not a bare line.
    expect(offline).toContain('<p class="fd-emptystate-title">Offline</p>');
    expect(offline).toContain('The stream needs a live connection.');
  });

  test('only the open request of a session that needs you now is yellow; the rest is history', () => {
    const waiting = [
      card('a', 'needs_you', 'station-a', { title: 'Login', repo: web, updated_at: 30 }),
      ...sessions.slice(1),
    ];
    const html = page({
      sessions: waiting,
      initialItems: [
        item({
          kind: 'approval',
          text: 'Needs you: Bash cargo test',
          request_kind: 'command',
          station_seq: 5,
        }),
        item({
          kind: 'approval',
          text: 'Needs you: Bash ls',
          request_kind: 'command',
          station_seq: 4,
        }),
        item({ kind: 'approval', text: 'Approval expired', station_seq: 3 }),
        item({
          session_id: 'b',
          session_title: 'Cache',
          kind: 'approval',
          text: 'Needs you: Pick a database',
          request_kind: 'question',
          station_seq: 2,
        }),
      ],
    });
    // One open request: the newest request of the session that needs you now.
    expect(html.match(/data-open="true"/g)).toHaveLength(1);
    expect(html).toMatch(
      /<li class="fd-arow" data-kind="approval" data-run="start" data-open="true">(?:(?!<\/li>)[\s\S])*<span class="fd-arow-lead">Needs you:<\/span><b class="fd-arow-tool">Bash<\/b><code class="fd-arow-target">cargo test<\/code>/,
    );
    // An older request of the same session, an expired one and one of a session that waits for
    // nothing are history: faint, and a closed request says "Asked:".
    expect(html).toContain(
      '<span class="fd-arow-lead">Asked:</span><b class="fd-arow-tool">Bash</b><code class="fd-arow-target">ls</code>',
    );
    expect(html).toContain('<span class="fd-arow-text">Asked: Pick a database</span>');
    expect(html).toContain('<span class="fd-arow-text">Approval expired</span>');
    expect(
      page({ initialItems: [item({ kind: 'approval', text: 'Needs you: Bash ls' })] }),
    ).not.toContain('data-open');
  });

  test('a start, an end and a request always name their session', () => {
    const html = page({
      initialItems: [
        item({ text: 'One', station_seq: 4 }),
        item({ kind: 'lifecycle', text: 'Session ended', station_seq: 3 }),
        item({ kind: 'approval', text: 'Approval expired', station_seq: 2 }),
        item({ text: 'Two', station_seq: 1 }),
      ],
    });
    // One, the end and the expiry start runs; Two goes on the run of the expiry.
    expect(html.match(/data-run="start"/g)).toHaveLength(3);
    expect(html.match(/title="[^"]*">Login<\/a>/g)).toHaveLength(3);
  });

  test('a lifecycle row has the icon of its event', () => {
    const icon = (text: string) => rowIcon({ kind: 'lifecycle', text });
    expect(icon('Session started')).toBe(Play);
    expect(icon('Session ended')).toBe(CircleStop);
    expect(icon('Session ended: process_exit')).toBe(CircleStop);
    expect(icon('Hibernated after 30 min idle')).toBe(Moon);
  });

  test('a built-in tool reads in words; a finished call without a target says done', () => {
    const html = page({
      initialItems: [
        item({ kind: 'tool', text: 'AskUserQuestion: {"questions":[]} (ok)', station_seq: 3 }),
        item({ kind: 'approval', text: 'Needs you: AskUserQuestion', station_seq: 2 }),
        item({ kind: 'tool', text: 'Workflow (ok)', station_seq: 1 }),
      ],
    });
    expect(html).toContain(
      '<b class="fd-arow-tool">Question</b><span class="fd-arow-words">done</span>',
    );
    expect(html).toContain(
      '<span class="fd-arow-lead">Asked:</span><b class="fd-arow-tool">Question</b>',
    );
    expect(html).toContain(
      '<b class="fd-arow-tool">Workflow</b><span class="fd-arow-words">done</span>',
    );
    expect(html).not.toContain('>AskUserQuestion');
    expect(rowIcon({ kind: 'tool', text: 'AskUserQuestion: {} (ok)' })).toBe(MessageSquare);
    expect(rowIcon({ kind: 'tool', text: 'constructor (ok)' })).toBe(Wrench);
  });

  test('the channel headings have one notation: the label and its count pill', () => {
    expect(page({ initialItems: [] })).toContain('Repositories <span class="fd-count">2</span>');
  });
});
