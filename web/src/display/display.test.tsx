// R13 P2c: the wall display page: the link, the pairing, the directive and the pair page.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { wallDefaults } from '../app/routes';
import type { DirectiveBody, WallControlFrame, WallRemoteState } from '../gen';
import { ApiError } from '../runtime/api';
import { Connection, socketUrl } from '../runtime/connection';
import { FakeEnvironment, settle } from '../runtime/testing';
import { applyFrame, initialControl, shownRemote } from '../wall/control';
import {
  artifactPath,
  changesPath,
  DISPLAY_FEATURES,
  directiveRoute,
  directiveView,
  displayChanges,
  expectedLaunch,
  isDisplayPath,
  pollPairing,
  redeemDisplayLink,
  startPairing,
  takeDisplayLink,
  withDisplayHeader,
} from './api';
import { isDisplayActor } from './DisplayRuntime';
import { countdown, groupCode, PairPage, type PairState } from './PairPage';
import { directiveStep } from './useDirective';

const loc = (hash: string) => ({ hash, pathname: '/display', search: '' });

describe('the display link', () => {
  test('the token is taken once and the fragment is stripped', () => {
    const calls: unknown[][] = [];
    const history = { replaceState: (...a: unknown[]) => calls.push(a) };
    expect(takeDisplayLink(loc('#display=abc'), history)).toBe('abc');
    expect(calls).toEqual([[null, '', '/display']]);
  });

  test('a route hash or an empty hash holds no token and changes nothing', () => {
    const calls: unknown[][] = [];
    const history = { replaceState: (...a: unknown[]) => calls.push(a) };
    expect(takeDisplayLink(loc('#/display-pair?expect=x'), history)).toBeNull();
    expect(takeDisplayLink(loc(''), history)).toBeNull();
    expect(calls).toEqual([]);
  });

  test('only the path /display is the display document', () => {
    expect(isDisplayPath('/display')).toBe(true);
    expect(isDisplayPath('/display/')).toBe(false);
    expect(isDisplayPath('/')).toBe(false);
  });

  test('a redemption posts the token and reports success or failure', async () => {
    const seen: { path: string; init: RequestInit }[] = [];
    const ok = async (path: string, init: RequestInit) => {
      seen.push({ path, init });
      return new Response(null, { status: 204 });
    };
    expect(await redeemDisplayLink(ok, 'tok')).toBe(true);
    expect(seen[0]?.path).toBe('/api/auth/display-link');
    expect(seen[0]?.init.method).toBe('POST');
    expect(seen[0]?.init.body).toBe('{"token":"tok"}');
    const bad = async () =>
      new Response(JSON.stringify({ error: 'invalid_credentials', message: 'no' }), {
        status: 401,
      });
    expect(await redeemDisplayLink(bad, 'tok')).toBe(false);
  });
});

describe('the display header', () => {
  test('every request of the display page carries it', async () => {
    const seen: RequestInit[] = [];
    const fetcher = withDisplayHeader(async (_p, init) => {
      seen.push(init);
      return new Response('{}');
    });
    await fetcher('/x', { headers: { 'Content-Type': 'application/json' } });
    await fetcher('/y', {});
    expect(seen[0]?.headers).toEqual({
      'Content-Type': 'application/json',
      'X-Fleetdeck-Display': '1',
      'X-FleetDeck-Features': 'artifacts_video.v1 wall_control.v1 wall_pages.v1',
    });
    expect(seen[1]?.headers).toEqual({
      'X-Fleetdeck-Display': '1',
      'X-FleetDeck-Features': 'artifacts_video.v1 wall_control.v1 wall_pages.v1',
    });
  });

  test('the_display_socket_declares_wall_control', () => {
    expect(DISPLAY_FEATURES.split(' ')).toEqual([
      'artifacts_video.v1',
      'wall_control.v1',
      'wall_pages.v1',
    ]);
    // The socket of a display declares the three tokens; the board socket still declares one.
    const display = socketUrl('https://deck.example/display#/wall', DISPLAY_FEATURES);
    expect(display.searchParams.get('features')?.split(/[ ,]/)).toContain('wall_control.v1');
    expect(display.pathname).toBe('/api/ws');
    expect(socketUrl('https://deck.example/').search).toBe('?features=artifacts_video.v1');
  });
});

