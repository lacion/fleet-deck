import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { activeFilters, clearFilter, FleetHome } from '../app/FleetHome';
import type { DeckSession, OpenRequest } from '../gen';
import { card, station } from '../testing/fleet';
import { priority } from '../ui/localSet';
import { emptyFilter } from './fleet';
import { FleetBoard, STRIP_ORDER, stripCards } from './FleetBoard';

const NOW = 1_760_000_040_000;

const board = (cards: DeckSession[], extra = {}) =>
  renderToStaticMarkup(
    <FleetBoard
      sessions={cards}
      stations={[station(), station('station-b')]}
      freshness="live"
      now={NOW}
      {...extra}
    />,
  );

const ids = (cards: readonly DeckSession[]) => cards.map((c) => c.summary.session_id);

const ask: OpenRequest = {
  request_id: 'r1',
  node_id: null,
  request_kind: 'tool',
  title: 'Run it?',
  details: '',
  input: null,
  options: [],
  response: 'rpc_reply',
  requested_at: 1,
};

const home = (cards: DeckSession[], layout: 'desktop' | 'phone' = 'desktop') =>
  renderToStaticMarkup(
    <FleetHome
      sessions={cards}
      stations={[station()]}
      freshness="live"
      assign={undefined}
      fleet={undefined}
      conflicts={undefined}
      feed={[]}
      layout={layout}
      onOpen={() => {}}
      onSpawn={() => {}}
    />,
  );

describe('strip order', () => {
  test('the status rank is needs you, working, verifying, queued, idle, unreachable, lost, ended', () => {
    expect(STRIP_ORDER).toEqual([
      'needs_you',
      'working',
      'verifying',
      'queued',
      'idle',
      'unreachable',
      'lost',
      'ended',
    ]);
  });

  test('a lane sorts by status rank, then newest first', () => {
    const cards = [...STRIP_ORDER]
      .reverse()
      .map((status, i) => card(status, status, 'station-a', { updated_at: i }));
    cards.push(card('w-new', 'working', 'station-a', { updated_at: 99 }));
    expect(ids(stripCards(cards))).toEqual([
      'needs_you',
      'w-new',
      'working',
      'verifying',
      'queued',
      'idle',
      'unreachable',
      'lost',
      'ended',
    ]);
  });

  test('starred strips come first, and keep the rank and the age among themselves', () => {
    const cards = [
      card('idle-star', 'idle', 'station-a', { updated_at: 1 }),
      card('work', 'working', 'station-a', { updated_at: 5 }),
      card('ended-star', 'ended', 'station-a', { updated_at: 9 }),
      card('need', 'needs_you', 'station-a', { updated_at: 2 }),
    ];
    const starred = new Set(['station-a/idle-star', 'station-a/ended-star']);
    expect(ids(stripCards(cards, starred))).toEqual(['idle-star', 'ended-star', 'need', 'work']);
    priority.add(['station-a/idle-star']);
    const html = board(cards);
    priority.remove(['station-a/idle-star']);
    expect(html.indexOf('data-session="idle-star"')).toBeLessThan(
      html.indexOf('data-session="need"'),
    );
  });

  test('a lane with only ended or lost sessions goes after the lanes with a live one', () => {
    const html = board([
      card('a', 'ended', 'station-a'),
      card('b', 'lost', 'station-a'),
      card('c', 'idle', 'station-b'),
    ]);
    expect(html.indexOf('aria-label="Station Beta"')).toBeLessThan(
      html.indexOf('aria-label="Station Alpha"'),
    );
  });
});

describe('strip markup', () => {
  test('the state edge is the data-status of the strip', () => {
    for (const status of STRIP_ORDER) {
      const html = board([card('x', status)]);
      expect(html).toMatch(new RegExp(`<a class="fd-strip"[^>]*data-status="${status}"`));
    }
  });

  test('a strip has the callsign, the title, the model and the age cells', () => {
    const html = board([
      card('abcdefghij', 'working', 'station-a', {
        callsign: 'falcon-899q',
        title: 'Fix the test',
        model: 'claude-opus-4-1',
        updated_at: NOW - 4 * 60_000,
      }),
      card('nocall'),
    ]);
    expect(html).toMatch(/class="fd-strip-call fd-callsign"[^>]*>falcon-899q</);
    expect(html).toContain('>Fix the test<');
    expect(html).toContain('class="fd-strip-model" data-family="opus">Opus 4.1<');
    expect(html).toContain('>4m<');
    // no callsign: the first 8 characters of the id
    expect(html).toMatch(/class="fd-strip-call fd-callsign"[^>]*>nocall</);
    expect(html).not.toContain('fd-dot');
  });

  test('the meta cell shows an icon only for a fact that is present', () => {
    const bare = board([card('a')]);
    expect(bare).not.toContain('fd-strip-ico');
    expect(bare).not.toContain('fd-strip-diff');
    const full = board(
      [
        card('b', 'working', 'station-a', {
          mode: 'bypassPermissions',
          artifacts_count: 3,
          background_tasks: ['t1', 't2'],
          terminal: { token: 't', cols: 80, rows: 24 } as never,
        }),
      ],
      {
        extras: new Map([
          ['station-a/b', { station_id: 'station-a', session_id: 'b', mail_pending: 2, files: [] }],
        ]),
      },
    );
    for (const kind of ['risky', 'mail', 'artifacts', 'background', 'terminal'])
      expect(full).toContain(`data-ico="${kind}"`);
    expect(full).not.toContain('data-ico="stale"');
    expect(full).not.toContain('data-ico="archived"');
    expect(full).not.toContain('data-ico="contested"');
    expect(full).toContain('title="In terminal"');
    expect(full).toContain('data-artifacts="3"');
  });

  test('the now cell shows the focus line before the status, and an ask before the focus', () => {
    const focus = { focus: 'writing the changelog' };
    const withFocus = board([card('f', 'working', 'station-a', focus)]);
    expect(withFocus).toContain('class="fd-focus" title="writing the changelog"');
    expect(withFocus).not.toContain('data-note="working"');
    const asking = board([card('g', 'working', 'station-a', { ...focus, open_requests: [ask] })]);
    expect(asking).toContain('Run it?');
    expect(asking).not.toContain('fd-focus');
    // An ended session shows its state, not the last focus of the agent.
    const ended = board([card('h', 'ended', 'station-a', focus)]);
    expect(ended).toContain('data-note="ended"');
    expect(ended).not.toContain('fd-focus');
  });

  test('the station name prefixes the title for a repo grouping only', () => {
    const cards = [card('a', 'working', 'station-a', { title: 'T' })];
    expect(board(cards)).not.toContain('fd-strip-station');
    expect(board(cards, { group: 'repo' })).toContain(
      '<span class="fd-strip-station">Alpha</span><span class="fd-strip-text">T</span>',
    );
  });
});

