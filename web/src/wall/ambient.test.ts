// R13 P1: the ambient offset is a pure function of the minute.
import { describe, expect, test } from 'bun:test';
import { AMBIENT_MAX_PX, AMBIENT_STEP_MIN, ambientOffset } from './ambient';

describe('ambientOffset', () => {
  test('the same minute gives the same offset', () => {
    expect(ambientOffset(12345)).toEqual(ambientOffset(12345));
  });

  test('it moves every 5 minutes and not before', () => {
    expect(ambientOffset(0)).toEqual(ambientOffset(AMBIENT_STEP_MIN - 1));
    expect(ambientOffset(AMBIENT_STEP_MIN)).not.toEqual(ambientOffset(0));
  });

  test('it stays within 8 px on each axis, over a whole day', () => {
    for (let minute = 0; minute < 24 * 60; minute += 1) {
      const o = ambientOffset(minute);
      expect(Math.abs(o.x)).toBeLessThanOrEqual(AMBIENT_MAX_PX);
      expect(Math.abs(o.y)).toBeLessThanOrEqual(AMBIENT_MAX_PX);
    }
  });

  test('a bad minute is the first spot', () => {
    expect(ambientOffset(-10)).toEqual(ambientOffset(0));
  });
});
