// FG-7, FG-8: the radar and land calls of the board.
import { describe, expect, test } from 'bun:test';
import { ApiError } from '../runtime/api';
import { integrationActions } from './actions';

type Call = readonly [string, string, unknown?];

function rig(answer: (path: string) => unknown) {
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

describe('the radar and land calls (FG-7, FG-8)', () => {
  test('the radar and the lands read their routes', async () => {
    const { calls, io } = rig((path) =>
      path === '/api/radar'
        ? { groups: [], notices: [{ project_id: 'p1', code: 'fetch_failed', text: 't' }] }
        : { queue: [], recent: [] },
    );
    const actions = integrationActions(io, ['read', 'drive', 'admin']);
    expect(await actions.radar()).toEqual({
      groups: [],
      notices: [{ project_id: 'p1', code: 'fetch_failed', text: 't' }],
    });
    expect(await actions.projectLands('p/1')).toEqual({ queue: [], recent: [] });
    expect(calls.map((c) => c[1])).toEqual(['/api/radar', '/api/projects/p%2F1/lands']);
  });

  test('a 404 means an older deck; another failure is thrown', async () => {
    expect(await failing(404).radar()).toBeNull();
    expect(await failing(404).projectLands('p1')).toBeNull();
    await expect(failing(500, 'boom').radar()).rejects.toThrow('boom');
  });

  test('a radar answer without notices has none, a bad one is refused', async () => {
    const ok = integrationActions(rig(() => ({ groups: [] })).io, ['admin']);
    expect(await ok.radar()).toEqual({ groups: [], notices: [] });
    const bad = integrationActions(rig(() => ({ groups: 3 })).io, ['admin']);
    await expect(bad.radar()).rejects.toThrow('invalid radar');
  });

  test('the land calls post to their routes with a command id where the deck wants one', async () => {
    const { calls, io } = rig((path) =>
      path.endsWith('/land') ? { land_id: 'land-1', position: 2 } : {},
    );
    const actions = integrationActions(io, ['read', 'drive', 'admin']);
    expect(await actions.land('st a', 's1')).toEqual({ land_id: 'land-1', position: 2 });
    await actions.cancelLand('land-1');
    await actions.abandonLand('land-1');
    await actions.forgetLand('land-1');
    await actions.sendLand('land-1');
    expect(calls.map((c) => `${c[0]} ${c[1]}`)).toEqual([
      'POST /api/sessions/st%20a/s1/land',
      'POST /api/lands/land-1/cancel',
      'POST /api/lands/land-1/abandon',
      'POST /api/lands/land-1/forget',
      'POST /api/lands/land-1/send',
    ]);
    const id = { command_id: expect.stringMatching(/^[0-9a-f]{32}$/) };
    expect(calls[0]?.[2]).toEqual(id);
    expect(calls[1]?.[2]).toEqual({});
    expect(calls[4]?.[2]).toEqual(id);
  });

  test('a refused land throws the message of the deck', async () => {
    await expect(failing(409, 'The session is working.').land('st', 's1')).rejects.toThrow(
      'The session is working.',
    );
  });
});
