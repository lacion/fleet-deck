import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { fitChars, PathTail, shortPath } from './PathTail';

const REAPER = 'crates/fleetdeck-station/src/shim/reaper.rs';

describe('path shortening', () => {
  test('a path that fits stays whole', () => {
    expect(shortPath(REAPER, REAPER.length)).toBe(REAPER);
    expect(shortPath(REAPER, 200)).toBe(REAPER);
  });

  test('whole leading folders give way to "…/", never part of a word', () => {
    expect(shortPath(REAPER, 40)).toBe('…/fleetdeck-station/src/shim/reaper.rs');
    expect(shortPath(REAPER, 37)).toBe('…/src/shim/reaper.rs');
    expect(shortPath(REAPER, 20)).toBe('…/src/shim/reaper.rs');
    expect(shortPath(REAPER, 19)).toBe('…/shim/reaper.rs');
    expect(shortPath(REAPER, 11)).toBe('…/reaper.rs');
  });

  test('a file name that alone is too long keeps "…/" and the name (the CSS cuts the rest)', () => {
    expect(shortPath(REAPER, 5)).toBe('…/reaper.rs');
    expect(shortPath('reaper.rs', 5)).toBe('reaper.rs');
    expect(shortPath('/abs/dir/', 3)).toBe('/abs/dir/');
  });

  test('the characters that fit a box, from the measured text width', () => {
    expect(fitChars(300, 430, 43)).toBe(30);
    expect(fitChars(299.995, 430, 43)).toBe(30);
    expect(fitChars(0, 430, 43)).toBe(0);
    expect(fitChars(300, 0, 43)).toBeNull();
    expect(fitChars(300, 430, 0)).toBeNull();
  });

  test('before it is measured, the whole path renders', () => {
    expect(renderToStaticMarkup(<PathTail path={REAPER} />)).toBe(`<bdi>${REAPER}</bdi>`);
  });
});
