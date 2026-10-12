import { describe, expect, test } from 'bun:test';
import type { TaskView } from '../gen';
import { fleetActions, taskList, taskStatus } from './fleet';

const task = (extra: Partial<TaskView> = {}): TaskView => ({
  task_id: 't1',
  title: 'write the docs',
  creator: 'owner',
  station_id: 'st',
  session_id: 's1',
  state: 'open',
  result: null,
  delivery: 'queued',
  reason: null,
  created_at: 1,
  updated_at: 1,
  completed_at: null,
  ...extra,
});

describe('fleet mail and tasks', () => {
  test('a mail and a task post one request each with a new id', async () => {
    const posts: [string, unknown][] = [];
    const a = fleetActions(
      async () => [],
      async (path, body) => {
        posts.push([path, body]);
        return {};
      },
      () => 'id1',
    );
    const to = { stationId: 'st', sessionId: 's1' };
    expect(await a.mail(to, 'rebase', true)).toEqual({ ok: true, text: 'Mail queued.' });
    expect(await a.task(to, 'write the docs')).toEqual({ ok: true, text: 'Task created.' });
    expect(posts).toEqual([
      [
        '/api/mail',
        { mail_id: 'id1', station: 'st', session_id: 's1', text: 'rebase', delivery: 'steer' },
      ],
      ['/api/tasks', { task_id: 'id1', title: 'write the docs', station: 'st', session_id: 's1' }],
    ]);
  });

  test('a refused request says why', async () => {
    const a = fleetActions(
      async () => {
        throw new Error('offline');
      },
      async () => {
        throw new Error('unknown_session');
      },
    );
    const to = { stationId: 'st', sessionId: 's1' };
    expect(await a.mail(to, 'x', false)).toEqual({
      ok: false,
      text: 'Mail not sent: unknown_session',
    });
    expect(await a.tasks()).toEqual([]);
  });

  test('the task status shows the end, else the delivery', () => {
    expect(taskStatus(task())).toBe('Open, not yet delivered');
    expect(taskStatus(task({ delivery: 'delivered' }))).toBe('Open');
    expect(taskStatus(task({ delivery: 'dead', reason: 'expired' }))).toBe(
      'Not delivered: expired',
    );
    expect(taskStatus(task({ state: 'done', result: 'merged' }))).toBe('Done: merged');
    expect(taskStatus(task({ state: 'failed' }))).toBe('Failed');
    expect(taskList([task(), { nope: 1 }, 3])).toHaveLength(1);
    expect(taskList({})).toEqual([]);
  });
});