describe('the wall control frames (B17)', () => {
  const picked: WallRemoteState = {
    selected: { station_id: 'st', session_id: 'se' },
    frame_tenths: 30,
  };
  const frame = (
    seq: number,
    generation: number,
    event: WallControlFrame['event'],
    state: WallRemoteState,
    incarnation = 'inc',
  ): WallControlFrame => ({ incarnation, seq, generation, event, state });

  async function live() {
    const env = new FakeEnvironment();
    const connection = new Connection(env);
    connection.start();
    await settle();
    env.latest().live();
    return { env, connection };
  }
  const send = (socket: { onmessage: ((e: { data: string }) => void) | null }, value: unknown) =>
    socket.onmessage?.({ data: JSON.stringify(value) });

  test('a_wall_control_frame_reaches_the_wall', async () => {
    const { env, connection } = await live();
    const seen: WallControlFrame[] = [];
    const stop = connection.onControl((f) => seen.push(f));
    const before = connection.getSnapshot();
    const f = frame(1, 2, { kind: 'control', control: { op: 'select', item: null } }, picked);
    env.latest().message({ type: 'wall_control', frame: f });
    expect(seen).toEqual([f]);
    // The read model does not change, and the connection stays up.
    expect(connection.getSnapshot()).toBe(before);
    expect(connection.getSnapshot().transport).toBe('connected');
    // A listener that stopped gets nothing; one that throws does not end the connection.
    stop();
    connection.onControl(() => {
      throw new Error('boom');
    });
    env
      .latest()
      .message({ type: 'wall_control', frame: frame(2, 2, { kind: 'navigate' }, picked) });
    expect(seen).toHaveLength(1);
    expect(connection.getSnapshot().transport).toBe('connected');
    // The wall applies it: the state of the generation that it shows, with the hold.
    const out = applyFrame(initialControl, f, { shown: 2, now: 1_000 });
    expect(shownRemote(out.state, 2)).toEqual(picked);
    expect(out.state.holdUntil).toBe(121_000);
  });

  test('a_navigate_frame_rereads_the_directive_at_once', () => {
    // The wall shows generation 4; the owner remote stored generation 5 and sent `navigate`.
    const out = applyFrame(initialControl, frame(1, 5, { kind: 'navigate' }, picked), {
      shown: 4,
      now: 0,
    });
    expect(out.reread).toBe(true);
    // The poll then applies generation 5 once, and the kept state shows with it.
    expect(shownRemote(out.state, 5)).toEqual(picked);
    // The frame of the generation that is shown does not read again.
    const same = applyFrame(out.state, frame(2, 5, { kind: 'navigate' }, picked), {
      shown: 5,
      now: 0,
    });
    expect(same.reread).toBe(false);
  });

  test('the_sync_frame_restores_the_selection_after_a_reconnect', async () => {
    const { env, connection } = await live();
    let state = initialControl;
    connection.onControl((f) => {
      state = applyFrame(state, f, { shown: 2, now: 5 }).state;
    });
    const select = frame(3, 2, { kind: 'control', control: { op: 'select', item: null } }, picked);
    env.latest().message({ type: 'wall_control', frame: select });
    expect(shownRemote(state, 2)).toEqual(picked);
    // The socket drops; the wall loses the control state of its page only if it reloads. The new
    // socket starts with a `sync` of the checkpoint, which sets the state again.
    env.latest().drop();
    env.advance(500);
    await settle();
    env.latest().live();
    const moved: WallRemoteState = { selected: null, frame_tenths: 0 };
    env.latest().message({ type: 'wall_control', frame: frame(5, 2, { kind: 'sync' }, moved) });
    expect(shownRemote(state, 2)).toEqual(moved);
    expect(state.holdUntil).toBe(5 + 120_000);
    // A reloaded wall has no state: the same `sync` gives the selection.
    const reloaded = applyFrame(initialControl, frame(5, 2, { kind: 'sync' }, picked), {
      shown: 2,
      now: 9,
    });
    expect(shownRemote(reloaded.state, 2)).toEqual(picked);
    expect(reloaded.state.holdUntil).toBe(0);
  });

  test('an_unknown_frame_still_reconnects', async () => {
    for (const bad of [
      { type: 'wall_unknown', frame: {} },
      { type: 'wall_control', frame: { incarnation: 1 } },
      { type: 'wall_control' },
    ]) {
      const { env, connection } = await live();
      send(env.latest(), bad);
      expect(connection.getSnapshot().transport).toBe('reconnecting');
    }
  });
});

