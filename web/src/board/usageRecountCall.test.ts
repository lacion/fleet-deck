// B20 (design `usage-cost.md` §4.4): the typed call of the recount.
import { describe, expect, test } from 'bun:test';
import { ApiError } from '../runtime/api';
import { usageRecountCall } from './usageRecountCall';

const key = { station_id: 'st-a', session_id: 's1' };

/** A deck command answer; the receipt is `accepted`, `rejected` with a message, or absent. */
const answer = (
  state: string,
  receipt: 'accepted' | { reject: string } | null = null,
  reason: string | null = null,
) => ({
  command_id: 'c1',
  state,
  reason,
  receipt:
    receipt === null
      ? null
      : receipt === 'accepted'
        ? { state: 'accepted', reject: null }
        : { state: 'rejected', reject: { code: 'invalid', message: receipt.reject } },
});

const options = (extra: Parameters<typeof usageRecountCall>[2] = {}) => ({
  newId: () => 'c1',
  sleep: async () => {},
  ...extra,
});

describe('the recount call (usage-cost §4.4)', () => {
  test('an_accepted_receipt_is_accepted', async () => {
    const posts: { path: string; body: unknown }[] = [];
    const call = usageRecountCall(
      async () => ({}),
      async (path, body) => {
        posts.push({ path, body });
        return answer('done', 'accepted');
      },
      options(),
    );
    expect(await call(key)).toEqual({ kind: 'accepted', commandId: 'c1' });
    expect(posts).toEqual([
      {
        path: '/api/commands/session.usage_recount',
        body: {
          station: 'st-a',
          command_id: 'c1',
          command: { op: 'usage_recount', session_id: 's1' },
        },
      },
    ]);
  });

  test('a_rejected_receipt_keeps_its_code', async () => {
    const call = (message: string) =>
      usageRecountCall(
        async () => ({}),
        async () => answer('done', { reject: message }),
        options(),
      );
    expect(await call('session_running: This session still runs.')(key)).toEqual({
      kind: 'refused',
      code: 'session_running',
      message: 'This session still runs.',
    });
    // A message without a code keeps its whole text under the code `refused`.
    expect(await call('Something went wrong: try later')(key)).toEqual({
      kind: 'refused',
      code: 'refused',
      message: 'Something went wrong: try later',
    });
    expect(await call('no code here')(key)).toEqual({
      kind: 'refused',
      code: 'refused',
      message: 'no code here',
    });
  });

  test('not_supported_is_its_own_result', async () => {
    const call = usageRecountCall(
      async () => ({}),
      async () => {
        throw new ApiError(409, 'this station cannot recount usage', 'not_supported');
      },
      options(),
    );
    expect(await call(key)).toEqual({ kind: 'not_supported' });
    // Any other thrown error is a failure with its message.
    const other = usageRecountCall(
      async () => ({}),
      async () => {
        throw new ApiError(403, 'only the owner', 'owner_only');
      },
      options(),
    );
    expect(await other(key)).toEqual({ kind: 'failed', text: 'only the owner' });
  });

  test('a_pending_command_is_polled_until_done', async () => {
    const gets: string[] = [];
    const waits: number[] = [];
    const states = ['pending', 'pending', 'done'];
    const call = usageRecountCall(
      async (path) => {
        gets.push(path);
        const state = states.shift() ?? 'done';
        return answer(state, state === 'done' ? 'accepted' : null);
      },
      async () => answer('pending'),
      options({
        pollMs: 250,
        sleep: async (ms) => {
          waits.push(ms);
        },
      }),
    );
    expect(await call(key)).toEqual({ kind: 'accepted', commandId: 'c1' });
    expect(gets).toEqual(['/api/commands/c1', '/api/commands/c1', '/api/commands/c1']);
    expect(waits).toEqual([250, 250, 250]);
  });

  test('a_pending_command_past_the_limit_stays_pending', async () => {
    let gets = 0;
    const call = usageRecountCall(
      async () => {
        gets += 1;
        return answer('pending');
      },
      async () => answer('pending'),
      options({ pollMs: 1000, limitMs: 3000 }),
    );
    expect(await call(key)).toEqual({ kind: 'pending', commandId: 'c1' });
    expect(gets).toBe(3);
  });

  test('failed_and_ambiguous_are_failed', async () => {
    const call = (state: string, reason: string | null) =>
      usageRecountCall(
        async () => ({}),
        async () => answer(state, null, reason),
        options(),
      );
    expect(await call('failed', 'the link is down')(key)).toEqual({
      kind: 'failed',
      text: 'Not delivered: the link is down',
    });
    expect(await call('ambiguous', null)(key)).toEqual({
      kind: 'failed',
      text: 'Delivery is unknown.',
    });
    // A body that is not a command is a failure, not a crash.
    const bad = usageRecountCall(
      async () => ({}),
      async () => 'nope',
      options(),
    );
    expect(await bad(key)).toEqual({
      kind: 'failed',
      text: 'The deck sent an invalid command response.',
    });
  });
});
