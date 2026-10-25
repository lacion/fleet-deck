import { describe, expect, test } from 'bun:test';
import { newUlid } from './ulid';

const ALPHABET = /^[0-9A-HJKMNP-TV-Z]{26}$/;

describe('the capture ULID', () => {
  test('26 characters of the Crockford alphabet, uppercase, first character 0-7', () => {
    for (let i = 0; i < 50; i += 1) {
      const id = newUlid();
      expect(id).toMatch(ALPHABET);
      expect(id.length).toBe(26);
      expect('01234567').toContain(id.charAt(0));
    }
  });

  test('the time prefix grows with the time', () => {
    const fill = (b: Uint8Array) => b.fill(0);
    const a = newUlid(1_700_000_000_000, fill);
    const b = newUlid(1_700_000_000_001, fill);
    const c = newUlid(1_800_000_000_000, fill);
    expect(a.slice(0, 10) < b.slice(0, 10)).toBe(true);
    expect(b.slice(0, 10) < c.slice(0, 10)).toBe(true);
    expect(a.slice(10)).toBe('0'.repeat(16));
  });

  test('a known time gives the known prefix, the random bytes fill the rest', () => {
    // 2^48 - 1 is the largest time: "7ZZZZZZZZZ".
    expect(newUlid(2 ** 48 - 1, (b) => b.fill(255)).startsWith('7ZZZZZZZZZ')).toBe(true);
    expect(newUlid(0, (b) => b.fill(255)).slice(10)).toBe('Z'.repeat(16));
    expect(newUlid(0, (b) => b.fill(0))).toBe('0'.repeat(26));
  });

  test('two ULIDs of the same millisecond differ in the random part', () => {
    expect(newUlid(5).slice(10)).not.toBe(newUlid(5).slice(10));
  });
});
