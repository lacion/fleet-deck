// R6.5 (DL-169): the channels and the page of the activity stream.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { FleetActions } from '../fleet/fleet';
import type { ActivityItem } from '../gen';
import type { ViewsActions } from '../runtime/views';
import { card, station } from '../testing/fleet';
import { ActivityPage } from './ActivityPage';
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
    expect(list.sessions.map((s) => s.label)).toEqual(['Login', 'Cache', 'Docs', 'Old']);
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

  test('before the first answer the page says it loads, and offline it says so', () => {
    expect(page()).toContain('Loading…');
    expect(page({ live: false })).toContain('The stream needs a live connection.');
  });
});
