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

/** The columns and rows of tiles that fit an area, and how many tiles the wall subscribes. */
export function tileLimit(
  areaWidthPx: number,
  areaHeightPx: number,
  remPx: number,
  scale: number,
): number {
  const columns = Math.max(1, Math.floor(areaWidthPx / (22 * remPx * scale)));
  const rows = Math.max(1, Math.floor(areaHeightPx / (11 * remPx * scale)));
  return Math.min(MAX_TILES, columns * rows);
}

/** The base font of the root, in px: `clamp(14px, height / 80, 28px)`. */
export const baseRemPx = (heightPx: number): number => Math.min(28, Math.max(14, heightPx / 80));
