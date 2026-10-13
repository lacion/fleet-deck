import { describe, expect, test } from 'bun:test';
import {
  authLabel,
  confirmMatches,
  expiresText,
  invitationView,
  requestedName,
  stationActions,
  stationResult,
  worktreeActor,
} from './pairing';

describe('pairing calls', () => {
  test('each action posts to its board method with an encoded station name', async () => {
    const calls: [string, unknown][] = [];
    const replies: unknown[] = [
      { invitation: 'fdinv1_x', expires_at: 1, join_command: 'fleetdeck station join' },
      { station_id: 's1', name: 'alpha' },
      { station_id: 's2', name: 'a b/c' },
    ];
    const actions = stationActions(async (path, body) => {
      calls.push([path, body]);
      return replies.shift();
    }, true);
    expect((await actions.invite()).invitation).toBe('fdinv1_x');
    expect((await actions.add({ invitation: 'fdinv1_y', name: null })).name).toBe('alpha');
    expect((await actions.remove('a b/c')).station_id).toBe('s2');
    expect(calls).toEqual([
      ['/api/stations/invite', {}],
      ['/api/stations', { invitation: 'fdinv1_y', name: null }],
      ['/api/stations/a%20b%2Fc/remove', {}],
    ]);
  });
  test('malformed responses are refused instead of shown', () => {
    expect(() => invitationView({ invitation: '', expires_at: 1, join_command: 'x' })).toThrow();
    expect(() => invitationView({ invitation: 'a', expires_at: 1.5, join_command: 'x' })).toThrow();
    expect(() => invitationView(null)).toThrow();
    expect(() => stationResult({ name: 'x' })).toThrow();
    expect(() => stationResult([])).toThrow();
  });
  test('a blank name asks for the station name and labels stay explicit', () => {
    expect(requestedName('  ')).toBeNull();
    expect(requestedName(' box ')).toBe('box');
    expect(authLabel('signed_in')).toBe('signed in');
    expect(authLabel('signed_out')).toBe('signed out');
    expect(authLabel('anything')).toBe('sign-in unknown');
    expect(expiresText(0)).toBe('1970-01-01 00:00 UTC');
  });
});

describe('worktree commands (DL-150)', () => {
  const command = (receipt: unknown, state = 'done') => ({ state, receipt });
  const actorWith = (reply: () => unknown) => {
    const calls: [string, Record<string, unknown>][] = [];
    const act = worktreeActor(async (path, body) => {
      calls.push([path, body as Record<string, unknown>]);
      return reply();
    });
    return { act, calls };
  };

  test('the action posts the session, a new command id and the confirm name only when given', async () => {
    const { act, calls } = actorWith(() => command({ state: 'accepted' }));
    await act('Alpha box', 'trash', 's1');
    await act('Alpha box', 'purge', 's1', 'trash-s1');
    expect(calls.map(([path]) => path)).toEqual([
      '/api/stations/Alpha%20box/worktrees/trash',
      '/api/stations/Alpha%20box/worktrees/purge',
    ]);
    const [first, second] = calls.map(([, body]) => body);
    expect(Object.keys(first ?? {}).sort()).toEqual(['command_id', 'session_id']);
    expect(first?.['session_id']).toBe('s1');
    expect(second?.['confirm_name']).toBe('trash-s1');
    expect(first?.['command_id']).toMatch(/^b[0-9a-z]{24}$/);
    expect(first?.['command_id']).not.toBe(second?.['command_id']);
  });
  test('an accepted receipt says the work started, a rejected one gives the station message', async () => {
    const accepted = actorWith(() => command({ state: 'accepted' }));
    expect(await accepted.act('a', 'trash', 's')).toEqual({
      ok: true,
      text: 'Moving the worktree to the trash…',
    });
    expect((await accepted.act('a', 'restore', 's')).text).toBe('Restoring the worktree…');
    expect((await accepted.act('a', 'purge', 's')).text).toBe('Deleting the trashed folder…');
    expect((await accepted.act('a', 'release', 's')).text).toBe(
      'Fleet Deck no longer tracks this folder.',
    );
    const rejected = actorWith(() =>
      command({ state: 'rejected', reject: { code: 'invalid', message: 'The session is live.' } }),
    );
    expect(await rejected.act('a', 'trash', 's')).toEqual({
      ok: false,
      text: 'The session is live.',
    });
  });
  test('the other command states read like a session command', async () => {
    expect((await actorWith(() => command(null, 'pending')).act('a', 'trash', 's')).ok).toBe(true);
    const failed = await actorWith(() => ({ state: 'failed', reason: 'no link' })).act(
      'a',
      'trash',
      's',
    );
    expect(failed).toEqual({ ok: false, text: 'Not delivered: no link' });
    expect((await actorWith(() => 7).act('a', 'trash', 's')).ok).toBe(false);
  });
  test('a thrown error becomes its message', async () => {
    const act = worktreeActor(async () => {
      throw new Error('The station is not linked.');
    });
    expect(await act('a', 'trash', 's')).toEqual({ ok: false, text: 'The station is not linked.' });
  });
  test('the delete confirm needs the exact folder name', () => {
    expect(confirmMatches('trash-s1', '/w/.trash/trash-s1')).toBe(true);
    expect(confirmMatches('trash-s1', '/w/.trash/trash-s1/')).toBe(true);
    expect(confirmMatches('trash-s', '/w/.trash/trash-s1')).toBe(false);
    expect(confirmMatches(' trash-s1', '/w/.trash/trash-s1')).toBe(false);
    expect(confirmMatches('', '')).toBe(false);
  });
});
