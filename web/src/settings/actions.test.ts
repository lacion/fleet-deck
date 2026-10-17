import { describe, expect, test } from 'bun:test';
import { accountActions } from '../accounts/actions';
import { DECK, restartTarget, settingsActions, settingsPath } from './actions';
import { DeckError, deckHttp, type DeckIo } from './http';

type Call = { method: 'GET' | 'POST'; path: string; body: unknown };

/** An io that records the calls and answers with `answer(path)`. */
function io(answer: (path: string) => unknown = () => ({})): { io: DeckIo; calls: Call[] } {
  const calls: Call[] = [];
  return {
    calls,
    io: {
      get: async (path) => {
        calls.push({ method: 'GET', path, body: undefined });
        return answer(path);
      },
      post: async (path, body) => {
        calls.push({ method: 'POST', path, body });
        return answer(path);
      },
    },
  };
}

const VIEW = { values: {}, fields: [], revision: 'r', overrides: [] };
const SAVED = {
  values: {},
  applied: [],
  restart_needed: [],
  unapplied: [],
  revision: 'r2',
  durability: 'confirmed',
};

describe('the settings routes', () => {
  test('the paths of the deck and of a station', () => {
    expect(settingsPath(DECK)).toBe('/api/settings/deck');
    expect(settingsPath({ kind: 'station', id: 'st a/1' })).toBe(
      '/api/stations/st%20a%2F1/settings',
    );
    expect(restartTarget(DECK)).toBe('deck');
    expect(restartTarget({ kind: 'station', id: 'st-1' })).toBe('st-1');
  });

  test('view, save, secret and restart use the deck routes with the shapes of the deck', async () => {
    const t = io((path) => {
      if (path.endsWith('/restart')) return { restarting: true, would_stop: [] };
      return path.includes('/secrets/') ? SAVED : VIEW;
    });
    const a = settingsActions(t.io);
    await a.view({ kind: 'station', id: 'st-1' });
    await a.setSecret('st-1', {
      key: 'provider_instance.c.env.K',
      value: 'v',
      revision: 'r',
      elevation: 'el',
    });
    await a.restart({ kind: 'station', id: 'st-1' }, true, 'el');
    expect(t.calls).toEqual([
      { method: 'GET', path: '/api/stations/st-1/settings', body: undefined },
      {
        method: 'POST',
        path: '/api/secrets/stations/st-1/settings',
        body: { key: 'provider_instance.c.env.K', value: 'v', revision: 'r', elevation: 'el' },
      },
      {
        method: 'POST',
        path: '/api/settings/restart',
        body: { target: 'st-1', stop_direct_sessions: true, elevation: 'el' },
      },
    ]);
  });

  test('a save posts the change as it is', async () => {
    const t = io(() => SAVED);
    const a = settingsActions(t.io);
    await a.save(DECK, { changes: { name: null }, revision: 'r', elevation: null });
    expect(t.calls[0]).toEqual({
      method: 'POST',
      path: '/api/settings/deck',
      body: { changes: { name: null }, revision: 'r', elevation: null },
    });
  });

  test('an answer of the wrong shape is an error', async () => {
    const a = settingsActions(io(() => ({ nope: 1 })).io);
    await expect(a.view(DECK)).rejects.toThrow('invalid settings view');
    await expect(a.restart(DECK, false, 'e')).rejects.toThrow('invalid restart answer');
  });
});

describe('the elevation memory', () => {
  test('it is kept until it expires, and it can be forgotten', async () => {
    let now = 1_000_000;
    const a = settingsActions(
      io(() => ({ elevation: 'el-1', expires_at: 1_000_000 + 600_000 })).io,
      () => now,
    );
    expect(a.elevation()).toBeNull();
    await a.elevate({ password: 'pw' });
    expect(a.elevation()).toBe('el-1');
    now += 500_000;
    expect(a.elevation()).toBe('el-1');
    // Five seconds before the deck's limit it is dropped.
    now = 1_000_000 + 600_000 - 4_000;
    expect(a.elevation()).toBeNull();
    now = 1_000_000;
    expect(a.elevation()).toBe('el-1');
    a.forgetElevation();
    expect(a.elevation()).toBeNull();
  });

  test('it is posted with the password or the code, and never stored by the board', async () => {
    const t = io(() => ({ elevation: 'el-1', expires_at: Date.now() + 600_000 }));
    const a = settingsActions(t.io);
    await a.elevate({ code: 'ABCDE12345' });
    expect(t.calls[0]).toEqual({
      method: 'POST',
      path: '/api/auth/elevate',
      body: { code: 'ABCDE12345' },
    });
    const src = await Bun.file(new URL('./actions.ts', import.meta.url)).text();
    expect(src).not.toMatch(/localStorage|sessionStorage|indexedDB/);
  });

  test('a bad elevation answer is an error', async () => {
    const a = settingsActions(io(() => ({ elevation: 1, expires_at: 'x' })).io);
    await expect(a.elevate({ password: 'p' })).rejects.toThrow('invalid elevation');
  });
});

