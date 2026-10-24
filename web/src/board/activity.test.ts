import { describe, expect, test } from 'bun:test';
import { card } from '../testing/fleet';
import { buckets, record } from './activity';

const MINUTE = 60_000;
const T0 = 1_760_000_000_000 - (1_760_000_000_000 % MINUTE);

const at = (station: string, seq: number) => card('s', 'working', station, { session_seq: seq });
const sum = (values: readonly number[]) => values.reduce((a, b) => a + b, 0);

describe('card activity buckets', () => {
  test('a session with no sight has 20 empty buckets', () => {
    const values = buckets('nobody/s', T0);
    expect(values).toHaveLength(20);
    expect(sum(values)).toBe(0);
  });
  test('the first sight stores the seq and counts nothing', () => {
    record([at('st-first', 40)], T0);
    expect(sum(buckets('st-first/s', T0))).toBe(0);
  });
  test('growth goes into the bucket of the current minute, the last one', () => {
    record([at('st-grow', 10)], T0);
    record([at('st-grow', 14)], T0 + 5_000);
    record([at('st-grow', 15)], T0 + 20_000);
    const values = buckets('st-grow/s', T0 + 30_000);
    expect(values[19]).toBe(5);
    expect(sum(values)).toBe(5);
    record([at('st-grow', 17)], T0 + MINUTE);
    const next = buckets('st-grow/s', T0 + MINUTE);
    expect(next[18]).toBe(5);
    expect(next[19]).toBe(2);
  });
  test('the ring rolls: a count older than 20 minutes is gone', () => {
    record([at('st-roll', 1)], T0);
    record([at('st-roll', 4)], T0);
    expect(buckets('st-roll/s', T0 + 19 * MINUTE)[0]).toBe(3);
    expect(sum(buckets('st-roll/s', T0 + 20 * MINUTE))).toBe(0);
  });
  test('a decreasing seq counts 0 and the new seq is the base', () => {
    record([at('st-down', 10)], T0);
    record([at('st-down', 4)], T0);
    expect(sum(buckets('st-down/s', T0))).toBe(0);
    record([at('st-down', 6)], T0);
    expect(buckets('st-down/s', T0)[19]).toBe(2);
  });
});
