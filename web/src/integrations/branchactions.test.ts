// FG-10: the branches and fetch calls of the board.
import { describe, expect, test } from 'bun:test';
import { ApiError } from '../runtime/api';
import { integrationActions } from './actions';

type Call = readonly [string, string, unknown?];

function rig(answer: unknown) {
  const calls: Call[] = [];
  const io = {
    get: async (path: string) => {
      calls.push(['GET', path]);
      return answer;
    },
    post: async (path: string, body: unknown) => {
      calls.push(['POST', path, body]);
      return answer;
    },
    put: async () => ({}),
    del: async () => null,
  };
  return { calls, io };
}

const failing = (status: number, message = 'x') => {
  const fail = async (): Promise<never> => {
    throw new ApiError(status, message, null);
  };
  return integrationActions({ get: fail, post: fail, put: fail, del: fail }, ['read', 'admin']);
};

describe('the branches and fetch calls (FG-10)', () => {
  test('the branches read their route and the fetch posts an empty body', async () => {
    const list = { remote: 'origin', default_branch: 'main', truncated: false, branches: [] };
    const read = rig(list);
    expect(await integrationActions(read.io, ['admin']).projectBranches('p/1')).toEqual(list);
    expect(read.calls).toEqual([['GET', '/api/projects/p%2F1/branches']]);
    const fetch = rig({ fetched_at: 5 });
    expect(await integrationActions(fetch.io, ['admin']).fetchProject('p/1')).toEqual({
      fetched_at: 5,
    });
    expect(fetch.calls).toEqual([['POST', '/api/projects/p%2F1/fetch', {}]]);
  });

  test('a 404 means an older deck; another failure is thrown', async () => {
    expect(await failing(404).projectBranches('p1')).toBeNull();
    await expect(failing(409, 'sync off').projectBranches('p1')).rejects.toThrow('sync off');
    await expect(failing(502, 'bad').fetchProject('p1')).rejects.toThrow('bad');
  });

  test('an invalid answer is refused', async () => {
    const bad = integrationActions(rig({ branches: 3 }).io, ['admin']);
    await expect(bad.projectBranches('p1')).rejects.toThrow('invalid branch list');
    const none = integrationActions(rig(null).io, ['admin']);
    await expect(none.fetchProject('p1')).rejects.toThrow('invalid fetch answer');
  });
});
