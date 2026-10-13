import { describe, expect, test } from 'bun:test';
import type { TaskView } from '../gen';
import {
  fleetActions,
  grantList,
  grantStatus,
  nextSends,
  type SendRecord,
  type SendTarget,
  taskList,
  taskStatus,
} from './fleet';

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

  test('a mail with an explicit mailId posts it, else a new id', async () => {
    const posts: unknown[] = [];
    const a = fleetActions(
      async () => [],
      async (_path, body) => {
        posts.push(body);
        return {};
      },
      () => 'new1',
    );
    const to = { stationId: 'st', sessionId: 's1' };
    await a.mail(to, 'rebase', false, 'old7');
    await a.mail(to, 'rebase', false);
    expect(posts).toMatchObject([{ mail_id: 'old7' }, { mail_id: 'new1' }]);
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

describe('coordinator grants', () => {
  const grant = {
    grant_id: 'g1',
    station_id: 'st',
    session_id: 'coord-1',
    classes: ['read', 'mail', 'spawn'],
    stations: ['st'],
    spawn_cap: 3,
    spawn_used: 1,
    state: 'active',
    revoked_reason: null,
    generation: null,
    expires_at: 10,
    last_used_at: null,
  };
  test('the list keeps grants only and the status says what the grant may do', async () => {
    expect(grantList([grant, { nope: 1 }, 'x'])).toEqual([grant]);
    expect(grantList({})).toEqual([]);
    expect(grantStatus(grant)).toBe('read, mail, spawn, spawned 1 of 3: waiting for its session');
    expect(grantStatus({ ...grant, classes: ['read'], generation: 1 })).toBe('read: active');
  });
  test('a revoke posts to the grant and a failure says so', async () => {
    const posts: string[] = [];
    const ok = fleetActions(
      async () => [grant],
      async (path) => {
        posts.push(path);
        return {};
      },
    );
    expect(await ok.grants()).toEqual([grant]);
    expect(await ok.revoke('g/1')).toEqual({ ok: true, text: 'Grant revoked.' });
    expect(posts).toEqual(['/api/coordinator/grants/g%2F1/revoke']);
    const bad = fleetActions(
      async () => {
        throw new Error('down');
      },
      async () => {
        throw new Error('403 forbidden');
      },
    );
    expect(await bad.grants()).toEqual([]);
    expect(await bad.revoke('g1')).toEqual({ ok: false, text: 'Grant not revoked: 403 forbidden' });
  });
  test('a renew posts to the grant and a refusal says so (ADR-006 25b)', async () => {
    const posts: string[] = [];
    const ok = fleetActions(
      async () => [grant],
      async (path) => {
        posts.push(path);
        return {};
      },
    );
    expect((await ok.renew('g/1')).ok).toBe(true);
    expect(posts).toEqual(['/api/coordinator/grants/g%2F1/renew']);
    const bad = fleetActions(
      async () => [],
      async () => {
        throw new Error('409 not_renewable');
      },
    );
    expect(await bad.renew('g1')).toEqual({
      ok: false,
      text: 'Grant not renewed: 409 not_renewable',
    });
  });
});

describe('the record of the last mail send', () => {
  const target = (ref: string): SendTarget => ({
    ref,
    target: { stationId: 'st', sessionId: ref },
  });
  const ids = () => {
    let n = 0;
    return () => `id${++n}`;
  };

  test('a new send gets a new id for each target', () => {
    const { record, todo } = nextSends(null, [target('a'), target('b')], 'hi', false, ids());
    expect(todo.map((e) => [e.ref, e.mailId, e.state])).toEqual([
      ['a', 'id1', 'failed'],
      ['b', 'id2', 'failed'],
    ]);
    expect(record.entries).toEqual(todo);
  });

  test('the same input sends only the failed entries, with their old ids', () => {
    const first = nextSends(null, [target('a'), target('b')], 'hi', false, ids()).record;
    const [a, b] = first.entries;
    if (a === undefined || b === undefined) throw new Error('two entries');
    const after: SendRecord = { ...first, entries: [{ ...a, state: 'sent' }, b] };
    // the order of the targets does not matter
    const again = nextSends(after, [target('b'), target('a')], 'hi', false, ids());
    expect(again.record).toBe(after);
    expect(again.todo).toEqual([b]);
    expect(again.todo[0]?.mailId).toBe('id2');
  });

  test('a changed text, steer flag or target set starts a new record with new ids', () => {
    const first = nextSends(null, [target('a'), target('b')], 'hi', false, ids()).record;
    for (const [targets, text, steer] of [
      [[target('a'), target('b')], 'other', false],
      [[target('a'), target('b')], 'hi', true],
      [[target('a')], 'hi', false],
      [[target('a'), target('c')], 'hi', false],
    ] as const) {
      const next = nextSends(first, targets, text, steer, ids());
      expect(next.record).not.toBe(first);
      expect(next.todo).toHaveLength(targets.length);
      expect(next.todo.map((e) => e.mailId)).toEqual(targets.map((_, i) => `id${i + 1}`));
    }
  });
});
