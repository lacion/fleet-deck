// R13 P4 task F2 (design `omarchy-p4.md` §3.7): the wall display uses the Omarchy palette of its
// bound station. It reads `GET /api/displays/self/palette` at start and every 60 s: a 200 applies the
// palette, a 404 gives the default tokens, a 401 calls `onEnded` (the login ended), and another
// failure keeps what the page shows. The wall has no select.
import { useMemo } from 'react';
import { useShownPalette } from '../app/OmarchyTheme';
import { type Palette, readPalette, settlePalette } from '../ui/palette';

/** The path of the palette of the bound station (the display login only). */
export const DISPLAY_PALETTE_PATH = '/api/displays/self/palette';

/** One read and its effect. Returns whether a palette shows after it. */
export async function paletteTick(
  get: (path: string) => Promise<unknown>,
  shown: boolean,
  show: (palette: Palette | null) => void,
  onEnded: () => void,
): Promise<boolean> {
  return settlePalette(await readPalette(() => get(DISPLAY_PALETTE_PATH)), shown, show, onEnded);
}

/** Applies the palette of the display's bound station; one request at a time. */
export function usePalette(get: (path: string) => Promise<unknown>, onEnded: () => void): void {
  const load = useMemo(() => () => get(DISPLAY_PALETTE_PATH), [get]);
  useShownPalette(load, onEnded);
}
