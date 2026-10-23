import { beforeEach, describe, expect, test } from 'bun:test';
import { card, station } from '../testing/fleet';
import {
  basename,
  matchesQuery,
  paletteMatches,
  relativeAge,
  resetStatusOrder,
  sessionTitle,
  sidebarModel,
  sidebarOrder,
  stationOrder,
} from './sessions';

const ids = (cards: readonly { summary: { session_id: string } }[]) =>
  cards.map((c) => c.summary.session_id);

// Every test starts as a fresh board: no status order is known yet.
beforeEach(() => resetStatusOrder());

describe('relativeAge', () => {
  test('now, minutes, hours and days', () => {
    const now = 10 * 86_400_000;
    expect(relativeAge(now - 5_000, now)).toBe('now');
    expect(relativeAge(now - 59_000, now)).toBe('now');
    expect(relativeAge(now - 60_000, now)).toBe('1 min');
    expect(relativeAge(now - 5 * 60_000, now)).toBe('5 min');
    expect(relativeAge(now - 3 * 3_600_000, now)).toBe('3 h');
    expect(relativeAge(now - 2 * 86_400_000, now)).toBe('2 d');
  });

  test('a time in the future is now', () => {
    expect(relativeAge(5_000, 1_000)).toBe('now');
  });
});

describe('titles and search', () => {
  test('the title falls back to the callsign and the id', () => {
    expect(sessionTitle(card('s1', 'working', 'station-a', { title: 'Fix login' }))).toBe(
      'Fix login',
    );
    expect(
      sessionTitle(card('s1', 'working', 'station-a', { title: null, callsign: 'Falcon' })),
    ).toBe('Falcon');
    expect(sessionTitle(card('s1', 'working', 'station-a', { title: null, callsign: null }))).toBe(
      's1',
    );
  });

  test('the query matches title, id, cwd, branch and model without case', () => {
    const c = card('s9', 'working', 'station-a', {
      title: 'Fix Login',
      cwd: '/repos/Deck',
      branch: 'feat/x',
      model: 'opus',
    });
    for (const q of ['login', 'S9', 'repos/deck', 'FEAT', 'opus', '  ', '']) {
      expect(matchesQuery(c, q)).toBe(true);
    }
    expect(matchesQuery(c, 'zzz')).toBe(false);
  });

  test('basename is the last segment', () => {
    expect(basename('/repos/deck')).toBe('deck');
    expect(basename('/repos/deck/')).toBe('deck');
    expect(basename('/')).toBe('/');
  });
});

describe('sidebar model', () => {
  const stations = [station('station-b'), station('station-a')];
  const sessions = [
    card('w1', 'working', 'station-a', { updated_at: 5 }),
    card('n1', 'needs_you', 'station-a', { updated_at: 1 }),
    card('e1', 'ended', 'station-a', { updated_at: 9 }),
    card('i1', 'idle', 'station-b', { updated_at: 3 }),
  ];

  test('needs-you sessions form the top group and stay in their station', () => {
    const model = sidebarModel(sessions, stations, '');
    expect(ids(model.needs)).toEqual(['n1']);
    const alpha = model.groups.find((g) => g.id === 'station-a');
    expect(ids(alpha?.live ?? [])).toEqual(['n1', 'w1']);
    expect(ids(alpha?.ended ?? [])).toEqual(['e1']);
    expect(alpha?.count).toBe(3);
  });

  test('station groups follow the station list, the lane order of the fleet board', () => {
    const model = sidebarModel(sessions, stations, '');
    expect(model.groups.map((g) => g.name)).toEqual(['Beta', 'Alpha']);
  });

  test('the station order: the station list first, then the other ids by id', () => {
    expect(stationOrder(['zeta', 'station-a', 'beta', 'station-b', 'zeta'], stations)).toEqual([
      'station-b',
      'station-a',
      'beta',
      'zeta',
    ]);
  });

  test('a station without a session has no group (the Stations page lists it)', () => {
    const only = [card('w1', 'working', 'station-a')];
    expect(sidebarModel(only, stations, '').groups.map((g) => g.id)).toEqual(['station-a']);
    expect(sidebarModel(only, stations, 'w1').groups).toHaveLength(1);
    expect(sidebarModel([], stations, '').groups).toEqual([]);
  });

  test('a station whose only session needs you lists it in Needs you, with no empty group', () => {
    const model = sidebarModel([card('n1', 'needs_you', 'station-a')], stations, '');
    expect(model.groups).toEqual([]);
    expect(ids(model.needs)).toEqual(['n1']);
    expect(ids(sidebarOrder(model))).toEqual(['n1']);
  });

  test('a search whose only match on a station needs you shows no empty station group', () => {
    const only = [card('n1', 'needs_you', 'station-a', { title: 'Fix the login' })];
    const model = sidebarModel(only, stations, 'login');
    expect(model.groups).toEqual([]);
    expect(ids(model.needs)).toEqual(['n1']);
  });

  test('the j/k order is the live sessions of the groups, each once', () => {
    expect(ids(sidebarOrder(sidebarModel(sessions, stations, '')))).toEqual(['n1', 'i1', 'w1']);
  });

  test('the j/k order follows the rows: the needs-you group first, then the stations', () => {
    const two = [station('station-a'), station('station-b')];
    const list = [
      card('a-idle', 'idle', 'station-a'),
      card('b-needs', 'needs_you', 'station-b'),
      card('b-idle', 'idle', 'station-b'),
    ];
    expect(ids(sidebarOrder(sidebarModel(list, two, '')))).toEqual(['b-needs', 'a-idle', 'b-idle']);
  });

  test('a station that is not in the list gets a group with its id', () => {
    const model = sidebarModel([card('x', 'working', 'station-z')], [], '');
    expect(model.groups.map((g) => g.id)).toEqual(['station-z']);
    expect(model.groups[0]?.reachable).toBe(false);
  });
});

