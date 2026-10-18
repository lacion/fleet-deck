import { describe, expect, test } from 'bun:test';
import type { TaskView } from '../gen';
import { station } from '../testing/fleet';
import {
  desktopOpList,
  fleetActions,
  grantList,
  grantStations,
  grantStatus,
  nextSends,
  type SendEntry,
  type SendIntent,
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
  completion_reason: null,
  question: null,
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
    expect(taskStatus(task({ state: 'blocked' }))).toBe('Blocked');
    expect(
      taskStatus(task({ state: 'blocked', completion_reason: 'the worker session ended' })),
    ).toBe('Blocked: the worker session ended');
    expect(taskStatus(task({ state: 'question', question: 'which key?' }))).toBe(
      'Asks: which key?',
    );
    expect(taskStatus(task({ state: 'question' }))).toBe('Asks a question');
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
  test('the stations of a grant show by name, the own station first, only with targets', () => {
    const stations = [station('station-a'), station('station-b')];
    const own = { ...grant, station_id: 'station-a', stations: ['station-a'] };
    expect(grantStations(own, stations)).toBe('');
    expect(grantStations({ ...own, stations: ['station-b', 'station-a'] }, stations)).toBe(
      'Stations: Alpha, Beta',
    );
    // A station that is not in the list shows by its id.
    expect(grantStations({ ...own, stations: ['station-a', 'gone'] }, stations)).toBe(
      'Stations: Alpha, gone',
    );
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

describe('desktop usage and operations (R13 P6)', () => {
  const grant = {
    grant_id: 'g1',
    station_id: 'st',
    session_id: 'coord-1',
    classes: ['read', 'desktop', 'capture'],
    stations: ['st'],
    spawn_cap: 0,
    spawn_used: 0,
    state: 'active',
    revoked_reason: null,
    generation: 1,
    expires_at: 10,
    last_used_at: null,
  };
  test('grantStatus appends the usage of each limit that is present', () => {
    expect(grantStatus({ ...grant, desktop_cap: 300, desktop_used: 4 })).toBe(
      'read, desktop, capture, desktop actions 4 of 300: active',
    );
    expect(
      grantStatus({
        ...grant,
        desktop_cap: 300,
        desktop_used: 4,
        capture_cap: 20,
        capture_used: 2,
        image_byte_cap: 67_108_864,
        image_byte_used: 1_572_864,
      }),
    ).toBe(
      'read, desktop, capture, desktop actions 4 of 300, screenshots 2 of 20, image reads 1.5 of 64 MiB: active',
    );
    expect(
      grantStatus({
        ...grant,
        classes: ['read'],
        image_byte_cap: 67_108_864,
        image_byte_used: 0,
      }),
    ).toBe('read, image reads 0 of 64 MiB: active');
  });
  test('caps posts the body, and the header only when an elevation is given', async () => {
    const posts: { path: string; body: unknown; headers: unknown }[] = [];
    const a = fleetActions(
      async () => [],
      async (path, body, headers) => {
        posts.push({ path, body, headers });
        return {};
      },
    );
    expect(await a.caps('g/1', { desktop_cap: 5 })).toEqual({ ok: true, text: 'Limits changed.' });
    expect(await a.caps('g/1', { desktop_cap: 9, capture_cap: 1 }, 'code-1')).toEqual({
      ok: true,
      text: 'Limits changed.',
    });
    expect(posts).toEqual([
      { path: '/api/coordinator/grants/g%2F1/caps', body: { desktop_cap: 5 }, headers: undefined },
      {
        path: '/api/coordinator/grants/g%2F1/caps',
        body: { desktop_cap: 9, capture_cap: 1 },
        headers: { 'x-fleetdeck-elevation': 'code-1' },
      },
    ]);
  });
  test('a refused caps call says so', async () => {
    const bad = fleetActions(
      async () => [],
      async () => {
        throw new Error('400 elevation_required');
      },
    );
    expect(await bad.caps('g1', { desktop_cap: 5 })).toEqual({
      ok: false,
      text: 'Change limits: 400 elevation_required',
    });
  });
  const row = {
    operation_id: 'o1',
    tool: 'notify',
    target_station: 'st',
    state: 'done',
    code: null,
    created_at: 2,
    updated_at: 3,
  };
  test('desktopOps reads the rows and drops anything malformed', async () => {
    const paths: string[] = [];
    const a = fleetActions(
      async (path) => {
        paths.push(path);
        return [row, { nope: 1 }, 'x', { ...row, state: 3 }, { ...row, code: 5 }];
      },
      async () => ({}),
    );
    expect(await a.desktopOps('g/1')).toEqual([row]);
    expect(paths).toEqual(['/api/coordinator/grants/g%2F1/desktop-ops']);
    expect(desktopOpList({})).toEqual([]);
    const down = fleetActions(
      async () => {
        throw new Error('down');
      },
      async () => ({}),
    );
    expect(await down.desktopOps('g1')).toEqual([]);
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

  const intent = (extra: Partial<SendIntent> = {}): SendIntent => ({
    text: 'hi',
    steer: false,
    audience: 'all',
    selection: '',
    ...extra,
  });
  /** A first send to a and b where a was sent and b failed. */
  const afterFirst = (): { after: SendRecord; b: SendEntry } => {
    const first = nextSends(null, intent(), [target('a'), target('b')], ids()).record;
    const [a, b] = first.entries;
    if (a === undefined || b === undefined) throw new Error('two entries');
    return { after: { ...first, entries: [{ ...a, state: 'sent' }, b] }, b };
  };

  test('a new send gets a new id for each target', () => {
    const { record, todo } = nextSends(null, intent(), [target('a'), target('b')], ids());
    expect(todo.map((e) => [e.ref, e.mailId, e.state])).toEqual([
      ['a', 'id1', 'failed'],
      ['b', 'id2', 'failed'],
    ]);
    expect(record.entries).toEqual(todo);
    expect(record.intent).toEqual(intent());
  });

  test('the same intent sends only the failed entries, with their old ids', () => {
    const { after, b } = afterFirst();
    const again = nextSends(after, intent(), [target('b'), target('a')], ids());
    expect(again.record).toBe(after);
    expect(again.todo).toEqual([b]);
    expect(again.todo[0]?.mailId).toBe('id2');
  });

  test('a session that appears between the attempts is not sent', () => {
    const { after, b } = afterFirst();
    const again = nextSends(after, intent(), [target('a'), target('b'), target('c')], ids());
    expect(again.record).toBe(after);
    expect(again.todo).toEqual([b]);
    expect(again.todo[0]?.mailId).toBe('id2');
  });

  test('a failed session that leaves the targets is still retried with its old id', () => {
    const { after, b } = afterFirst();
    const again = nextSends(after, intent(), [target('a')], ids());
    expect(again.record).toBe(after);
    expect(again.todo).toEqual([b]);
    expect(again.todo[0]?.mailId).toBe('id2');
  });

  test('a changed text, steer flag, audience or selection starts a new record with new ids', () => {
    const { after } = afterFirst();
    const targets = [target('a'), target('c')];
    for (const changed of [
      { text: 'other' },
      { steer: true },
      { audience: 'station' },
      { selection: 'st' },
    ] as const) {
      const next = nextSends(after, intent(changed), targets, ids());
      expect(next.record).not.toBe(after);
      expect(next.todo.map((e) => [e.ref, e.mailId])).toEqual([
        ['a', 'id1'],
        ['c', 'id2'],
      ]);
    }
  });
});
