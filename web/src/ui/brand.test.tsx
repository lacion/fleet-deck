// The Fleet Deck mark (DESIGN-SYSTEM.md §2.6): the path rules of brand.ts and the Mark component.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { MARK_BOX, MARK_PATH } from './brand';
import { Mark } from './Mark';

/** The numbers of a path, without the two arc flags of every `A` command. */
function coordinates(path: string): number[] {
  const out: number[] = [];
  for (const segment of path.split(/(?=[MLQAHVZ])/)) {
    const command = segment.charAt(0);
    const numbers = (segment.slice(1).match(/-?\d+(?:\.\d+)?/g) ?? []).map(Number);
    if (command === 'A') {
      // rx ry rotation large-arc sweep x y: skip the two flags (index 3 and 4 of each group of 7).
      numbers.forEach((value, index) => {
        if (index % 7 !== 3 && index % 7 !== 4) {
          out.push(value);
        }
      });
    } else {
      out.push(...numbers);
    }
  }
  return out;
}

describe('the mark path', () => {
  test('the mark path uses absolute commands and stays in its box', () => {
    expect(MARK_PATH).toMatch(/^[MLQAHVZ0-9. -]+$/);
    expect(MARK_PATH.startsWith('M')).toBe(true);
    const numbers = coordinates(MARK_PATH);
    expect(numbers.length).toBeGreaterThan(0);
    for (const value of numbers) {
      expect(value).toBeGreaterThanOrEqual(0);
      expect(value).toBeLessThanOrEqual(MARK_BOX);
    }
  });
});

describe('the Mark component', () => {
  test('Mark draws one even-odd path and no text', () => {
    const html = renderToStaticMarkup(<Mark />);
    expect(html.match(/<path/g)?.length).toBe(1);
    expect(html).toContain('fill-rule="evenodd"');
    expect(html).toContain(`d="${MARK_PATH}"`);
    expect(html).toContain('width="24"');
    expect(html).toContain('viewBox="0 0 24 24"');
    expect(html).toContain('aria-hidden="true"');
    expect(html).not.toContain('<text');
    expect(html).not.toContain('<rect');
    expect(renderToStaticMarkup(<Mark size={32} />)).toContain('width="32"');
  });
});