describe('the pairing calls', () => {
  const json =
    (v: unknown, status = 200) =>
    async () =>
      new Response(JSON.stringify(v), { status });

  test('the launch values need both a 26 character id and a number', () => {
    const id = '01J0000000000000000000ABCD';
    expect(expectedLaunch(`expect=${id}&op=3`)).toEqual({
      display_id: id,
      operation_revision: 3,
    });
    expect(expectedLaunch(`expect=${id}`)).toBeNull();
    expect(expectedLaunch('expect=short&op=3')).toBeNull();
    expect(expectedLaunch(`expect=${id}&op=-1`)).toBeNull();
    expect(expectedLaunch(`expect=${id}&op=x`)).toBeNull();
  });

  test('the start posts an empty body, or the launch values', async () => {
    const bodies: unknown[] = [];
    const fetcher = async (_p: string, init: RequestInit) => {
      bodies.push(init.body);
      return new Response(JSON.stringify({ pairing_id: 'p', code: '123456', expires_at: 9 }), {
        status: 200,
      });
    };
    const started = await startPairing(fetcher, null);
    expect(started.code).toBe('123456');
    await startPairing(fetcher, { display_id: 'd'.repeat(26), operation_revision: 4 });
    expect(bodies[0]).toBe('{}');
    expect(bodies[1]).toBe(
      JSON.stringify({ expected_display_id: 'd'.repeat(26), operation_revision: 4 }),
    );
  });

  test('a bad answer is an error, and the poll reads the state', async () => {
    await expect(startPairing(json({ code: 1 }), null)).rejects.toThrow('invalid pairing');
    expect(await pollPairing(json({ state: 'pending' }), 'p')).toBe('pending');
    expect(await pollPairing(json({ state: 'approved_claimed' }), 'p')).toBe('approved_claimed');
    await expect(pollPairing(json({ state: 'x' }), 'p')).rejects.toThrow('invalid pairing state');
  });

  test('a refusal keeps its status and code', async () => {
    const busy = json({ error: 'pairing_busy', message: 'Too many display pairings' }, 503);
    try {
      await startPairing(busy, null);
      throw new Error('no error');
    } catch (e) {
      expect(e).toBeInstanceOf(ApiError);
      expect((e as ApiError).status).toBe(503);
      expect((e as ApiError).code).toBe('pairing_busy');
    }
  });
});