describe('the account routes', () => {
  const ACCOUNT = { account_id: 'a', kind: 'jira', method: 'token', status: 'ok', message: '' };

  test('paths and bodies', async () => {
    const t = io((path) => {
      if (path.endsWith('/accounts')) return { accounts: [], apps: [], env_tokens: [] };
      if (path.endsWith('/containers')) return { containers: [] };
      if (path.includes('/forge/login/')) return { state: 'pending' };
      if (path.endsWith('/forge/login'))
        return { login_id: 'l', code: 'A-B', url: 'u', expires_in: 1 };
      if (path.endsWith('/apps'))
        return { kind: 'jira', client_id: 'c', has_secret: true, generation: 1 };
      if (path.endsWith('/oauth/start')) return { authorize_url: 'https://auth.atlassian.com/x' };
      return ACCOUNT;
    });
    const a = accountActions(t.io);
    await a.list('st-1');
    await a.forgeLogin('st-1', 'github', 'github.com');
    await a.forgeLoginStatus('st-1', 'l 1');
    await a.logout('st-1', 'gh-1');
    await a.containers('st-1', 'jira-1');
    await a.setApp('st-1', { kind: 'jira', client_id: 'c', client_secret: 's' });
    await a.setToken('st-1', {
      kind: 'jira',
      site: 'https://a.atlassian.net',
      email: 'e',
      token: 't',
    });
    await a.oauthStart('st-1', 'jira');
    await a.oauthFinish('tx-1');
    expect(t.calls.map((c) => `${c.method} ${c.path}`)).toEqual([
      'GET /api/stations/st-1/accounts',
      'POST /api/stations/st-1/accounts/forge/login',
      'GET /api/stations/st-1/accounts/forge/login/l%201',
      'POST /api/stations/st-1/accounts/gh-1/logout',
      'GET /api/stations/st-1/accounts/jira-1/containers',
      'POST /api/secrets/stations/st-1/accounts/apps',
      'POST /api/secrets/stations/st-1/accounts/token',
      'POST /api/accounts/oauth/start',
      'POST /api/accounts/oauth/finish',
    ]);
    expect(t.calls[1]?.body).toEqual({ kind: 'github', host: 'github.com' });
    expect(t.calls[7]?.body).toEqual({ station: 'st-1', kind: 'jira' });
    expect(t.calls[8]?.body).toEqual({ transaction: 'tx-1' });
  });
});

describe('the transport', () => {
  const fetcher =
    (status: number, body: unknown) =>
    async (_path: string, _init: RequestInit): Promise<Response> =>
      new Response(JSON.stringify(body), {
        status,
        headers: { 'Content-Type': 'application/json' },
      });

  test('an error answer keeps its code, its text and its details', async () => {
    const http = deckHttp(
      fetcher(409, {
        error: 'restart_would_stop_sessions',
        message: 'a restart would end live work',
        would_stop: [{ kind: 'session', id: 's', title: 't' }],
      }),
      () => 'csrf',
      1000,
    );
    const e = await http.post('/api/settings/restart', {}).catch((x: unknown) => x);
    expect(e).toBeInstanceOf(DeckError);
    if (!(e instanceof DeckError)) return;
    expect(e.status).toBe(409);
    expect(e.code).toBe('restart_would_stop_sessions');
    expect(e.message).toBe('a restart would end live work');
    expect(e.details).toEqual({ would_stop: [{ kind: 'session', id: 's', title: 't' }] });
  });

  test('it sends the cookie, the CSRF token and the body only on a POST', async () => {
    const seen: { path: string; init: RequestInit }[] = [];
    const http = deckHttp(
      async (path, init) => {
        seen.push({ path, init });
        return new Response('{}', { status: 200 });
      },
      () => 'csrf-1',
      1000,
    );
    await http.get('/api/a');
    await http.post('/api/b', { x: 1 });
    expect(seen[0]?.init.method).toBe('GET');
    expect(seen[0]?.init.body).toBeNull();
    expect(seen[1]?.init.method).toBe('POST');
    expect(seen[1]?.init.body).toBe('{"x":1}');
    expect(seen[1]?.init.credentials).toBe('same-origin');
    expect(seen[1]?.init.cache).toBe('no-store');
    const headers = (seen[1]?.init.headers ?? {}) as Record<string, string>;
    expect(headers['X-CSRF-Token']).toBe('csrf-1');
  });

  test('a body that is not JSON keeps the status text', async () => {
    const http = deckHttp(
      async () => new Response('<html>', { status: 502 }),
      () => null,
      1000,
    );
    const e = await http.get('/x').catch((x: unknown) => x);
    expect(e).toBeInstanceOf(DeckError);
    expect((e as DeckError).message).toBe('Request failed (502)');
  });
});
