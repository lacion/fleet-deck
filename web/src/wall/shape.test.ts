// R13 P1: the shape class and the tile count are pure functions of the viewport and the URL.
import { describe, expect, test } from 'bun:test';
import { MAX_TILES } from '../watch/tiles';
import {
  baseRemPx,
  SCROLL_TILES_SHARE,
  shapeOf,
  TILE_GROW_MAX,
  tileBox,
  tileGrid,
  tileLimit,
} from './shape';

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

  test('an ultra wall has a floor of 16 px', () => {
    expect(baseRemPx(1080, 'ultra')).toBe(16);
    expect(baseRemPx(1080, 'wide')).toBe(14);
    expect(baseRemPx(1440, 'ultra')).toBe(18);
  });
});

describe('tileGrid', () => {
  const GAP = 8;

  test('few tiles share the area and their type grows, up to the limit', () => {
    // 1060 × 700 at 14 px: a smallest tile is 308 × 154.
    const three = tileGrid(3, 1060, 700, 14, 1, GAP);
    expect(three.columns).toBe(2);
    // 526 / 308 = 1.71: the type grows to the limit.
    expect(three.k).toBe(TILE_GROW_MAX);
    expect(tileGrid(1, 2000, 1000, 14, 1, GAP)).toEqual({ columns: 1, k: TILE_GROW_MAX });
    // The needs area grows by the same factor: the cap keeps it inside its column.
    expect(TILE_GROW_MAX).toBe(1.25);
  });

  test('a full area keeps the columns that fit, and the type grows only by the room left', () => {
    // 3 × 4 tiles: the rows are (700 - 3 × 8) / 4 = 169 px high, 169 / 154 = 1.10.
    const twelve = tileGrid(12, 1060, 700, 14, 1, GAP);
    expect(twelve.columns).toBe(3);
    expect(twelve.k).toBeCloseTo(169 / 154, 2);
    expect(tileGrid(30, 1060, 700, 14, 1, GAP).k).toBe(1);
    expect(tileGrid(0, 1060, 700, 14, 1, GAP).columns).toBe(1);
  });
});

describe('tileLimit', () => {
  // The CSS gap of `.fd-wall-tiles` is `--sp-2`: 8 px.
  const GAP = 8;

  test('the four shapes (the tiles area of each, in px)', () => {
    // 3440×1440 ultra: the area is about 60% wide and 75% high.
    expect(tileLimit(2000, 1050, baseRemPx(1440), 1, GAP)).toBe(20); // 4 × 5
    // 2560×1440 wide.
    expect(tileLimit(1900, 900, baseRemPx(1440), 1, GAP)).toBe(16); // 4 × 4
    // 1920×1200 (a ratio of 1.6: wide).
    expect(tileLimit(1850, 600, baseRemPx(1200), 1, GAP)).toBe(15); // 5 × 3
    // 1080×1920 portrait.
    expect(tileLimit(1000, 1000, baseRemPx(1920), 1, GAP)).toBe(3); // 1 × 3
  });

  test('the gap counts: two tiles of 396 px and one gap need 800 px, not 792', () => {
    expect(tileLimit(792, 198, 18, 1, 0)).toBe(2); // without a gap two tiles fit
    expect(tileLimit(792, 198, 18, 1, GAP)).toBe(1); // 792 px, 18 px base, scale 1, the CSS gap
    expect(tileLimit(800, 198, 18, 1, GAP)).toBe(2);
  });

  test('a tile never fits less than one, and the scale makes tiles larger', () => {
    expect(tileLimit(0, 0, 18, 1, GAP)).toBe(1);
    expect(tileLimit(100, 100, 18, 2, GAP)).toBe(1);
    expect(tileLimit(2000, 1050, 18, 2, GAP)).toBe(4); // 2 × 2
  });

  test('never more than the 24 subscriptions', () => {
    expect(tileLimit(100_000, 100_000, 14, 0.75, GAP)).toBe(MAX_TILES);
  });
});

describe('tileBox', () => {
  const area = { w: 358, h: 1200, gap: 8 };

  test('a phone held upright scrolls: the tiles count on a fixed share of the screen', () => {
    expect(tileBox(area, 'portrait', { w: 390, h: 844 })).toEqual({
      w: 358,
      h: Math.round(844 * SCROLL_TILES_SHARE),
      gap: 8,
    });
    // The count does not follow the height of the tiles: a taller area gives the same box.
    expect(tileBox({ ...area, h: 300 }, 'portrait', { w: 390, h: 844 })).toEqual(
      tileBox(area, 'portrait', { w: 390, h: 844 }),
    );
  });

  test('a wall that does not scroll reads its tiles area', () => {
    expect(tileBox(area, 'portrait', { w: 1080, h: 1920 })).toBe(area);
    expect(tileBox(area, 'standard', { w: 600, h: 480 })).toBe(area);
    expect(tileBox(area, 'wide', { w: 1920, h: 1080 })).toBe(area);
  });
});
