// An effect returns nothing or a cleanup function. An expression body returns the value of its
// expression: `useEffect(() => el.scrollTo(…))` returns a Promise in new browsers, and React then
// calls it as a cleanup and the board crashes when the component unmounts (Chrome 155). Every
// effect of the board has a block body (or returns a cleanup arrow: `() => () => …`).
import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

const ROOT = join(import.meta.dir, '..');

function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return name === 'gen' ? [] : sources(path);
    return /\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name) ? [path] : [];
  });
}

/** `useEffect(() => expr` or `useLayoutEffect(() => expr` where expr is neither a block nor `() =>`. */
const EXPRESSION_EFFECT = /use(?:Layout)?Effect\(\s*\(\)\s*=>\s*(?!\{|\(\)\s*=>)\S/g;

describe('effects', () => {
  test('every effect has a block body or returns a cleanup arrow', () => {
    const bad = sources(ROOT).flatMap((file) => {
      const text = readFileSync(file, 'utf8');
      return [...text.matchAll(EXPRESSION_EFFECT)].map(
        (m) => `${file.slice(ROOT.length + 1)}:${text.slice(0, m.index).split('\n').length}`,
      );
    });
    expect(bad).toEqual([]);
  });
});
