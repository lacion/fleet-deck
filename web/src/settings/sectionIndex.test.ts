// The spy of the section index: the section in view is the last one that starts at or above the
// reading line, not the first one that still shows its tail.
import { describe, expect, test } from 'bun:test';
import { currentIndex, readingLine } from './SectionIndex';

describe('the section in view', () => {
  test('the last section whose top is at or above the reading line', () => {
    // The tail of the first section is still on screen; the second one starts above the line.
    expect(currentIndex([-400, 10, 600], 300)).toBe(1);
    expect(currentIndex([-400, -10, 300], 300)).toBe(2);
  });

  test('before the first section reaches the line, the first one', () => {
    expect(currentIndex([320, 900], 300)).toBe(0);
    expect(currentIndex([], 300)).toBe(0);
  });

  test('a section that is not on the page does not count', () => {
    expect(currentIndex([-800, Number.POSITIVE_INFINITY, -100], 300)).toBe(2);
  });

  test('the line sits near the top, so a short section at the top stays current', () => {
    expect(readingLine(900)).toBe(120);
    expect(readingLine(300)).toBe(99);
    // Omarchy at the top (24px), Desktop tools 230px under it: Omarchy.
    expect(currentIndex([-600, 24, 254], readingLine(900))).toBe(1);
  });

  test('at the end of the scroll, the last section that shows on the screen', () => {
    expect(currentIndex([-900, 300, 620], 120)).toBe(0);
    expect(currentIndex([-900, 300, 620], 120, { height: 900 })).toBe(2);
    expect(currentIndex([-900, 300, 980], 120, { height: 900 })).toBe(1);
  });
});
