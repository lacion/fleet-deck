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
} from './sessions';

const ids = (cards: readonly { summary: { session_id: string } }[]) =>
  cards.map((c) => c.summary.session_id);

describe('relativeAge', () => {
  test('now, minutes, hours and days', () => {
    const now = 10 * 86_400_000;
    expect(relativeAge(now - 5_000, now)).toBe('now');
    expect(relativeAge(now - 59_000, now)).toBe('now');
    expect(relativeAge(now - 60_000, now)).toBe('1m');
    expect(relativeAge(now - 5 * 60_000, now)).toBe('5m');
    expect(relativeAge(now - 3 * 3_600_000, now)).toBe('3h');
    expect(relativeAge(now - 2 * 86_400_000, now)).toBe('2d');
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

  test('station groups are sorted by name', () => {
    const model = sidebarModel(sessions, stations, '');
    expect(model.groups.map((g) => g.name)).toEqual(['Alpha', 'Beta']);
  });

  test('stations without sessions show only when there is no query', () => {
    const only = [card('w1', 'working', 'station-a')];
    expect(sidebarModel(only, stations, '').groups).toHaveLength(2);
    expect(sidebarModel(only, stations, 'w1').groups).toHaveLength(1);
  });

  test('the j/k order is the live sessions of the groups, each once', () => {
    expect(ids(sidebarOrder(sidebarModel(sessions, stations, '')))).toEqual(['n1', 'w1', 'i1']);
  });

  test('a station that is not in the list gets a group with its id', () => {
    const model = sidebarModel([card('x', 'working', 'station-z')], [], '');
    expect(model.groups.map((g) => g.id)).toEqual(['station-z']);
    expect(model.groups[0]?.reachable).toBe(false);
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
