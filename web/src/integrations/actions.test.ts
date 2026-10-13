import { describe, expect, test } from 'bun:test';
import { integrationActions } from './actions';
import { assetPath, issuesPath, projectPath } from './actions';
import { issueDetail, issueSummary, project, prDetail, prSummary } from './fixtures';
import { integrationHttp } from './http';

type Call = readonly [string, string, unknown?];

function rig(answer: (method: string, path: string) => unknown = () => ({})) {
  const calls: Call[] = [];
  const io = {
    get: async (path: string) => {
      calls.push(['GET', path]);
      return answer('GET', path);
    },
    post: async (path: string, body: unknown) => {
      calls.push(['POST', path, body]);
      return answer('POST', path);
    },
    put: async (path: string, body: unknown) => {
      calls.push(['PUT', path, body]);
      return answer('PUT', path);
    },
    del: async (path: string) => {
      calls.push(['DELETE', path]);
      return null;
    },
  };
  return { calls, io };
}

describe('integration actions', () => {
  test('the scopes decide canDrive and canAdmin', () => {
    const { io } = rig();
    expect(integrationActions(io, ['read']).canDrive).toBe(false);
    expect(integrationActions(io, ['read', 'drive']).canDrive).toBe(true);
    expect(integrationActions(io, ['read', 'drive']).canAdmin).toBe(false);
    expect(integrationActions(io, ['read', 'drive', 'admin']).canAdmin).toBe(true);
  });

  test('the paths escape ids and keep only the filters that are set', () => {
    expect(projectPath('a/b')).toBe('/api/projects/a%2Fb');
    expect(issuesPath('p1')).toBe('/api/projects/p1/issues');
    expect(issuesPath('p1', { source: '', query: '  ', mine: false })).toBe(
      '/api/projects/p1/issues',
    );
    expect(issuesPath('p1', { source: 'jira-b', query: ' login bug ', mine: true })).toBe(
      '/api/projects/p1/issues?source=jira-b&query=login+bug&mine=true',
    );
    expect(assetPath('p1', undefined, 'https://x/y.png?a=1')).toBe(
      '/api/projects/p1/assets?url=https%3A%2F%2Fx%2Fy.png%3Fa%3D1',
    );
    expect(assetPath('p1', 'forge', 'u')).toBe('/api/projects/p1/assets?source=forge&url=u');
  });

  test('the project CRUD uses GET, POST, PUT and DELETE', async () => {
    const { calls, io } = rig((method, path) => {
      if (method === 'GET') return { projects: [project()] };
      return path.endsWith('/preflight') ? { status: null, checks: [] } : project();
    });
    const actions = integrationActions(io, ['admin']);
    expect((await actions.projects())[0]?.project_id).toBe('p1');
    const input = {
      name: 'Atlas',
      station: 'station-a',
      path: '/work/atlas',
      forge: null,
      trackers: [],
      prompt_template: null,
      review_prompt: null,
    };
    await actions.createProject(input);
    await actions.updateProject('p 1', input);
    await actions.deleteProject('p 1');
    await actions.preflight('p1');
    expect(calls.map((c) => c.slice(0, 2).join(' '))).toEqual([
      'GET /api/projects',
      'POST /api/projects',
      'PUT /api/projects/p%201',
      'DELETE /api/projects/p%201',
      'POST /api/projects/p1/preflight',
    ]);
    expect(calls[2]?.[2]).toEqual(input);
  });

  test('an answer with a wrong shape is refused', async () => {
    const bad = integrationActions(rig(() => ({ nope: 1 })).io, ['admin']);
    await expect(bad.projects()).rejects.toThrow('invalid list');
    await expect(bad.issue('p', 'K-1')).rejects.toThrow('invalid issue');
    await expect(bad.prs('p')).rejects.toThrow('invalid pull request list');
    await expect(
      bad.commentIssue('p', 'K-1', { command_id: 'c', confirm: true, body: 'x' }),
    ).rejects.toThrow('invalid write result');
  });

  test('reads: issues, one issue, pull requests, one pull request, checks', async () => {
    const { calls, io } = rig((_, path) => {
      if (path.endsWith('/issues') || path.includes('/issues?'))
        return { issues: [issueSummary('PROJ-1')] };
      if (path.includes('/issues/')) return issueDetail('PROJ-1');
      if (path.endsWith('/prs')) {
        return {
          groups: { review_requested: [], mine: [prSummary(1)], pending: [], recent_closed: [] },
        };
      }
      if (path.endsWith('/checks')) return { failures: [{ name: 'ci', url: 'u', log_tail: '' }] };
      return prDetail(3);
    });
    const actions = integrationActions(io, ['read']);
    expect((await actions.issues('p1', { mine: true }))[0]?.ref.key).toBe('PROJ-1');
    expect((await actions.issue('p1', '#12', 'forge')).summary.ref.key).toBe('PROJ-1');
    expect((await actions.prs('p1')).mine).toHaveLength(1);
    expect((await actions.pr('p1', 3)).summary.number).toBe(3);
    expect((await actions.prChecks('p1', 3))[0]?.name).toBe('ci');
    expect(calls.map((c) => c.join(' '))).toEqual([
      'GET /api/projects/p1/issues?mine=true',
      'GET /api/projects/p1/issues/%2312?source=forge',
      'GET /api/projects/p1/prs',
      'GET /api/projects/p1/prs/3',
      'GET /api/projects/p1/prs/3/checks',
    ]);
  });

  test('every write posts its own route with confirm and the command id', async () => {
    const { calls, io } = rig(() => ({ url: 'https://x/1', actor: 'ana' }));
    const actions = integrationActions(io, ['drive']);
    const base = { command_id: 'c1', confirm: true };
    await actions.commentIssue('p1', 'PROJ-1', { ...base, body: 'hi' });
    await actions.moveIssue('p1', 'PROJ-1', { ...base, to: 'Done' });
    await actions.commentPr('p1', 4, { ...base, body: 'hi' });
    await actions.reviewPr('p1', 4, { ...base, event: 'approve', body: '' });
    const done = await actions.openPr('p1', {
      ...base,
      head: 'h',
      base: 'main',
      title: 't',
      body: '',
      draft: true,
    });
    expect(done.url).toBe('https://x/1');
    expect(calls.map((c) => `${c[0]} ${c[1]}`)).toEqual([
      'POST /api/projects/p1/issues/PROJ-1/comments',
      'POST /api/projects/p1/issues/PROJ-1/transition',
      'POST /api/projects/p1/prs/4/comments',
      'POST /api/projects/p1/prs/4/reviews',
      'POST /api/projects/p1/prs',
    ]);
    for (const c of calls) {
      expect((c[2] as { confirm: boolean }).confirm).toBe(true);
      expect((c[2] as { command_id: string }).command_id).toBe('c1');
    }
  });

  test('a start posts the batch to the sessions route', async () => {
    const { calls, io } = rig(() => ({ started: [], failed: [] }));
    const actions = integrationActions(io, ['drive']);
    await actions.startSessions('p1', {
      batch_id: 'B1',
      issues: ['PROJ-1'],
      provider_instance: 'claude-1',
      mode: null,
      model: null,
      count_per_issue: 2,
      review_of_pr: null,
    });
    expect(calls[0]?.slice(0, 2)).toEqual(['POST', '/api/projects/p1/sessions']);
    expect(calls[0]?.[2]).toMatchObject({ count_per_issue: 2 });
  });
});

