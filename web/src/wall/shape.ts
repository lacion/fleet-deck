// R13 P1 (design §4.1): the shape class of the wall and the tile count are pure functions of the
// viewport and the URL.
import type { WallShape } from '../app/routes';
import { MAX_TILES } from '../watch/tiles';

/** The shape from the size of the root: an override wins, then the aspect ratio w/h. */
export function shapeOf(width: number, height: number, override: 'auto' | WallShape): WallShape {
  if (override !== 'auto') return override;
  const ratio = height > 0 ? width / height : 0;
  if (ratio >= 2.2) return 'ultra';
  if (ratio >= 1.6) return 'wide';
  if (ratio >= 1.2) return 'standard';
  return 'portrait';
}

/**
 * The columns and rows of tiles that fit an area, and how many tiles the wall subscribes. `n` tiles
 * and `n - 1` gaps fit when `n * tile + (n - 1) * gap <= size`, so `n = floor((size + gap) / (tile + gap))`.
 */
export function tileLimit(
  areaWidthPx: number,
  areaHeightPx: number,
  remPx: number,
  scale: number,
  gapPx: number,
): number {
  const tileW = 22 * remPx * scale;
  const tileH = 11 * remPx * scale;
  const columns = Math.max(1, Math.floor((areaWidthPx + gapPx) / (tileW + gapPx)));
  const rows = Math.max(1, Math.floor((areaHeightPx + gapPx) / (tileH + gapPx)));
  return Math.min(MAX_TILES, columns * rows);
}

/** The base font of the root, in px: `clamp(14px, height / 80, 28px)`; an ultra wall is read from
 * further away, so its lower limit is 16 px (the same values as `--fd-wall-rem` in wall.css). */
export const baseRemPx = (heightPx: number, shape?: WallShape): number =>
  Math.min(28, Math.max(shape === 'ultra' ? 16 : 14, heightPx / 80));

/** The most that the tile type grows when there are few tiles. The needs area grows by the same
 * factor (`--fd-wall-tile-k` on the grid), so a tile never reads louder than a request. */
export const TILE_GROW_MAX = 1.25;

/**
 * How `n` tiles fill an area: the column count that gives the largest tiles, and `k`, how much the
 * tile type grows (1 at the smallest tile, at most `TILE_GROW_MAX`). The rows share the height.
 */
export function tileGrid(
  n: number,
  areaWidthPx: number,
  areaHeightPx: number,
  remPx: number,
  scale: number,
  gapPx: number,
): { readonly columns: number; readonly k: number } {
  const tileW = 22 * remPx * scale;
  const tileH = 11 * remPx * scale;
  const fit = Math.max(1, Math.floor((areaWidthPx + gapPx) / (tileW + gapPx)));
  let best = { columns: Math.min(fit, Math.max(1, n)), k: 1 };
  let bestGrow = 0;
  for (let columns = 1; columns <= Math.min(fit, Math.max(1, n)); columns += 1) {
    const rows = Math.ceil(Math.max(1, n) / columns);
    const w = (areaWidthPx - (columns - 1) * gapPx) / columns;
    const h = (areaHeightPx - (rows - 1) * gapPx) / rows;
    const grow = Math.min(w / tileW, h / tileH);
    if (grow > bestGrow) {
      bestGrow = grow;
      best = { columns, k: Math.min(TILE_GROW_MAX, Math.max(1, grow)) };
    }
  }
  return best;
}
