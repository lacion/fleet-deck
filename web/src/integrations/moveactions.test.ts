// FG-9 (design §12.8, §12.4A): the move calls of the board.
import { describe, expect, test } from 'bun:test';
import { ApiError } from '../runtime/api';
import { integrationActions } from './actions';

type Call = readonly [string, string, unknown?];

function rig(answer: (method: string, path: string) => unknown) {
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
    put: async () => ({}),
    del: async () => null,
  };
  return { calls, io };
}

describe('the move calls (FG-9)', () => {
  test('the move calls use their routes and send a command id', async () => {
    const { calls, io } = rig((method, path) => {
      if (method === 'GET') return { move: null, targets: [] };
      return path.endsWith('/undo') ? { move_id: 'm1', state: 'undoing' } : { move_id: 'm1' };
    });
    const actions = integrationActions(io, ['read', 'drive', 'admin']);
    expect(await actions.moveRead('st a', 's/1')).toEqual({ move: null, targets: [] });
    expect(await actions.moveStart('st a', 's/1', 'Beta')).toEqual({ move_id: 'm1' });
    expect(await actions.moveUndo('m 1')).toEqual({ move_id: 'm1', state: 'undoing' });
    expect(calls.map((c) => `${c[0]} ${c[1]}`)).toEqual([
      'GET /api/sessions/st%20a/s%2F1/move',
      'POST /api/sessions/st%20a/s%2F1/move',
      'POST /api/moves/m%201/undo',
    ]);
    const id = expect.stringMatching(/^[0-9a-f]{32}$/);
    expect(calls[1]?.[2]).toEqual({ to_station: 'Beta', command_id: id });
    expect(calls[2]?.[2]).toEqual({ command_id: id });
  });

  test('a bad answer is refused and a deck refusal keeps its message', async () => {
    const bad = integrationActions(rig(() => ({ move: null, targets: 3 })).io, ['admin']);
    await expect(bad.moveRead('a', 'b')).rejects.toThrow('invalid move answer');
    const none = integrationActions(rig(() => ({})).io, ['admin']);
    await expect(none.moveStart('a', 'b', 'c')).rejects.toThrow('invalid move answer');
    const fail = async (): Promise<never> => {
      throw new ApiError(409, 'The branch is not pushed.', 'move_not_ready');
    };
    const refused = integrationActions({ get: fail, post: fail, put: fail, del: fail }, ['admin']);
    await expect(refused.moveStart('a', 'b', 'c')).rejects.toThrow('The branch is not pushed.');
  });
});
