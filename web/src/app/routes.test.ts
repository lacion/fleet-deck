import { describe, expect, test } from 'bun:test';
import { formatHash, parseHash, type Route } from './routes';

const session = (
  station: string,
  id: string,
  tab: 'chat' | 'changes' | 'files' = 'chat',
): Route => ({
  name: 'session',
  station,
  session: id,
  tab,
});

describe('routes', () => {
  test('empty hashes are the home', () => {
    for (const hash of ['', '#', '#/']) expect(parseHash(hash)).toEqual({ name: 'home' });
  });

  test('the plain pages parse and format back', () => {
    for (const name of [
      'fleet',
      'new',
      'plans',
      'stations',
      'files',
      'terminals',
      'more',
    ] as const) {
      expect(parseHash(`#/${name}`)).toEqual({ name });
      expect(formatHash({ name })).toBe(`#/${name}`);
    }
    expect(formatHash({ name: 'home' })).toBe('#/');
  });

  test('a session route carries the station, the session and the tab', () => {
    expect(parseHash('#/s/station-a/s1')).toEqual(session('station-a', 's1'));
    expect(parseHash('#/s/station-a/s1?tab=changes')).toEqual(
      session('station-a', 's1', 'changes'),
    );
    expect(parseHash('#/s/station-a/s1?tab=files')).toEqual(session('station-a', 's1', 'files'));
  });

  test('the compare route carries the batch id (R1.4)', () => {
    expect(parseHash('#/compare/01JABC')).toEqual({ name: 'compare', batch: '01JABC' });
    expect(formatHash({ name: 'compare', batch: 'a b/c' })).toBe('#/compare/a%20b%2Fc');
    const route: Route = { name: 'compare', batch: 'x y' };
    expect(parseHash(formatHash(route))).toEqual(route);
    // No id, too many parts and a bad escape are the home.
    for (const hash of ['#/compare', '#/compare/', '#/compare/a/b', '#/compare/%E0%A4%A']) {
      expect(parseHash(hash)).toEqual({ name: 'home' });
    }
  });

  test('an unknown tab is the chat tab', () => {
    expect(parseHash('#/s/a/b?tab=nope')).toEqual(session('a', 'b'));
    expect(parseHash('#/s/a/b?x=1')).toEqual(session('a', 'b'));
  });

  test('round trips keep ids that need escaping', () => {
    const routes: Route[] = [
      { name: 'home' },
      { name: 'fleet' },
      session('a/b', 'c d'),
      session('é', '100%', 'files'),
      session('s?', 'x#y', 'changes'),
    ];
    for (const route of routes) expect(parseHash(formatHash(route))).toEqual(route);
    expect(formatHash(session('a/b', 'c d'))).toBe('#/s/a%2Fb/c%20d');
  });

  test('the chat tab has no query', () => {
    expect(formatHash(session('a', 'b'))).toBe('#/s/a/b');
    expect(formatHash(session('a', 'b', 'files'))).toBe('#/s/a/b?tab=files');
  });

  test('anything else is the home', () => {
    for (const hash of [
      '#/nope',
      '#/fleet/extra',
      '#/s',
      '#/s/a',
      '#/s/a/b/c',
      '#/s//b',
      '#/s/a/',
      '#/s/%E0%A4%A/b',
      'fleet',
    ]) {
      expect(parseHash(hash)).toEqual({ name: 'home' });
    }
  });

  test('a hash without the leading # parses like one with it', () => {
    expect(parseHash('/fleet')).toEqual({ name: 'fleet' });
    expect(parseHash('/s/a/b')).toEqual(session('a', 'b'));
  });
});