describe('the status order of the live rows', () => {
  const stations = [station('station-a')];
  const at = (id: string, status: 'working' | 'idle', updated_at: number) =>
    card(id, status, 'station-a', { updated_at });
  const rows = (list: ReturnType<typeof card>[]) =>
    ids(sidebarModel(list, stations, '').groups[0]?.live ?? []);

  test('a fresh board lists each status the last activity first', () => {
    expect(rows([at('w1', 'working', 1), at('w2', 'working', 5), at('i1', 'idle', 9)])).toEqual([
      'w2',
      'w1',
      'i1',
    ]);
  });

  test('an event keeps every row in place; a new status puts the row after that status', () => {
    expect(rows([at('a', 'working', 3), at('b', 'working', 2), at('c', 'idle', 9)])).toEqual([
      'a',
      'b',
      'c',
    ]);
    // b has a newer event in the same status: it stays under a (it went first before).
    expect(rows([at('a', 'working', 3), at('b', 'working', 8), at('c', 'idle', 9)])).toEqual([
      'a',
      'b',
      'c',
    ]);
    // c starts to work: after a and b, which were working before it.
    expect(rows([at('a', 'working', 3), at('b', 'working', 8), at('c', 'working', 10)])).toEqual([
      'a',
      'b',
      'c',
    ]);
    // a waits, then works again: each time after the rows already in its status.
    expect(rows([at('a', 'idle', 11), at('b', 'working', 8), at('c', 'working', 10)])).toEqual([
      'b',
      'c',
      'a',
    ]);
    expect(rows([at('a', 'working', 12), at('b', 'working', 13), at('c', 'working', 14)])).toEqual([
      'b',
      'c',
      'a',
    ]);
  });

  test('the j/k keys follow the rows; a session hidden by the query keeps its place', () => {
    const list = [at('a', 'working', 1), at('b', 'working', 2)];
    expect(ids(sidebarOrder(sidebarModel(list, stations, 'a')))).toEqual(['a']);
    const later = [at('a', 'working', 9), at('b', 'working', 2)];
    expect(ids(sidebarOrder(sidebarModel(later, stations, '')))).toEqual(['b', 'a']);
  });
});

describe('sidebar stars and archive (DL-141)', () => {
  const stations = [station('station-a')];
  const sessions = [
    card('w1', 'working', 'station-a', { updated_at: 5 }),
    card('w2', 'working', 'station-a', { updated_at: 1 }),
    card('e1', 'ended', 'station-a', { updated_at: 9 }),
    card('e2', 'ended', 'station-a', { updated_at: 2 }),
    card('l1', 'lost', 'station-a', { updated_at: 3 }),
    card('u1', 'unreachable', 'station-a', { updated_at: 4 }),
  ];
  const none = new Set<string>();

  test('starred sessions sort first in their group, then by the usual order', () => {
    const model = sidebarModel(sessions, stations, '', {
      starred: new Set(['station-a/w2', 'station-a/e2']),
      archived: none,
    });
    const alpha = model.groups[0];
    expect(ids(alpha?.live ?? [])).toEqual(['w2', 'w1']);
    expect(ids(alpha?.ended ?? [])).toEqual(['e2', 'e1', 'u1', 'l1']);
    expect(ids(sidebarOrder(model))).toEqual(['w2', 'w1']);
  });

  test('a starred needs-you session leads the needs list', () => {
    const list = [
      card('n1', 'needs_you', 'station-a', { updated_at: 9 }),
      card('n2', 'needs_you', 'station-a', { updated_at: 1 }),
    ];
    const model = sidebarModel(list, stations, '', {
      starred: new Set(['station-a/n2']),
      archived: none,
    });
    expect(ids(model.needs)).toEqual(['n2', 'n1']);
  });

  test('archived ended and lost sessions leave the groups and the counts', () => {
    const model = sidebarModel(sessions, stations, '', {
      starred: none,
      archived: new Set(['station-a/e1', 'station-a/l1', 'station-a/u1']),
    });
    const alpha = model.groups[0];
    expect(ids(alpha?.ended ?? [])).toEqual(['u1', 'e2']);
    expect(alpha?.count).toBe(4);
    expect(ids(model.archived)).toEqual(['e1', 'l1']);
  });

  test('an archived session shows again when its status is not ended or lost', () => {
    const model = sidebarModel([card('w1', 'working', 'station-a')], stations, '', {
      starred: none,
      archived: new Set(['station-a/w1']),
    });
    expect(ids(model.groups[0]?.live ?? [])).toEqual(['w1']);
    expect(model.archived).toHaveLength(0);
  });

  test('the query filters the archived list too', () => {
    const model = sidebarModel(sessions, stations, 'e1', {
      starred: none,
      archived: new Set(['station-a/e1', 'station-a/l1']),
    });
    expect(ids(model.archived)).toEqual(['e1']);
  });
});

