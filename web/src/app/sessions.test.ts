import { describe, expect, test } from 'bun:test';
import { card, station } from '../testing/fleet';
import {
  basename,
  matchesQuery,
  paletteMatches,
  relativeAge,
  sessionTitle,
  sidebarModel,
  sidebarOrder,
  stationOrder,
} from './sessions';

const ids = (cards: readonly { summary: { session_id: string } }[]) =>
  cards.map((c) => c.summary.session_id);

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

  test('stations without sessions show only when there is no query', () => {
    const only = [card('w1', 'working', 'station-a')];
    expect(sidebarModel(only, stations, '').groups).toHaveLength(2);
    expect(sidebarModel(only, stations, 'w1').groups).toHaveLength(1);
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
  test('needs you first, then newest, at most max', () => {
    const list = [
      card('a', 'working', 'station-a', { updated_at: 1 }),
      card('b', 'working', 'station-a', { updated_at: 3 }),
      card('c', 'needs_you', 'station-a', { updated_at: 0 }),
    ];
    expect(ids(paletteMatches(list, ''))).toEqual(['c', 'b', 'a']);
    expect(ids(paletteMatches(list, '', 2))).toEqual(['c', 'b']);
    expect(ids(paletteMatches(list, 'a'))).toEqual(['a']);
  });
});