describe('toolbar filters', () => {
  test('only station, provider, repo and archived count as active filters', () => {
    const stations = [station(), station('station-b')];
    expect(activeFilters(emptyFilter, false, stations)).toEqual([]);
    expect(activeFilters({ ...emptyFilter, search: 'x' }, false, stations)).toEqual([]);
    const all = activeFilters(
      { station: 'station-b', provider: 'codex', repo: 'org/x', search: '' },
      true,
      stations,
    );
    expect(all.map((f) => f.text)).toEqual([
      'Station: Beta',
      'Provider: Codex',
      'Repo: org/x',
      'Archived shown',
    ]);
  });

  test('clearing one active filter keeps the others and the search', () => {
    const filter = { station: 'station-b', provider: 'codex', repo: 'org/x', search: 'find' };
    expect(clearFilter(filter, 'station')).toEqual({ ...filter, station: '' });
    expect(clearFilter(filter, 'provider')).toEqual({ ...filter, provider: '' });
    expect(clearFilter(filter, 'repo')).toEqual({ ...filter, repo: '' });
  });

  test('the toolbar has a Filters menu with the filter group inside, and the group select', () => {
    const html = home([card('a')]);
    expect(html).toContain('<details class="fd-menu fd-filters-menu">');
    expect(html).toMatch(/<summary class="fd-ghost fd-sm">Filters<\/summary>/);
    expect(html).toContain('role="group" aria-label="Fleet filters"');
    expect(html).toContain('aria-label="Group by"');
    expect(html).not.toContain('fd-active-filters');
    expect(html).not.toContain('fd-filters-count');
  });

  test('the header shows facts as separate spans, and All clear when nothing waits', () => {
    const html = home([card('a')]);
    expect(html).toContain('<span>1 session</span>');
    expect(html).toContain('<span class="fd-home-needs" data-clear="true">All clear</span>');
    expect(html).not.toContain(' · ');
    const needs = home([card('n', 'needs_you', 'station-a', { open_requests: [ask] })]);
    expect(needs).toContain('<span class="fd-home-needs">1 needs you</span>');
    // A request of an unreachable station still waits: no All clear.
    const away = home([card('u', 'unreachable', 'station-a', { open_requests: [ask] })]);
    expect(away).toContain('<span class="fd-home-needs">1 needs you</span>');
  });
});

describe('rail and panel', () => {
  test('on a wide desktop the side rail is absent with no request, and present with one', () => {
    const none = home([card('a')]);
    expect(none).not.toContain('Needs you rail');
    expect(none).toContain('data-rail="false"');
    const some = home([card('n', 'needs_you', 'station-a', { open_requests: [ask] })]);
    expect(some).toContain('aria-label="Needs you rail"');
    expect(some).toContain('data-rail="true"');
  });

  test('the activity panel is closed when nothing is stored', () => {
    const html = home([card('a')]);
    expect(html).toContain('aria-label="Fleet panel"');
    expect(html).toContain('data-open="false"');
    expect(html).toMatch(/aria-expanded="false"[^>]*aria-label="Expand panel"/);
  });
});

describe('phone board', () => {
  test('the phone renders the lanes of strips, without the group select and the old columns', () => {
    const html = home([card('a', 'working'), card('b', 'ended')], 'phone');
    expect(html).toContain('data-layout="phone"');
    expect(html).toContain('class="fd-lane"');
    expect(html).toContain('class="fd-strip"');
    expect(html).not.toContain('fd-phone-col');
    expect(html).not.toContain('aria-label="Group by"');
    expect(html).not.toContain('fd-strip-station');
    expect(html).toContain('Offline sessions');
  });
});
