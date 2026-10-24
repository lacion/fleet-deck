import { describe, expect, test } from 'bun:test';
import {
  type OmarchyActions,
  type OmarchyActResult,
  type OmarchyStatus,
  runAction,
} from './omarchy';
import { unusedReads } from './omarchyFixtures';
import { type Gate, gateActions, type WatchEnv, watchStatus } from './OmarchyCard';

const status = (): OmarchyStatus => ({
  omarchy: { present: true, version: '3.1.0' },
  install: null,
  screensaver: null,
  provision: null,
  running: null,
});

const done = (): OmarchyActResult => ({
  outcome: 'done',
  code: null,
  message: null,
  status: status(),
});

/** A promise that the test settles by hand. */
type Deferred<T> = {
  readonly promise: Promise<T>;
  readonly resolve: (v: T) => void;
  readonly reject: (e: Error) => void;
};

function deferred<T>(): Deferred<T> {
  let resolve: (v: T) => void = () => undefined;
  let reject: (e: Error) => void = () => undefined;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const tick = () => new Promise<void>((r) => setTimeout(r, 0));

/** The raw calls: each call is logged and waits for the test to settle it. */
function rig() {
  const log: string[] = [];
  const reads: Array<Deferred<OmarchyStatus>> = [];
  const posts: Array<Deferred<OmarchyActResult>> = [];
  const raw: OmarchyActions = {
    status: () => {
      log.push('read');
      const d = deferred<OmarchyStatus>();
      reads.push(d);
      return d.promise;
    },
    act: (_station, action) => {
      log.push(`post ${action}`);
      const d = deferred<OmarchyActResult>();
      posts.push(d);
      return d.promise;
    },
    ...unusedReads,
  };
  const gate: Gate = { current: Promise.resolve() };
  return { log, reads, posts, gated: gateActions(raw, gate) };
}

describe('the request gate of the card', () => {
  test('an action started while a status read is unresolved is sent after that read', async () => {
    const r = rig();
    const read = r.gated.status('alpha');
    const acted = runAction(r.gated, 'alpha', 'install', async () => 'tok');
    await tick();
    expect(r.log).toEqual(['read']);
    r.reads[0]?.resolve(status());
    await read;
    await tick();
    expect(r.log).toEqual(['read', 'post install']);
    r.posts[0]?.resolve(done());
    expect((await acted).kind).toBe('done');
  });

  test('two actions in a row are sent one after the other', async () => {
    const r = rig();
    const first = runAction(r.gated, 'alpha', 'install', async () => 'tok');
    const second = runAction(r.gated, 'alpha', 'uninstall', async () => 'tok');
    await tick();
    expect(r.log).toEqual(['post install']);
    r.posts[0]?.resolve(done());
    await first;
    await tick();
    expect(r.log).toEqual(['post install', 'post uninstall']);
    r.posts[1]?.resolve(done());
    await second;
  });

  test('a failed call rejects for its caller only; the next call is still sent', async () => {
    const r = rig();
    const bad = r.gated.status('alpha').then(
      () => 'ok',
      () => 'rejected',
    );
    const next = r.gated.status('alpha');
    await tick();
    expect(r.log).toEqual(['read']);
    r.reads[0]?.reject(new Error('down'));
    expect(await bad).toBe('rejected');
    await tick();
    expect(r.log).toEqual(['read', 'read']);
    r.reads[1]?.resolve(status());
    expect((await next).omarchy.present).toBe(true);
  });

  test('the refresh sends no read while an action is queued or running', async () => {
    const r = rig();
    let t = 0;
    let ticks = 0;
    const env: WatchEnv = {
      sleep: async (ms) => {
        t += ms;
        ticks += 1;
        await tick();
      },
      now: () => t,
      // 12 ticks of 5 s pass: the watch would read at 15 s, 30 s and 45 s.
      stopped: () => ticks >= 12,
      paused: () => true,
      known: () => status(),
      turn: () => 0,
      onStatus: () => undefined,
    };
    const acted = runAction(r.gated, 'alpha', 'install', async () => 'tok');
    await watchStatus(r.gated, 'alpha', env);
    expect(r.log).toEqual(['post install']);
    r.posts[0]?.resolve(done());
    await acted;
  });
});

describe('a done action with warnings', () => {
  test('runAction keeps the warnings of the station', async () => {
    const warned: OmarchyActResult = {
      ...done(),
      message: 'fleetdeck: warning: plugin not enabled',
    };
    const actions = {
      status: async () => status(),
      act: async () => warned,
    } as unknown as OmarchyActions;
    expect(await runAction(actions, 'a', 'install', async () => 't')).toEqual({
      kind: 'done',
      status: warned.status,
      message: 'fleetdeck: warning: plugin not enabled',
    });
  });
});
