// Test helper: a fake clock with timers that the test advances by hand.
import { describe, expect, test } from 'bun:test';
import type { Timers } from './flow';

type Entry = { readonly id: number; readonly at: number; readonly fn: () => void };

/** Real macrotask: lets the promise chains of the controllers run. */
export const flush = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

export class FakeTime {
  now = 1_700_000_000_000;
  private next = 1;
  private entries: Entry[] = [];

  readonly timers: Timers = {
    set: (fn, ms) => {
      const id = this.next++;
      this.entries.push({ id, at: this.now + ms, fn });
      return id;
    },
    clear: (handle) => {
      this.entries = this.entries.filter((e) => e.id !== handle);
    },
  };

  readonly clock = (): number => this.now;

  get pending(): number {
    return this.entries.length;
  }

  /** Moves the clock by `ms`, running the timers that fall due, and lets the promises settle. */
  async advance(ms: number): Promise<void> {
    const target = this.now + ms;
    for (;;) {
      const due = this.entries.filter((e) => e.at <= target).sort((a, b) => a.at - b.at)[0];
      if (due === undefined) break;
      this.entries = this.entries.filter((e) => e.id !== due.id);
      this.now = Math.max(this.now, due.at);
      due.fn();
      await flush();
    }
    this.now = target;
    await flush();
  }
}

describe('FakeTime', () => {
  test('runs the timers that fall due, in order, and clears one', async () => {
    const time = new FakeTime();
    const seen: number[] = [];
    time.timers.set(() => seen.push(2), 200);
    time.timers.set(() => seen.push(1), 100);
    const gone = time.timers.set(() => seen.push(3), 150);
    time.timers.clear(gone);
    await time.advance(250);
    expect(seen).toEqual([1, 2]);
    expect(time.pending).toBe(0);
  });
});