describe('integration http', () => {
  type Seen = { path: string; init: RequestInit };
  const seen: Seen[] = [];
  const fetcher = (response: Response) => async (path: string, init: RequestInit) => {
    seen.push({ path, init });
    return response;
  };

  test('PUT sends the JSON body, the CSRF token and the cookie', async () => {
    seen.length = 0;
    const http = integrationHttp(fetcher(Response.json({ ok: 1 })), () => 'csrf-1', 1000);
    expect(await http.put('/api/projects/p1', { name: 'A' })).toEqual({ ok: 1 });
    const init = seen[0]?.init;
    expect(init?.method).toBe('PUT');
    expect(init?.credentials).toBe('same-origin');
    expect(init?.redirect).toBe('error');
    expect(init?.headers).toEqual({ 'Content-Type': 'application/json', 'X-CSRF-Token': 'csrf-1' });
    expect(init?.body).toBe('{"name":"A"}');
  });

  test('DELETE sends the CSRF token and no body; 204 is null', async () => {
    seen.length = 0;
    const http = integrationHttp(
      fetcher(new Response(null, { status: 204 })),
      () => 'csrf-2',
      1000,
    );
    expect(await http.del('/api/projects/p1')).toBeNull();
    const init = seen[0]?.init;
    expect(init?.method).toBe('DELETE');
    expect(init?.headers).toEqual({ 'X-CSRF-Token': 'csrf-2' });
    expect(init?.body).toBeNull();
  });

  test('without a token no header is sent, and an error throws the deck message', async () => {
    seen.length = 0;
    const http = integrationHttp(
      fetcher(Response.json({ message: 'only the owner' }, { status: 403 })),
      () => null,
      1000,
    );
    await expect(http.del('/x')).rejects.toThrow('only the owner');
    expect(seen[0]?.init.headers).toEqual({});
    const plain = integrationHttp(fetcher(new Response('nope', { status: 502 })), () => null, 1000);
    await expect(plain.put('/x', {})).rejects.toThrow('Request failed (502)');
  });
});
