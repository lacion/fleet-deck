import { describe, expect, test } from 'bun:test';
import { artifactActions, artifactHttp, listPath, rawPath } from './actions';
import { deckArtifact, detail, published } from './fixtures';

type Call = readonly [string, string, unknown?];

function rig(answer: (path: string) => unknown = () => ({})) {
  const calls: Call[] = [];
  const io = {
    get: async (path: string) => {
      calls.push(['GET', path]);
      return answer(path);
    },
    post: async (path: string, body: unknown) => {
      calls.push(['POST', path, body]);
      return answer(path);
    },
    del: async (path: string) => {
      calls.push(['DELETE', path]);
      return null;
    },
    text: async (path: string) => {
      calls.push(['TEXT', path]);
      return '# hi';
    },
  };
  return { calls, io };
}

describe('artifact actions', () => {
  test('the scopes decide canPublish and canShare', () => {
    const { io } = rig();
    expect(artifactActions(io, ['read']).canPublish).toBe(false);
    expect(artifactActions(io, ['read', 'drive']).canPublish).toBe(true);
    expect(artifactActions(io, ['read', 'drive']).canShare).toBe(false);
    expect(artifactActions(io, ['read', 'drive', 'admin']).canShare).toBe(true);
  });

  test('the list path carries only the filters that are set', () => {
    expect(listPath()).toBe('/api/artifacts');
    expect(listPath({ station: '', session: '', q: '  ' })).toBe('/api/artifacts');
    expect(listPath({ station: 'st a', session: 's1', q: ' plan ' })).toBe(
      '/api/artifacts?station=st+a&session=s1&q=plan',
    );
  });

  test('list and detail use the read routes and refuse an invalid answer', async () => {
    const { calls, io } = rig((path) =>
      path === '/api/artifacts' ? [deckArtifact('a1')] : detail('a/1'),
    );
    const actions = artifactActions(io, ['read']);
    expect((await actions.list())[0]?.artifact_id).toBe('a1');
    expect((await actions.detail('st 1', 'a/1')).artifact.artifact_id).toBe('a/1');
    expect(calls.map((c) => c.join(' '))).toEqual([
      'GET /api/artifacts',
      'GET /api/artifacts/st%201/a%2F1',
    ]);
    const bad = artifactActions(rig(() => ({ nope: 1 })).io, ['read']);
    await expect(bad.list()).rejects.toThrow('invalid artifact list');
    await expect(bad.detail('s', 'a')).rejects.toThrow('invalid artifact');
  });

  test('view posts {} for the latest version and {version} for a fixed one', async () => {
    const { calls, io } = rig(() => ({ url: '/a/t/', expires_at: 1 }));
    const actions = artifactActions(io, ['read']);
    await actions.view('st', 'a1', null);
    await actions.view('st', 'a1', 3);
    expect(calls).toEqual([
      ['POST', '/api/artifacts/st/a1/view', {}],
      ['POST', '/api/artifacts/st/a1/view', { version: 3 }],
    ]);
  });

  test('publish posts the request as it is', async () => {
    const { calls, io } = rig(() => published('a1'));
    const request = {
      station: 'st',
      session_id: 's1',
      path: 'out/a b.html',
      title: 'A B',
      artifact_id: null,
    };
    const done = await artifactActions(io, ['drive']).publish(request);
    expect(done.created).toBe(true);
    expect(calls).toEqual([['POST', '/api/artifacts/publish', request]]);
  });

  test('share posts the version and the days; revoke deletes by the share id', async () => {
    const { calls, io } = rig(() => ({
      url: 'https://d/a/t/',
      share_id: 'sh 1',
      version: null,
      expires_at: 1,
    }));
    const actions = artifactActions(io, ['admin']);
    await actions.share('st', 'a1', { version: null, expires_in_days: 7 });
    await actions.share('st', 'a1', { version: 2, expires_in_days: 365 });
    await actions.revoke('st', 'a1', 'sh 1');
    expect(calls).toEqual([
      ['POST', '/api/artifacts/st/a1/shares', { version: null, expires_in_days: 7 }],
      ['POST', '/api/artifacts/st/a1/shares', { version: 2, expires_in_days: 365 }],
      ['DELETE', '/api/artifacts/st/a1/shares/sh%201'],
    ]);
  });

  test('the raw path escapes each segment and keeps the slashes', () => {
    expect(rawPath('st', 'a1', 2, 'img/my pic.png')).toBe(
      '/api/artifacts/st/a1/2/raw/img/my%20pic.png',
    );
    const { calls, io } = rig();
    void artifactActions(io, ['read']).text('st', 'a1', 2, 'notes.md');
    expect(calls).toEqual([['TEXT', '/api/artifacts/st/a1/2/raw/notes.md']]);
  });
});

describe('artifact http (DELETE and text)', () => {
  const seen: { path: string; init: RequestInit }[] = [];
  const fetcher =
    (response: Response) =>
    async (path: string, init: RequestInit): Promise<Response> => {
      seen.push({ path, init });
      return response;
    };

  test('DELETE sends the CSRF token and the cookie, and succeeds on 200', async () => {
    seen.length = 0;
    const http = artifactHttp(fetcher(Response.json({ revoked: true })), () => 'csrf-1', 1000);
    expect(await http.del('/api/artifacts/st/a1/shares/sh1')).toEqual({ revoked: true });
    const init = seen[0]?.init;
    expect(init?.method).toBe('DELETE');
    expect(init?.credentials).toBe('same-origin');
    expect(init?.headers).toEqual({ 'X-CSRF-Token': 'csrf-1' });
  });

  test('an error answer throws the deck message', async () => {
    const http = artifactHttp(
      fetcher(Response.json({ message: 'only the owner' }, { status: 403 })),
      () => null,
      1000,
    );
    await expect(http.del('/x')).rejects.toThrow('only the owner');
    const plain = artifactHttp(fetcher(new Response('nope', { status: 502 })), () => null, 1000);
    await expect(plain.text('/x')).rejects.toThrow('Request failed (502)');
  });

  test('text returns the body of a GET', async () => {
    seen.length = 0;
    const http = artifactHttp(fetcher(new Response('# Title')), () => null, 1000);
    expect(await http.text('/api/artifacts/st/a1/1/raw/a.md')).toBe('# Title');
    expect(seen[0]?.init.method).toBe('GET');
  });
});
