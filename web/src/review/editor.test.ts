import { describe, expect, test } from 'bun:test';
import { editorTitle, nextEditor, openEditor, rangeTooLong, spotLabel } from './editor';

const spot = (line: number, path = 'a.ts', side: 'new' | 'old' = 'new') => ({ side, path, line });

describe('nextEditor', () => {
  test('a click opens the editor on that line', () => {
    expect(nextEditor(null, spot(5), false)).toEqual({
      side: 'new',
      path: 'a.ts',
      first: 5,
      start: 5,
      end: 5,
    });
  });

  test('a second click on the same file and side makes a range, upward or downward', () => {
    const open = openEditor(spot(5));
    const down = nextEditor(open, spot(8), false);
    expect([down?.start, down?.end]).toEqual([5, 8]);
    // The range always runs from the first line: a click above makes the range 3..5.
    const up = nextEditor(down, spot(3), false);
    expect([up?.start, up?.end]).toEqual([3, 5]);
    // A click on the first line is a single line again.
    const back = nextEditor(up, spot(5), false);
    expect([back?.start, back?.end]).toEqual([5, 5]);
  });

  test('another file moves an empty editor and leaves a filled one', () => {
    const open = openEditor(spot(5));
    expect(nextEditor(open, spot(2, 'b.ts'), false)).toEqual(openEditor(spot(2, 'b.ts')));
    expect(nextEditor(open, spot(2, 'b.ts'), true)).toBe(open);
  });

  test('the other side of the same file is another place', () => {
    const open = openEditor(spot(5));
    expect(nextEditor(open, spot(2, 'a.ts', 'old'), false)).toEqual(
      openEditor(spot(2, 'a.ts', 'old')),
    );
    expect(nextEditor(open, spot(2, 'a.ts', 'old'), true)).toBe(open);
  });
});

describe('the range limit', () => {
  test('200 lines are too many, 199 are not', () => {
    const open = openEditor(spot(1));
    expect(rangeTooLong(nextEditor(open, spot(200), false) ?? open)).toBe(false);
    expect(rangeTooLong(nextEditor(open, spot(201), false) ?? open)).toBe(true);
  });
});

describe('texts', () => {
  test('the title of the editor', () => {
    expect(editorTitle(openEditor(spot(5)))).toBe('Line 5');
    expect(editorTitle({ ...openEditor(spot(5)), end: 8 })).toBe('Lines 5–8');
    expect(editorTitle(openEditor(spot(5, 'a.ts', 'old')))).toBe('Line 5 (old side)');
  });

  test('the label of a line number', () => {
    expect(spotLabel(spot(5))).toBe('Comment on a.ts line 5');
    expect(spotLabel(spot(5, 'a.ts', 'old'))).toBe('Comment on a.ts line 5 (old side)');
  });
});
