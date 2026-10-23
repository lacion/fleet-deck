import { describe, expect, test } from 'bun:test';
import { formatHash, parseHash, type Route, type WallRoute, wallDefaults } from './routes';

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
      'automations',
      'stations',
      'files',
      'watch',
      'activity',
      'terminals',
      'more',
      'shares',
      'spaces',
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

  test('the people tab of a session is in the URL (R11.6)', () => {
    expect(parseHash('#/s/a/b?tab=people')).toEqual({
      name: 'session',
      station: 'a',
      session: 'b',
      tab: 'people',
    });
    expect(formatHash({ name: 'session', station: 'a', session: 'b', tab: 'people' })).toBe(
      '#/s/a/b?tab=people',
    );
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

  test('the projects routes (R3): the list and one project', () => {
    expect(parseHash('#/projects')).toEqual({ name: 'projects' });
    expect(formatHash({ name: 'projects' })).toBe('#/projects');
    expect(parseHash('#/project/p%201')).toEqual({ name: 'project', id: 'p 1' });
    expect(formatHash({ name: 'project', id: 'p 1' })).toBe('#/project/p%201');
    const route: Route = { name: 'project', id: 'a/b' };
    expect(parseHash(formatHash(route))).toEqual(route);
    for (const hash of ['#/project', '#/project/', '#/project/a/b', '#/project/%E0%A4%A']) {
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

  test('the Displays tab and the wall artifact version parse and format back (R13 P2c)', () => {
    const tab: Route = { name: 'settings', tab: 'displays' };
    expect(formatHash(tab)).toBe('#/settings/displays');
    expect(parseHash('#/settings/displays')).toEqual(tab);
    const wall = parseHash('#/wall?mode=artifact&artifact=st/ar&version=3');
    expect(wall).toMatchObject({
      mode: 'artifact',
      artifact: { station: 'st', id: 'ar', version: 3 },
    });
    expect(formatHash(wall)).toContain('version=3');
    expect(parseHash('#/wall?mode=artifact&artifact=st/ar&version=0')).toMatchObject({
      artifact: { station: 'st', id: 'ar' },
    });
    expect(parseHash('#/wall?mode=artifact&artifact=st/ar')).not.toHaveProperty('artifact.version');
  });

  test('the settings routes parse and format back (DL-172, DL-173)', () => {
    expect(parseHash('#/settings')).toEqual({ name: 'settings', tab: 'deck' });
    expect(formatHash({ name: 'settings', tab: 'deck' })).toBe('#/settings');
    const station: Route = { name: 'settings', tab: 'station', station: 'st a' };
    expect(formatHash(station)).toBe('#/settings/station/st%20a');
    expect(parseHash('#/settings/station/st%20a')).toEqual(station);
    expect(parseHash('#/settings/station/')).toEqual({ name: 'home' });
    expect(parseHash('#/settings/accounts')).toEqual({
      name: 'settings',
      tab: 'accounts',
      station: null,
      finish: null,
      oauthError: null,
    });
    expect(parseHash('#/settings/accounts?station=s1&finish=t-1')).toEqual({
      name: 'settings',
      tab: 'accounts',
      station: 's1',
      finish: 't-1',
      oauthError: null,
    });
    expect(parseHash('#/settings/accounts?oauth_error=invalid_state')).toEqual({
      name: 'settings',
      tab: 'accounts',
      station: null,
      finish: null,
      oauthError: 'invalid_state',
    });
    const back: Route = {
      name: 'settings',
      tab: 'accounts',
      station: 's1',
      finish: 'x',
      oauthError: null,
    };
    expect(parseHash(formatHash(back))).toEqual(back);
    expect(parseHash('#/settings/nope')).toEqual({ name: 'home' });
  });

  test('the agent of the chat tab is in the URL and round-trips (B6)', () => {
    const route: Route = {
      name: 'session',
      station: 'a',
      session: 'b',
      tab: 'chat',
      agent: 'node-2',
    };
    expect(formatHash(route)).toBe('#/s/a/b?tab=chat&agent=node-2');
    expect(parseHash('#/s/a/b?tab=chat&agent=node-2')).toEqual(route);
    expect(parseHash('#/s/a/b?agent=node-2')).toEqual(route);
    const odd: Route = { name: 'session', station: 'a', session: 'b', tab: 'chat', agent: 'n/1 x' };
    expect(parseHash(formatHash(odd))).toEqual(odd);
  });

  test('a session route without an agent has no agent key; an empty agent is none', () => {
    expect(formatHash(session('a', 'b'))).toBe('#/s/a/b');
    expect('agent' in parseHash('#/s/a/b')).toBe(false);
    expect('agent' in parseHash('#/s/a/b?agent=')).toBe(false);
    expect(
      formatHash({ name: 'session', station: 'a', session: 'b', tab: 'chat', agent: '' }),
    ).toBe('#/s/a/b');
  });

  test('the agent belongs to the chat tab only', () => {
    expect('agent' in parseHash('#/s/a/b?tab=files&agent=node-2')).toBe(false);
    expect(
      formatHash({ name: 'session', station: 'a', session: 'b', tab: 'files', agent: 'node-2' }),
    ).toBe('#/s/a/b?tab=files');
  });
});

describe('the wall route (R13 P1)', () => {
  test('a bare #/wall has the defaults', () => {
    expect(parseHash('#/wall')).toEqual(wallDefaults);
    expect(parseHash('#/wall?')).toEqual(wallDefaults);
  });

  test('every field round-trips', () => {
    const full: WallRoute = {
      name: 'wall',
      mode: 'race',
      stations: ['station-a', 'station-b'],
      repo: 'atlas',
      scale: 1.5,
      shape: 'ultra',
      session: { station: 'station-a', id: 's1' },
      batch: 'B1',
      artifact: { station: 'station-b', id: 'art1' },
      pin: true,
    };
    expect(parseHash(formatHash(full))).toEqual(full);
    for (const mode of ['mission', 'ambient'] as const) {
      const r: WallRoute = { ...wallDefaults, mode };
      expect(parseHash(formatHash(r))).toEqual(r);
    }
    for (const shape of ['auto', 'ultra', 'wide', 'standard', 'portrait'] as const) {
      const r: WallRoute = { ...wallDefaults, shape };
      expect(parseHash(formatHash(r))).toEqual(r);
    }
    const focus: WallRoute = { ...wallDefaults, mode: 'focus', session: { station: 'a', id: 'b' } };
    expect(parseHash(formatHash(focus))).toEqual(focus);
    const review: WallRoute = {
      ...wallDefaults,
      mode: 'review',
      session: { station: 'a', id: 'b' },
    };
    expect(parseHash(formatHash(review))).toEqual(review);
    const art: WallRoute = {
      ...wallDefaults,
      mode: 'artifact',
      artifact: { station: 'a', id: 'x' },
    };
    expect(parseHash(formatHash(art))).toEqual(art);
  });

  test('the hash has the documented parameters; pin is left out when false', () => {
    const hash = formatHash({
      ...wallDefaults,
      mode: 'focus',
      stations: ['a', 'b'],
      repo: 'r',
      scale: 2,
      session: { station: 'a', id: 's' },
    });
    expect(hash).toBe('#/wall?mode=focus&stations=a%2Cb&repo=r&scale=2&shape=auto&session=a%2Fs');
    expect(hash).not.toContain('pin');
    expect(formatHash({ ...wallDefaults, pin: true })).toContain('pin=1');
  });

  test('scale is clamped to 0.75 to 2', () => {
    expect((parseHash('#/wall?scale=0.1') as WallRoute).scale).toBe(0.75);
    expect((parseHash('#/wall?scale=9') as WallRoute).scale).toBe(2);
    expect((parseHash('#/wall?scale=abc') as WallRoute).scale).toBe(1);
    expect((parseHash('#/wall?scale=1.25') as WallRoute).scale).toBe(1.25);
  });

  test('an unknown mode or shape is the default', () => {
    const r = parseHash('#/wall?mode=party&shape=round') as WallRoute;
    expect(r.mode).toBe('mission');
    expect(r.shape).toBe('auto');
  });

  test('a mode without its target falls back to mission', () => {
    for (const hash of [
      '#/wall?mode=focus',
      '#/wall?mode=review',
      '#/wall?mode=race',
      '#/wall?mode=artifact',
      '#/wall?mode=focus&session=nope',
    ]) {
      expect((parseHash(hash) as WallRoute).mode).toBe('mission');
    }
    expect((parseHash('#/wall?mode=race&batch=B') as WallRoute).mode).toBe('race');
  });
});

describe('the remote route (B17)', () => {
  test('remote_route_round_trips', () => {
    const route: Route = { name: 'remote', id: '01JABCDEFGHJKMNPQRSTVWXYZ0' };
    expect(formatHash(route)).toBe('#/remote/01JABCDEFGHJKMNPQRSTVWXYZ0');
    expect(parseHash(formatHash(route))).toEqual(route);
    // A part with a slash or a space is encoded and comes back as it was.
    const odd: Route = { name: 'remote', id: 'a b/c' };
    expect(formatHash(odd)).toBe('#/remote/a%20b%2Fc');
    expect(parseHash(formatHash(odd))).toEqual(odd);
    // A missing id, an extra part and a bad escape are the home.
    expect(parseHash('#/remote')).toEqual({ name: 'home' });
    expect(parseHash('#/remote/')).toEqual({ name: 'home' });
    expect(parseHash('#/remote/a/b')).toEqual({ name: 'home' });
    expect(parseHash('#/remote/%E0%A4%A')).toEqual({ name: 'home' });
  });
});
