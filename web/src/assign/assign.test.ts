import { describe, expect, test } from 'bun:test';
import { assignActions, assignResult } from './assign';

describe('assign auto', () => {
  test('an assigned task names the callsign; a pending one says sent', () => {
    const base = {
      outcome: 'assigned',
      station_id: 'st',
      session_id: 's1',
      callsign: 'otter-ab12',
      command_id: 'c1',
    };
    expect(assignResult({ ...base, state: 'done' })).toEqual({
      ok: true,
      text: 'Assigned to otter-ab12.',
      spawnOn: null,
    });
    expect(assignResult({ ...base, callsign: null, state: 'pending' }).text).toBe('Sent to s1.');
  });

  test('an unrouted task offers the best station, or says that none fits', () => {
    const r = assignResult({
      outcome: 'unrouted',
      reason: 'x',
      spawn_on: { station_id: 'st-a', name: 'Alpha' },
    });
    expect(r.ok).toBe(false);
    expect(r.spawnOn).toEqual({ stationId: 'st-a', name: 'Alpha' });
    expect(assignResult({ outcome: 'unrouted', reason: 'x', spawn_on: null }).text).toBe(
      'No session fits, and no station can start a new one.',
    );
    expect(assignResult({ nope: 1 }).ok).toBe(false);
  });

  test('a call posts one command id, the text, and the repo only when given', async () => {
    const calls: [string, unknown][] = [];
    const actions = assignActions(
      async (path, body) => {
        calls.push([path, body]);
        return { outcome: 'unrouted', reason: 'x', spawn_on: null };
      },
      () => 'id1',
    );
    await actions.assign('task', '  ');
    await actions.assign('task', '/p/app');
    expect(calls).toEqual([
      ['/api/assign', { command_id: 'id1', text: 'task' }],
      ['/api/assign', { command_id: 'id1', text: 'task', repo: '/p/app' }],
    ]);
    const failing = assignActions(async () => {
      throw new Error('offline');
    });
    expect((await failing.assign('t', '')).text).toBe('Not assigned: offline');
  });
});
