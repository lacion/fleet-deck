// B15 T7: the calls of the app host to the deck (design §4.7, §4.8).
import { describe, expect, test } from 'bun:test';
import { DeckError, deckHttp } from '../settings/http';
import { appActions, callReplyOf, callsOf, releaseOf, viewOf } from './actions';

type Seen = { path: string; init: RequestInit };

/** A deck that records the requests and answers with `answer(path)`. */
function deck(answer: (path: string) => unknown, status = 200): { seen: Seen[]; fetch: never } {
  const seen: Seen[] = [];
  const fetch = async (path: string, init: RequestInit): Promise<Response> => {
    seen.push({ path, init });
    return new Response(JSON.stringify(answer(path)), { status });
  };
  return { seen, fetch: fetch as never };
}

const VIEW = {
  url: '/app/tok',
  expires_at: 1,
  server: 'demo',
  tool: 'show_counter',
  resource_uri: 'ui://demo/counter',
  csp: { connect_domains: [], resource_domains: [] },
  prefers_border: null,
  tool_input: null,
  tool_result: null,
};
const CALL = {
  call_id: '01J00000000000000000000000',
  session_id: 's2',
  app_id: 'a1',
  server: 'demo',
  tool: 'bump',
  state: 'uncertain',
  cause: 'provider_error',
  created_at: 5,
};

describe('the app actions', () => {
  test('call and release send the CSRF header', async () => {
    const d = deck((p) =>
      p.endsWith('/release') ? { released: true } : { outcome: 'done', result: 1 },
    );
    const actions = appActions(deckHttp(d.fetch, () => 'csrf-9', 1000));
    await actions.call(
      'st',
      's1',
      'a1',
      { method: 'tools/call', name: 'bump', arguments: {} },
      true,
    );
    await actions.release('st', 's2', CALL.call_id);
    expect(d.seen).toHaveLength(2);
    for (const { init } of d.seen) {
      expect(init.method).toBe('POST');
      expect((init.headers as Record<string, string>)['X-CSRF-Token']).toBe('csrf-9');
    }
    expect(JSON.parse(d.seen[0]?.init.body as string)).toEqual({
      request: { method: 'tools/call', name: 'bump', arguments: {} },
      confirmed: true,
    });
    expect(d.seen[1]?.init.body).toBeNull();
  });

  test("release posts to the app-calls route of the call's own session", async () => {
    const d = deck(() => ({ released: false }));
    const actions = appActions(deckHttp(d.fetch, () => null, 1000));
    const answer = await actions.release('st a', 'other/session', CALL.call_id);
    expect(answer).toEqual({ released: false });
    expect(d.seen[0]?.path).toBe(
      `/api/sessions/st%20a/other%2Fsession/app-calls/${CALL.call_id}/release`,
    );
  });

  test('view and calls are GETs of the session routes', async () => {
    const d = deck((p) => (p.endsWith('/app-calls') ? { calls: [CALL] } : VIEW));
    const actions = appActions(deckHttp(d.fetch, () => null, 1000));
    expect((await actions.view('st', 's1', 'a1')).url).toBe('/app/tok');
    expect((await actions.calls('st', 's1')).calls).toHaveLength(1);
    expect(d.seen.map((s) => [s.init.method, s.path])).toEqual([
      ['GET', '/api/sessions/st/s1/apps/a1'],
      ['GET', '/api/sessions/st/s1/app-calls'],
    ]);
  });

  test('an error answer keeps its code', async () => {
    const d = deck(() => ({ error: 'app_gone', message: 'gone' }), 410);
    const actions = appActions(deckHttp(d.fetch, () => null, 1000));
    const e = await actions.view('st', 's1', 'a1').catch((x: unknown) => x);
    expect(e).toBeInstanceOf(DeckError);
    expect((e as DeckError).code).toBe('app_gone');
  });
});

describe('the shape checks', () => {
  test('a good view passes and a bad one throws', () => {
    expect(viewOf(VIEW).server).toBe('demo');
    expect(() => viewOf({ ...VIEW, csp: { connect_domains: 'x' } })).toThrow();
    expect(() => viewOf({ ...VIEW, url: 3 })).toThrow();
    expect(() => viewOf(null)).toThrow();
  });

  test('call replies', () => {
    expect(callReplyOf({ outcome: 'done', result: { a: 1 } })).toEqual({
      outcome: 'done',
      result: { a: 1 },
    });
    expect(callReplyOf({ outcome: 'confirm', tool: 'bump', title: null }).outcome).toBe('confirm');
    expect(() => callReplyOf({ outcome: 'confirm', tool: 1, title: null })).toThrow();
    expect(() => callReplyOf({ outcome: 'other' })).toThrow();
  });

  test('the call list and the release answer', () => {
    expect(callsOf({ calls: [CALL] }).calls).toHaveLength(1);
    expect(callsOf({ calls: [{ ...CALL, cause: undefined }] }).calls).toHaveLength(1);
    expect(() => callsOf({ calls: [{ ...CALL, state: 'done' }] })).toThrow();
    expect(() => callsOf({ calls: [{ ...CALL, cause: 'x' }] })).toThrow();
    expect(() => callsOf({})).toThrow();
    expect(releaseOf({ released: true })).toEqual({ released: true });
    expect(() => releaseOf({ released: 'yes' })).toThrow();
  });
});
