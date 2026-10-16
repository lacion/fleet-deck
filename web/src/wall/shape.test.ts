// R13 P1: the shape class and the tile count are pure functions of the viewport and the URL.
import { describe, expect, test } from 'bun:test';
import { MAX_TILES } from '../watch/tiles';
import { baseRemPx, shapeOf, tileLimit } from './shape';

describe('shapeOf', () => {
  test('the aspect ratio picks the shape', () => {
    expect(shapeOf(3440, 1440, 'auto')).toBe('ultra');
    expect(shapeOf(5120, 1440, 'auto')).toBe('ultra');
    expect(shapeOf(2560, 1440, 'auto')).toBe('wide');
    expect(shapeOf(1920, 1080, 'auto')).toBe('wide');
    // 1.6 exactly is the lower limit of `wide`.
    expect(shapeOf(1920, 1200, 'auto')).toBe('wide');
    expect(shapeOf(2560, 1600, 'auto')).toBe('wide');
    expect(shapeOf(1600, 1200, 'auto')).toBe('standard');
    expect(shapeOf(1440, 1080, 'auto')).toBe('standard');
    expect(shapeOf(1080, 1920, 'auto')).toBe('portrait');
  });

  test('the limits belong to the upper class', () => {
    expect(shapeOf(220, 100, 'auto')).toBe('ultra');
    expect(shapeOf(160, 100, 'auto')).toBe('wide');
    expect(shapeOf(120, 100, 'auto')).toBe('standard');
    expect(shapeOf(119, 100, 'auto')).toBe('portrait');
  });

  test('an override wins, and an empty viewport is a portrait', () => {
    expect(shapeOf(3440, 1440, 'portrait')).toBe('portrait');
    expect(shapeOf(1080, 1920, 'ultra')).toBe('ultra');
    expect(shapeOf(0, 0, 'auto')).toBe('portrait');
  });
});

describe('baseRemPx', () => {
  test('18 px at 1440 px high, clamped to 14 to 28', () => {
    expect(baseRemPx(1440)).toBe(18);
    expect(baseRemPx(300)).toBe(14);
    expect(baseRemPx(4000)).toBe(28);
  });
});

describe('tileLimit', () => {
  test('the four shapes (the tiles area of each, in px)', () => {
    // 3440×1440 ultra: the area is about 60% wide and 75% high.
    expect(tileLimit(2000, 1050, baseRemPx(1440), 1)).toBe(MAX_TILES); // 5 × 5 = 25, capped
    // 2560×1440 wide.
    expect(tileLimit(1900, 900, baseRemPx(1440), 1)).toBe(16); // 4 × 4
    // 1920×1200 (a ratio of 1.6: wide).
    expect(tileLimit(1850, 600, baseRemPx(1200), 1)).toBe(15); // 5 × 3
    // 1080×1920 portrait.
    expect(tileLimit(1000, 1000, baseRemPx(1920), 1)).toBe(3); // 1 × 3
  });

  test('a tile never fits less than one, and the scale makes tiles larger', () => {
    expect(tileLimit(0, 0, 18, 1)).toBe(1);
    expect(tileLimit(100, 100, 18, 2)).toBe(1);
    expect(tileLimit(2000, 1050, 18, 2)).toBe(4); // 2 × 2
  });

  test('never more than the 24 subscriptions', () => {
    expect(tileLimit(100_000, 100_000, 14, 0.75)).toBe(MAX_TILES);
  });
});
