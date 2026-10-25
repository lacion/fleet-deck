// G4: the board follows a pending command with `GET /api/commands/<id>`.
import { describe, expect, test } from 'bun:test';
import {
  FOLLOW_LIMIT_MS,
  finalOutcome,
  followCommand,
  type Outcome,
  onConfirmed,
  sessionActions,
} from './commands';

const pending = { state: 'pending', command_id: 'c1' };
const done = { state: 'done', command_id: 'c1', receipt: { state: 'accepted' } };
const refused = {
  state: 'done',
  command_id: 'c1',
  receipt: { state: 'rejected', reject: { code: 'unknown_request', message: 'no such request' } },
};

/** A reader that answers each call with the next item; an `Error` item is thrown. */
function reader(items: readonly unknown[]) {
  const paths: string[] = [];
  const get = async (path: string): Promise<unknown> => {
    paths.push(path);
    const item = items[paths.length - 1];
    if (item instanceof Error) throw item;
    return item;
  };
  return { get, paths };
}

/** A wait that records the ms and resolves at once. */
function waits() {
  const ms: number[] = [];
  return {
    ms,
    wait: async (n: number) => {
      ms.push(n);
    },
  };
}

const P: Outcome = { ok: true, text: 'Sent. The station has not confirmed it yet.' };
const D: Outcome = { ok: true, text: 'Done.' };
const R: Outcome = { ok: false, text: 'Refused: x' };

describe('followCommand (G4)', () => {
  test('a pending command is read until the station refuses it', async () => {
    const r = reader([pending, pending, refused]);
    const w = waits();
    const out = await followCommand(r.get, 'c1', new AbortController().signal, w.wait);
    expect(out).toEqual({ ok: false, text: 'Refused: no such request' });
    expect(r.paths).toEqual(['/api/commands/c1', '/api/commands/c1', '/api/commands/c1']);
    expect(w.ms).toEqual([1000, 1000, 1000]);
  });

  test('the reads slow down after ten', async () => {
    const r = reader([...Array.from({ length: 11 }, () => pending), done]);
    const w = waits();
    const out = await followCommand(r.get, 'c1', new AbortController().signal, w.wait);
    expect(out).toEqual({ ok: true, text: 'Done.' });
    expect(w.ms[9]).toBe(1000);
    expect(w.ms[10]).toBe(5000);
  });

  test('an abort stops the follow', async () => {
    const ctl = new AbortController();
    const paths: string[] = [];
    const get = async (path: string): Promise<unknown> => {
      paths.push(path);
      ctl.abort();
      return pending;
    };
    const w = waits();
    expect(await followCommand(get, 'c1', ctl.signal, w.wait)).toBeNull();
    expect(paths).toHaveLength(1);
  });

  test('three failed reads in a row stop the follow; a good read resets the count', async () => {
    const boom = new Error('network');
    const a = reader([boom, boom, done]);
    expect(await followCommand(a.get, 'c1', new AbortController().signal, waits().wait)).toEqual({
      ok: true,
      text: 'Done.',
    });
    const b = reader([boom, boom, boom, done]);
    expect(await followCommand(b.get, 'c1', new AbortController().signal, waits().wait)).toBeNull();
    expect(b.paths).toHaveLength(3);
  });

  test('the follow stops after 120 s', async () => {
    const r = reader([pending, pending]);
    let t = 0;
    const now = () => t;
    const wait = async () => {
      t += FOLLOW_LIMIT_MS + 1000;
    };
    expect(await followCommand(r.get, 'c1', new AbortController().signal, wait, now)).toBeNull();
    expect(r.paths).toHaveLength(0);
  });
});

describe('sessionActions follow-up (G4)', () => {
  const key = { station_id: 'station-a', session_id: 's1' };
  const make = (reply: unknown, get: ((path: string) => Promise<unknown>) | null) =>
    sessionActions(
      async () => reply,
      key,
      ['drive', 'approve'],
      () => 'c1',
      get,
    );

  test('sessionActions follows only a pending answer and only with a reader', async () => {
    const get = async () => done;
    const withGet = await make(pending, get).interrupt();
    expect(typeof withGet.later).toBe('function');
    const noGet = await make(pending, null).interrupt();
    expect(noGet.later).toBeUndefined();
    const final = await make(done, get).interrupt();
    expect(final.later).toBeUndefined();
  });
});

describe('finalOutcome (G4)', () => {
  const signal = new AbortController().signal;

  test('finalOutcome gives a final outcome as is and waits for a provisional one', async () => {
    expect(await finalOutcome(D, signal)).toBe(D);
    expect(await finalOutcome({ ...P, later: async () => R }, signal)).toBe(R);
    expect(await finalOutcome({ ...P, later: async () => null }, signal)).toBeNull();
  });
});

describe('onConfirmed (G4)', () => {
  test('onConfirmed runs its action only on a confirmed success', async () => {
    const signal = new AbortController().signal;

    let ran = 0;
    const final = onConfirmed(D, () => {
      ran += 1;
    });
    expect(final).toBe(D);
    expect(ran).toBe(1);

    ran = 0;
    expect(onConfirmed(R, () => (ran += 1))).toBe(R);
    expect(ran).toBe(0);

    ran = 0;
    const toDone = onConfirmed({ ...P, later: async () => D }, () => {
      ran += 1;
    });
    expect(ran).toBe(0);
    expect(await toDone.later?.(signal)).toBe(D);
    expect(ran).toBe(1);

    ran = 0;
    const toRefused = onConfirmed({ ...P, later: async () => R }, () => {
      ran += 1;
    });
    expect(await toRefused.later?.(signal)).toBe(R);
    expect(ran).toBe(0);

    ran = 0;
    const toNull = onConfirmed({ ...P, later: async () => null }, () => {
      ran += 1;
    });
    expect(await toNull.later?.(signal)).toBeNull();
    expect(ran).toBe(0);

    ran = 0;
    const ctl = new AbortController();
    const aborted = onConfirmed(
      {
        ...P,
        later: async () => {
          ctl.abort();
          return D;
        },
      },
      () => {
        ran += 1;
      },
    );
    await aborted.later?.(ctl.signal);
    expect(ran).toBe(0);
  });
});
