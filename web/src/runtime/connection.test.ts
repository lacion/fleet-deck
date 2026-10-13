import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { createElement } from 'react';
import { Connection, IDLE_MS, TIMEOUT_MS } from './connection';
import { RuntimeApp } from './RuntimeApp';
import { sessionKey } from './state';
import { card, envelope, FakeEnvironment, key, loginView, settle } from './testing';

async function fixture() {
  const env = new FakeEnvironment();
  const connection = new Connection(env);
  connection.start();
  await settle();
  env.latest().live();
  return { env, connection };
}
const timelineStart = {
  type: 'timeline_start',
  ...key,
  incarnation: 'station-inc',
  reset: false,
} as const;

describe('deck connection runtime', () => {
  test('one page owns one socket and no retry loop starts twice', async () => {
    const { env, connection } = await fixture();
    connection.start();
    connection.start();
    expect(env.sockets).toHaveLength(1);
    expect(env.requests).toHaveLength(1);
    expect(connection.getSnapshot().transport).toBe('connected');
    expect(connection.getSnapshot().freshness).toBe('live');
    env.latest().onerror?.();
    env.latest().drop();
    expect(env.timers.size).toBe(1);
    connection.stop();
    expect(env.timers.size).toBe(0);
  });

  test('an outage retains stale cards; old sockets never overwrite newer live data', async () => {
    const { env, connection } = await fixture();
    const old = env.latest();
    old.drop();
    expect(connection.getSnapshot().sessions[0]?.summary.title).toBe('initial');
    expect(connection.getSnapshot().freshness).toBe('stale');
    env.advance(500);
    await settle();
    env.latest().live(1, [card('new')]);
    old.message({ type: 'snapshot', version: 999, stations: [], sessions: [card('old')] });
    expect(connection.getSnapshot().sessions[0]?.summary.title).toBe('new');
    env.latest().message({
      type: 'delta',
      version: 4,
      stations: null,
      sessions: [card('latest')],
      removed: [],
    });
    env.latest().message({
      type: 'delta',
      version: 3,
      stations: null,
      sessions: [card('stale')],
      removed: [],
    });
    expect(connection.getSnapshot().sessions[0]?.summary.title).toBe('latest');
    env
      .latest()
      .message({ type: 'delta', version: 8, stations: null, sessions: [], removed: [key] });
    expect(connection.getSnapshot().sessions).toHaveLength(0);
  });

  test('a deck restart permits a lower snapshot version and resumes active timeline cursors', async () => {
    const { env, connection } = await fixture();
    connection.watch(key);
    env.latest().message(timelineStart);
    env.latest().message({ type: 'timeline_events', ...key, events: [envelope(21)] });
    env
      .latest()
      .message({ type: 'delta', version: 900, sessions: [], stations: null, removed: [] });
    env.latest().drop();
    env.advance(500);
    await settle();
    env.latest().live(0, [card('after restart')]);
    expect(connection.getSnapshot().version).toBe(0);
    expect(env.latest().sent).toEqual([
      { type: 'subscribe', ...key, incarnation: 'station-inc', after: 21 },
    ]);
    env.latest().message(timelineStart);
    env.latest().message({ type: 'timeline_events', ...key, events: [envelope(21), envelope(24)] });
    expect(
      connection.getSnapshot().timelines[sessionKey(key)]?.events.map((e) => e.station_seq),
    ).toEqual([21, 24]);
  });

  test('back navigation reuses a cursor for five idle minutes then expires the cache', async () => {
    const { env, connection } = await fixture();
    const unwatch = connection.watch(key);
    env.latest().message(timelineStart);
    env.latest().message({ type: 'timeline_events', ...key, events: [envelope(12)] });
    unwatch();
    unwatch();
    env.advance(IDLE_MS - 1);
    const again = connection.watch(key);
    expect(env.latest().sent.at(-1)).toEqual({
      type: 'subscribe',
      ...key,
      incarnation: 'station-inc',
      after: 12,
    });
    env.latest().message(timelineStart);
    again();
    env.advance(IDLE_MS);
    expect(connection.getSnapshot().timelines[sessionKey(key)]).toBeUndefined();
    connection.watch(key);
    expect(env.latest().sent.at(-1)).toEqual({
      type: 'subscribe',
      ...key,
      incarnation: null,
      after: null,
    });
  });

  test('a new station incarnation clears only its timeline and permits low sequence numbers', async () => {
    const { env, connection } = await fixture();
    connection.watch(key);
    env.latest().message(timelineStart);
    env.latest().message({ type: 'timeline_events', ...key, events: [envelope(51)] });
    env.latest().message({ ...timelineStart, incarnation: 'replacement', reset: true });
    env.latest().message({ type: 'timeline_events', ...key, events: [envelope(1)] });
    const t = connection.getSnapshot().timelines[sessionKey(key)];
    expect(t?.cursor).toBe(1);
    expect(t?.events).toHaveLength(1);
  });

  test('timeline failure and timeout do not change connected transport or fleet freshness', async () => {
    const { env, connection } = await fixture();
    connection.watch(key);
    env.latest().message({ type: 'error', code: 'unknown_station', message: 'No such station' });
    expect(connection.getSnapshot().transport).toBe('connected');
    expect(connection.getSnapshot().freshness).toBe('live');
    expect(connection.getSnapshot().timelines[sessionKey(key)]?.error).toBe('No such station');
    env.advance(TIMEOUT_MS);
    expect(connection.getSnapshot().timelines[sessionKey(key)]?.error).toContain('timed out');
    expect(env.sockets).toHaveLength(1);
  });

  test('subscription reference counts send only one subscribe and the last unsubscribe', async () => {
    const { env, connection } = await fixture();
    const one = connection.watch(key);
    const two = connection.watch(key);
    expect(env.latest().sent).toHaveLength(1);
    one();
    expect(env.latest().sent).toHaveLength(1);
    two();
    expect(env.latest().sent.at(-1)?.type).toBe('unsubscribe');
  });

  test('retries use bounded backoff and reset only after a stable connection', async () => {
    const { env, connection } = await fixture();
    for (let i = 0; i < 9; i++) {
      env.latest().drop();
      const task = [...env.timers.values()][0];
      const delay = (task?.at ?? env.clock) - env.clock;
      expect(delay).toBe(Math.min(30_000, 500 * 2 ** Math.min(i, 6)));
      env.advance(delay);
      await settle();
      env.latest().live();
    }
    env.advance(10_000);
    env.latest().drop();
    expect([...env.timers.values()][0]?.at).toBe(env.clock + 500);
    connection.stop();
  });

  test('a connected socket without a snapshot times out and cannot report fresh data', async () => {
    const env = new FakeEnvironment();
    const c = new Connection(env);
    c.start();
    await settle();
    env.latest().open();
    expect(c.getSnapshot().transport).toBe('connected');
    expect(c.getSnapshot().freshness).toBe('stale');
    env.advance(TIMEOUT_MS);
    expect(c.getSnapshot().transport).toBe('reconnecting');
    c.stop();
    expect(env.timers.size).toBe(0);
  });

  test('session reads recover after a failed request without retrying a login POST', async () => {
    const env = new FakeEnvironment();
    const c = new Connection(env);
    env.reply = async () => {
      throw new Error('offline');
    };
    c.start();
    await settle();
    expect(c.getSnapshot().transport).toBe('reconnecting');
    env.reply = async () => Response.json(loginView);
    env.advance(500);
    await settle();
    env.latest().live();
    expect(c.getSnapshot().freshness).toBe('live');
    env.reply = async () => {
      throw new Error('offline');
    };
    await c.login('not-retained');
    expect(c.getSnapshot().auth).toBe('anonymous');
    expect(env.timers.size).toBe(0);
  });

  test('a one-time link is posted once, then reconnect uses only the cookie session', async () => {
    const env = new FakeEnvironment();
    const connection = new Connection(env);
    connection.start('one-time-secret');
    connection.start('one-time-secret');
    await settle();
    env.latest().live();
    expect(env.requests[0]?.path).toBe('/api/auth/link');
    expect(env.requests[0]?.init.body).toBe('{"token":"one-time-secret"}');
    env.latest().drop();
    env.advance(500);
    await settle();
    env.latest().live();
    expect(env.requests.filter((r) => r.path === '/api/auth/link')).toHaveLength(1);
    expect(env.requests.at(-1)?.init.body).toBeNull();
    expect(JSON.stringify(connection.getSnapshot())).not.toContain('one-time-secret');
    connection.stop();
  });

  test('expired login clears credentials and cached data and stops every retry', async () => {
    const { env, connection } = await fixture();
    connection.watch(key);
    env.latest().drop(4401);
    const state = connection.getSnapshot();
    expect(state.auth).toBe('anonymous');
    expect(state.login).toBeNull();
    expect(state.sessions).toHaveLength(0);
    expect(state.timelines).toEqual({});
    expect(env.timers.size).toBe(0);
  });

  test('logout is a CSRF-protected POST and fences any in-flight login read', async () => {
    const { env, connection } = await fixture();
    env.reply = async () => new Response(null, { status: 204 });
    await connection.logout();
    const req = env.requests.at(-1);
    expect(req?.path).toBe('/api/auth/logout');
    expect(req?.init.method).toBe('POST');
    expect(req?.init.headers).toEqual({
      'Content-Type': 'application/json',
      'X-CSRF-Token': 'test-csrf',
    });
    expect(connection.getSnapshot().auth).toBe('anonymous');
    let resolve: ((response: Response) => void) | undefined;
    env.reply = () =>
      new Promise((done) => {
        resolve = done;
      });
    connection.stop();
    connection.start();
    connection.stop();
    resolve?.(Response.json(loginView));
    await settle();
    expect(env.sockets).toHaveLength(1);
  });

  test('malformed JSON and unsupported protocols reconnect without corrupting cached data', async () => {
    const { env, connection } = await fixture();
    env.latest().onmessage?.({ data: 'null' });
    expect(connection.getSnapshot().transport).toBe('reconnecting');
    expect(connection.getSnapshot().sessions).toHaveLength(1);
    env.advance(500);
    await settle();
    env.latest().message({ type: 'hello', protocol: 9, deck_incarnation: 'deck' });
    expect(connection.getSnapshot().freshness).toBe('stale');
  });

  test('cached inactive timelines cannot exceed the 32 active-subscription limit', async () => {
    const { connection } = await fixture();
    const inactive = { station_id: 'station', session_id: 'inactive' };
    connection.watch(inactive)();
    const dispose = Array.from({ length: 32 }, (_, i) =>
      connection.watch({ station_id: 'station', session_id: `active-${i}` }),
    );
    expect(() => connection.watch(inactive)).toThrow('At most 32 timelines can be open');
    expect(() => connection.watch({ station_id: 'station', session_id: '33' })).toThrow();
    for (const unwatch of dispose) unwatch();
    connection.watch(inactive)();
    connection.stop();
  });

  test('reconnect reads fresh cookie credentials and stops if the deck rejects them', async () => {
    const { env, connection } = await fixture();
    env.latest().drop();
    env.reply = async () => Response.json({ message: 'Login expired' }, { status: 401 });
    env.advance(500);
    await settle();
    expect(connection.getSnapshot().auth).toBe('anonymous');
    expect(env.requests.at(-1)?.path).toBe('/api/auth/session');
    expect(env.requests.at(-1)?.init.credentials).toBe('same-origin');
    expect(env.sockets).toHaveLength(1);
    expect(env.timers.size).toBe(0);
  });

  test('duplicate snapshots cannot regress cards or restart ready subscriptions', async () => {
    const { env, connection } = await fixture();
    connection.watch(key);
    env.latest().message(timelineStart);
    env.latest().message({ type: 'snapshot', version: 900, stations: [], sessions: [] });
    expect(connection.getSnapshot().sessions).toHaveLength(1);
    expect(env.latest().sent).toHaveLength(1);
    expect(connection.getSnapshot().timelines[sessionKey(key)]?.ready).toBe(true);
    connection.stop();
  });

  test('a read call aborts after the timeout it is given, the default one otherwise', async () => {
    const { env, connection } = await fixture();
    const abortAt = async (call: () => Promise<unknown>) => {
      const before = new Set(env.timers.keys());
      const pending = call();
      const added = [...env.timers.entries()].filter(([id]) => !before.has(id));
      await pending;
      return added.map(([, t]) => t.at - env.clock);
    };
    expect(await abortAt(() => connection.get('/api/slow', 65_000))).toEqual([65_000]);
    expect(await abortAt(() => connection.get('/api/quick'))).toEqual([TIMEOUT_MS]);
    connection.stop();
  });

  test('the UI shows the owner login and separate stale/connected states', async () => {
    const env = new FakeEnvironment();
    const c = new Connection(env);
    env.reply = async () => Response.json({ message: 'Not logged in' }, { status: 401 });
    c.start();
    await settle();
    const render = () => renderToStaticMarkup(createElement(RuntimeApp, { connection: c }));
    expect(render()).toContain('Owner password');
    expect(render()).toContain('type="password"');
    env.reply = async () => Response.json(loginView);
    await c.login('test-password');
    env.latest().live();
    expect(render()).toContain('Connected · Live fleet');
    env.latest().drop();
    expect(render()).toContain('Cached data is read-only');
    expect(render()).toContain('initial');
  });
});