describe('the directive', () => {
  const pin = false;
  const filter = null;
  const cases: [DirectiveBody, Record<string, unknown>][] = [
    [{ mode: 'mission', pin, filter }, { mode: 'mission' }],
    [{ mode: 'ambient', pin, filter }, { mode: 'ambient' }],
    [
      { mode: 'focus', session: { station_id: 'st', session_id: 'se' }, pin, filter },
      { mode: 'focus', session: { station: 'st', id: 'se' } },
    ],
    [
      { mode: 'review', session: { station_id: 'st', session_id: 'se' }, pin: true, filter },
      { mode: 'review', session: { station: 'st', id: 'se' }, pin: true },
    ],
    [
      { mode: 'race', batch_id: 'B1', pin, filter },
      { mode: 'race', batch: 'B1' },
    ],
    [
      {
        mode: 'artifact',
        artifact: { station_id: 'st', artifact_id: 'ar', version: 3 },
        pin,
        filter,
      },
      { mode: 'artifact', artifact: { station: 'st', id: 'ar', version: 3 } },
    ],
  ];
  for (const [body, want] of cases) {
    test(`mode ${body.mode} maps to its wall route`, () => {
      const route = directiveRoute(body);
      expect(route).toMatchObject({ name: 'wall', pin: body.pin, stations: [], repo: '', ...want });
    });
  }

  test('the scale and the shape stay and a stale target goes', () => {
    const base = {
      ...wallDefaults,
      scale: 1.5,
      shape: 'wide' as const,
      session: { station: 'old', id: 'old' },
      batch: 'old',
    };
    const route = directiveRoute({ mode: 'mission', pin: false, filter: null }, base);
    expect(route.scale).toBe(1.5);
    expect(route.shape).toBe('wide');
    expect(route.session).toBeUndefined();
    expect(route.batch).toBeUndefined();
  });

  test('a poll applies a generation once and the first answer always applies', async () => {
    const answer = (generation: number) => async () => ({
      generation,
      directive: { mode: 'ambient', pin: false, filter: null },
    });
    const first = await directiveStep(answer(0), null, wallDefaults);
    expect(first?.generation).toBe(0);
    expect(first?.route.mode).toBe('ambient');
    expect(await directiveStep(answer(0), 0, wallDefaults)).toBeNull();
    expect((await directiveStep(answer(1), 0, wallDefaults))?.generation).toBe(1);
  });

  test('a malformed answer is refused', () => {
    expect(() => directiveView({ generation: 'x' })).toThrow('invalid directive');
    expect(() => directiveView(null)).toThrow('invalid directive');
  });
});

describe('the paths', () => {
  test('the change summary and the artifact paths are encoded', () => {
    expect(changesPath('st a', 'se/b', true)).toBe(
      '/api/displays/self/sessions/st%20a/se%2Fb/changes?patch=1',
    );
    expect(changesPath('s', 't', false)).toContain('patch=0');
    expect(artifactPath('st', 'ar', 2)).toBe('/api/displays/self/artifact/st/ar/2/_');
  });

  test('a malformed change answer is refused', () => {
    expect(() => displayChanges({})).toThrow('invalid change summary');
  });

  test('the actor of a display login starts with display:', () => {
    expect(isDisplayActor('display:Wall')).toBe(true);
    expect(isDisplayActor('owner')).toBe(false);
  });
});

describe('the pair page', () => {
  test('the code is grouped and the countdown is M:SS', () => {
    expect(groupCode('123456')).toBe('123 456');
    expect(countdown(65_000)).toBe('1:05');
    expect(countdown(-5)).toBe('0:00');
  });

  const page = (initial: PairState) =>
    renderToStaticMarkup(
      <PairPage
        fetcher={async () => new Response('{}')}
        launch={null}
        onClaimed={() => {}}
        now={1_000}
        initial={initial}
      />,
    );

  test('it shows the code, the instruction and the time', () => {
    const html = page({ kind: 'waiting', code: '482913', expiresAt: 301_000 });
    expect(html).toContain('482 913');
    expect(html).toContain('Pair this wall display');
    expect(html).toContain(
      'On your Fleet Deck board, open Settings, Displays, and enter this code.',
    );
    expect(html).toContain('The code ends in 5:00');
  });

  test('it shows a superseded launch and a busy deck as text', () => {
    expect(
      page({ kind: 'superseded', text: 'This wall launch was replaced by a newer one.' }),
    ).toContain('This wall launch was replaced by a newer one.');
    expect(page({ kind: 'wait', text: 'Too many display pairings; try again later' })).toContain(
      'Too many display pairings; try again later',
    );
  });

  test('it has no control', () => {
    for (const state of [
      { kind: 'starting' as const },
      { kind: 'waiting' as const, code: '111111', expiresAt: 9_000 },
    ]) {
      const html = page(state);
      expect(html).not.toMatch(/<(button|input|textarea|select|form|a)\b/);
    }
  });
});