describe('palette matches', () => {
  const both = (g: ReturnType<typeof paletteMatches>) => [...ids(g.live), '|', ...ids(g.ended)];

  test('needs you, then running, then idle, then ended; the newest first in each; at most max', () => {
    const list = [
      card('e-new', 'ended', 'station-a', { updated_at: 9 }),
      card('i-old', 'idle', 'station-a', { updated_at: 1 }),
      card('w', 'working', 'station-a', { updated_at: 2 }),
      card('q', 'queued', 'station-a', { updated_at: 3 }),
      card('n', 'needs_you', 'station-a', { updated_at: 0 }),
      card('i-new', 'idle', 'station-a', { updated_at: 4 }),
      card('l', 'lost', 'station-a', { updated_at: 5 }),
    ];
    expect(both(paletteMatches(list, ''))).toEqual([
      'n',
      'q',
      'w',
      'i-new',
      'i-old',
      '|',
      'e-new',
      'l',
    ]);
    // The cap cuts the ended group first: a live session never hides behind an ended one.
    expect(both(paletteMatches(list, '', undefined, 3))).toEqual(['n', 'q', 'w', '|']);
    expect(both(paletteMatches(list, 'e-new'))).toEqual(['|', 'e-new']);
  });

  test('a hibernated session lists with the idle ones', () => {
    const list = [
      card('h', 'ended', 'station-a', { updated_at: 9, state: 'exited', hibernated: true }),
      card('i', 'idle', 'station-a', { updated_at: 1 }),
    ];
    expect(both(paletteMatches(list, ''))).toEqual(['h', 'i', '|']);
  });

  test('an archived session shows only for a query that matches it, after the other ended', () => {
    const list = [
      card('old-job', 'ended', 'station-a', { updated_at: 9 }),
      card('old-doc', 'ended', 'station-a', { updated_at: 1 }),
      card('w', 'working', 'station-a', { updated_at: 2 }),
    ];
    const cleared = new Set(['station-a/old-job']);
    expect(both(paletteMatches(list, '', cleared))).toEqual(['w', '|', 'old-doc']);
    expect(both(paletteMatches(list, 'old', cleared))).toEqual(['|', 'old-doc', 'old-job']);
  });

  test('the needs-you sessions list the longest wait first, as the Needs you panel', () => {
    const asks = (at: number) => [
      {
        request_id: `r${at}`,
        node_id: null,
        request_kind: 'tool' as const,
        title: 'Run it?',
        details: '',
        input: null,
        options: [],
        response: 'rpc_reply' as const,
        requested_at: at,
      },
    ];
    // "new" asked 2 min ago and updated last; "old" asked 6 min ago.
    const list = [
      card('new', 'needs_you', 'station-a', { updated_at: 9, open_requests: asks(8) }),
      card('old', 'needs_you', 'station-a', { updated_at: 5, open_requests: asks(4) }),
      card('w', 'working', 'station-a', { updated_at: 10 }),
    ];
    expect(ids(paletteMatches(list, '').live)).toEqual(['old', 'new', 'w']);
    expect(ids(sidebarModel(list, [station()], '').needs)).toEqual(['old', 'new']);
    // Starred stays the first key.
    const starred = sidebarModel(list, [station()], '', {
      starred: new Set(['station-a/new']),
      archived: new Set(),
    });
    expect(ids(starred.needs)).toEqual(['new', 'old']);
  });
});
