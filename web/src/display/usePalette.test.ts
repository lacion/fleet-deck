// R13 P4 task F2: the wall reads `GET /api/displays/self/palette`: 200 applies, 404 gives the
// default tokens, 401 ends the login.
import { describe, expect, test } from 'bun:test';
import type { PaletteColors } from '../gen';
import { ApiError } from '../runtime/api';
import type { Palette } from '../ui/palette';
import { DISPLAY_PALETTE_PATH, paletteTick } from './usePalette';

const colors: PaletteColors = {
  background: '#1a1b26',
  foreground: '#c0caf5',
  accent: '#7aa2f7',
  muted: '#565f89',
  dark_background: '#16161e',
  dark_foreground: '#a9b1d6',
  bright_foreground: '#ffffff',
  selection: '#283457',
  red: '#f7768e',
  yellow: '#e0af68',
};

function rig(answer: () => unknown) {
  const paths: string[] = [];
  const shown: (Palette | null)[] = [];
  let ended = 0;
  const get = async (path: string) => {
    paths.push(path);
    return answer();
  };
  const tick = (was: boolean) =>
    paletteTick(
      get,
      was,
      (p) => void shown.push(p),
      () => void ended++,
    );
  return { tick, paths, shown, ended: () => ended };
}

describe('the wall palette', () => {
  test('a 200 applies the palette', async () => {
    const r = rig(() => ({ mode: 'dark', colors, updated_at: 1 }));
    expect(await r.tick(false)).toBe(true);
    expect(r.paths).toEqual([DISPLAY_PALETTE_PATH]);
    expect(DISPLAY_PALETTE_PATH).toBe('/api/displays/self/palette');
    expect(r.shown).toEqual([{ mode: 'dark', colors }]);
    expect(r.ended()).toBe(0);
  });

  test('a 404 gives the default tokens after a palette, and nothing before one', async () => {
    const r = rig(() => {
      throw new ApiError(404, 'no palette', 'no_palette');
    });
    expect(await r.tick(true)).toBe(false);
    expect(r.shown).toEqual([null]);
    expect(await r.tick(false)).toBe(false);
    expect(r.shown).toEqual([null]);
  });

  test('a 401 ends the login and keeps the page', async () => {
    const r = rig(() => {
      throw new ApiError(401, 'login required', 'login_required');
    });
    expect(await r.tick(true)).toBe(true);
    expect(r.ended()).toBe(1);
    expect(r.shown).toEqual([]);
  });

  test('another failure keeps what shows', async () => {
    const r = rig(() => {
      throw new ApiError(500, 'boom');
    });
    expect(await r.tick(true)).toBe(true);
    expect(r.ended()).toBe(0);
    expect(r.shown).toEqual([]);
  });
});
