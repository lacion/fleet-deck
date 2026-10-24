// R13 P1: auto-focus follows the request order only.
import { describe, expect, test } from 'bun:test';
import { type Ask, askKey, FOCUS_HOLD_S, initialAuto, nextAutoFocus } from './autofocus';

const ask = (ref: string, at: number): Ask => ({ ref, requestId: `r${at}`, at });
const A = ask('st/a', 100);
const B = ask('st/b', 200);
const HOLD = FOCUS_HOLD_S * 1000;

describe('nextAutoFocus', () => {
  test('a new request takes the wall to focus on its session', () => {
    const s = initialAuto('mission', null);
    const next = nextAutoFocus([], [A], 1000, s, false);
    expect(next.mode).toBe('focus');
    expect(next.session).toBe('st/a');
    expect(next.hold?.until).toBe(1000 + HOLD);
    expect(next.hold?.back).toEqual({ mode: 'mission', session: null });
  });

  test('a request that was already open is not new', () => {
    const s = initialAuto('mission', null);
    expect(nextAutoFocus([A], [A], 1000, s, false)).toBe(s);
  });

  test('the wall returns to its mode when the hold time ends', () => {
    const held = nextAutoFocus([], [A], 1000, initialAuto('race', null), false);
    const still = nextAutoFocus([A], [A], 1000 + HOLD - 1, held, false);
    expect(still.mode).toBe('focus');
    const back = nextAutoFocus([A], [A], 1000 + HOLD, still, false);
    expect(back.mode).toBe('race');
    expect(back.session).toBeNull();
    expect(back.hold).toBeNull();
  });

  test('it returns earlier when the request closes', () => {
    const held = nextAutoFocus([], [A], 1000, initialAuto('mission', null), false);
    const back = nextAutoFocus([A], [], 5000, held, false);
    expect(back.mode).toBe('mission');
    expect(back.hold).toBeNull();
  });

  test('two new requests: the oldest first, the next after the hold', () => {
    const held = nextAutoFocus([], [B, A], 1000, initialAuto('mission', null), false);
    expect(held.session).toBe('st/a');
    expect(held.queue.map((q) => q.requestId)).toEqual([B.requestId]);
    const second = nextAutoFocus([A, B], [A, B], 1000 + HOLD, held, false);
    expect(second.mode).toBe('focus');
    expect(second.session).toBe('st/b');
    expect(second.hold?.back).toEqual({ mode: 'mission', session: null });
    expect(second.queue).toEqual([]);
    const end = nextAutoFocus([A, B], [A, B], 1000 + 2 * HOLD, second, false);
    expect(end.mode).toBe('mission');
  });

  test('a queued request that closes is dropped', () => {
    const held = nextAutoFocus([], [A, B], 1000, initialAuto('mission', null), false);
    const next = nextAutoFocus([A, B], [A], 2000, held, false);
    expect(next.queue).toEqual([]);
    expect(next.session).toBe('st/a');
  });

  test('pin turns auto-focus off', () => {
    const s = initialAuto('mission', null);
    expect(nextAutoFocus([], [A], 1000, s, true)).toBe(s);
  });

  test('a change of view by the owner ends the hold', () => {
    const held = nextAutoFocus([], [A], 1000, initialAuto('mission', null), false);
    const moved = { ...held, mode: 'ambient' as const };
    const next = nextAutoFocus([A], [A], 2000, moved, false);
    expect(next.mode).toBe('ambient');
    expect(next.hold).toBeNull();
  });

  test('the same request id in two sessions is two requests', () => {
    const one: Ask = { ref: 'st/a', requestId: 'req-1', at: 100 };
    const two: Ask = { ref: 'st/b', requestId: 'req-1', at: 200 };
    const held = nextAutoFocus([], [one], 1000, initialAuto('mission', null), false);
    expect(held.session).toBe('st/a');
    // the second session opens later with the same id: it queues, then gets its own focus
    const queued = nextAutoFocus([one], [one, two], 2000, held, false);
    expect(queued.session).toBe('st/a');
    expect(queued.queue.map(askKey)).toEqual(['st/b/req-1']);
    // the first closes: the hold of the second starts
    const second = nextAutoFocus([one, two], [two], 3000, queued, false);
    expect(second.mode).toBe('focus');
    expect(second.session).toBe('st/b');
    expect(second.hold?.back).toEqual({ mode: 'mission', session: null });
  });

  test('closing one request does not end the hold of another with the same id', () => {
    const one: Ask = { ref: 'st/a', requestId: 'req-1', at: 100 };
    const two: Ask = { ref: 'st/b', requestId: 'req-1', at: 200 };
    const held = nextAutoFocus([], [one], 1000, initialAuto('mission', null), false);
    // the request of the other session closes: the hold of `st/a` stays
    const next = nextAutoFocus([one, two], [one], 2000, held, false);
    expect(next.mode).toBe('focus');
    expect(next.session).toBe('st/a');
    expect(next.hold?.ref).toBe('st/a');
  });

  test('a queued request closes and another opens in the same update', () => {
    const C = ask('st/c', 300);
    const held = nextAutoFocus([], [A, B], 1000, initialAuto('mission', null), false);
    expect(held.queue.map((q) => q.requestId)).toEqual([B.requestId]);
    const next = nextAutoFocus([A, B], [A, C], 2000, held, false);
    expect(next.session).toBe('st/a');
    expect(next.queue.map(askKey)).toEqual([askKey(C)]);
    const after = nextAutoFocus([A, C], [C], 3000, next, false);
    expect(after.session).toBe('st/c');
  });
});
