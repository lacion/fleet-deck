import { describe, expect, test } from 'bun:test';
import { formatHash, parseHash, type Route } from '../app/routes';

describe('artifact routes', () => {
  test('#/artifacts is a plain page', () => {
    expect(parseHash('#/artifacts')).toEqual({ name: 'artifacts' });
    expect(formatHash({ name: 'artifacts' })).toBe('#/artifacts');
  });

  test('the viewer route carries the station, the id and an optional version', () => {
    expect(parseHash('#/artifact/st/a1')).toEqual({
      name: 'artifact',
      station: 'st',
      id: 'a1',
      version: null,
    });
    expect(parseHash('#/artifact/st/a1/3')).toEqual({
      name: 'artifact',
      station: 'st',
      id: 'a1',
      version: 3,
    });
  });

  test('a bad viewer route is the home', () => {
    for (const hash of [
      '#/artifact',
      '#/artifact/st',
      '#/artifact//a1',
      '#/artifact/st/a1/0',
      '#/artifact/st/a1/x',
      '#/artifact/st/a1/1.5',
      '#/artifact/st/a1/2/extra',
      '#/artifact/%E0/a1',
    ]) {
      expect(parseHash(hash)).toEqual({ name: 'home' });
    }
  });

  test('round trips keep ids that need escaping', () => {
    const routes: Route[] = [
      { name: 'artifact', station: 'a/b', id: 'c d', version: null },
      { name: 'artifact', station: 'é', id: '100%', version: 7 },
    ];
    for (const route of routes) expect(parseHash(formatHash(route))).toEqual(route);
    expect(formatHash(routes[0] as Route)).toBe('#/artifact/a%2Fb/c%20d');
  });

  test('the session route accepts the artifacts tab', () => {
    expect(parseHash('#/s/a/b?tab=artifacts')).toEqual({
      name: 'session',
      station: 'a',
      session: 'b',
      tab: 'artifacts',
    });
    expect(formatHash({ name: 'session', station: 'a', session: 'b', tab: 'artifacts' })).toBe(
      '#/s/a/b?tab=artifacts',
    );
  });
});
