// R13 P4 task F2 (design `omarchy-p4.md` §3.7): an Omarchy palette becomes the color tokens of the
// board. The pure functions here map the palette to CSS custom properties and set or remove them on
// a root element (the tests pass a fake root). Nothing here reads the network or the storage.
import type { PaletteColors, PaletteMode } from '../gen';
import { ApiError } from '../runtime/api';
import { resolvedTheme, setThemeOverride } from './theme';

/** The body of `GET /api/stations/{id}/palette` and `GET /api/displays/self/palette`. */
export type Palette = {
  readonly mode: PaletteMode;
  readonly colors: PaletteColors;
};

/** The ten color keys of a palette. */
export const PALETTE_KEYS = [
  'background',
  'foreground',
  'accent',
  'muted',
  'dark_background',
  'dark_foreground',
  'bright_foreground',
  'selection',
  'red',
  'yellow',
] as const satisfies readonly (keyof PaletteColors)[];

const COLOR = /^#[0-9A-Fa-f]{6}$/;

/** True for a color that the deck may send: `#rrggbb`. */
export const isHexColor = (v: unknown): v is string => typeof v === 'string' && COLOR.test(v);

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** Checks an answer of the palette routes. `null`: the mode or one of the ten colors is bad. */
export function parsePalette(v: unknown): Palette | null {
  if (!record(v) || (v['mode'] !== 'dark' && v['mode'] !== 'light')) return null;
  const colors = v['colors'];
  if (!record(colors)) return null;
  const out: Record<string, string> = {};
  for (const key of PALETTE_KEYS) {
    const value = colors[key];
    if (!isHexColor(value)) return null;
    out[key] = value;
  }
  return { mode: v['mode'], colors: out as PaletteColors };
}

/** The path of the palette of one station (owner login; the station id). */
export const stationPalettePath = (stationId: string): string =>
  `/api/stations/${encodeURIComponent(stationId)}/palette`;

/** One read of a palette route. */
export type PaletteRead =
  | { readonly kind: 'palette'; readonly palette: Palette }
  /** A 404 (`no_palette`), or an answer that is not a palette. */
  | { readonly kind: 'none' }
  /** A 401: the login ended. */
  | { readonly kind: 'ended' }
  /** Any other failure: the caller keeps what it shows. */
  | { readonly kind: 'error' };

/** Reads and checks a palette. `load` throws an `ApiError` for a non-2xx answer. */
export async function readPalette(load: () => Promise<unknown>): Promise<PaletteRead> {
  try {
    const palette = parsePalette(await load());
    return palette === null ? { kind: 'none' } : { kind: 'palette', palette };
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) return { kind: 'none' };
    if (e instanceof ApiError && e.status === 401) return { kind: 'ended' };
    return { kind: 'error' };
  }
}

/**
 * What one read changes: a palette shows; `none` shows the normal theme (only when a palette showed);
 * `ended` calls `onEnded`; an error keeps the page as it is. Returns whether a palette shows now.
 */
export function settlePalette(
  read: PaletteRead,
  shown: boolean,
  show: (palette: Palette | null) => void,
  onEnded?: () => void,
): boolean {
  switch (read.kind) {
    case 'palette':
      show(read.palette);
      return true;
    case 'none':
      if (shown) show(null);
      return false;
    case 'ended':
      onEnded?.();
      return shown;
    case 'error':
      return shown;
  }
}

/** The custom properties that a palette sets. */
export const PALETTE_VARS = [
  '--bg',
  '--text',
  '--code',
  '--surface',
  '--raised',
  '--border',
  '--border-soft',
  '--dim',
  '--faint',
  '--faint-decor',
  '--hazard',
  '--act',
  '--act-bg',
  '--hazard-bg',
  '--act-ink',
  '--raised-hi',
  '--act-fill',
  '--act-line',
  '--act-glow',
  '--ink-fill',
  '--ink-on',
  '--ok-bg',
  '--fuel',
  '--fuel-bg',
  '--focus',
  '--focus-bg',
] as const;

/**
 * The fuel color (usage, tokens, cost) of each mode. A palette has no purple: fuel keeps the
 * design system color, so that it never takes the meaning of another color.
 */
const FUEL: Record<PaletteMode, string> = { dark: '#b79cff', light: '#6e4fe0' };

/**
 * The token values of a palette (decision 1). Every color is checked again: a palette with a bad
 * color gives `{}`, so that nothing is applied.
 */
export function paletteVars(p: Palette): Record<string, string> {
  const c = p.colors;
  if (!PALETTE_KEYS.every((key) => isHexColor(c[key]))) return {};
  const mix = (a: string, pct: number, b: string) => `color-mix(in srgb, ${a} ${pct}%, ${b})`;
  const fg = c.foreground;
  const bg = c.background;
  return {
    '--bg': bg,
    '--text': fg,
    '--code': c.dark_background,
    '--surface': mix(fg, 4, bg),
    '--raised': mix(fg, 8, bg),
    '--border': mix(fg, 18, bg),
    '--border-soft': mix(fg, 10, bg),
    '--dim': mix(fg, 72, bg),
    '--faint': mix(fg, 58, bg),
    '--faint-decor': mix(fg, 40, bg),
    '--hazard': c.red,
    '--act': c.yellow,
    '--act-bg': mix(c.yellow, 14, bg),
    '--hazard-bg': mix(c.red, 14, bg),
    '--act-ink': bg,
    '--raised-hi': mix(fg, 12, bg),
    '--act-fill': c.yellow,
    '--act-line': c.yellow,
    '--act-glow': mix(c.yellow, 20, 'transparent'),
    '--ink-fill': fg,
    '--ink-on': bg,
    '--ok-bg': mix('var(--ok)', 14, bg),
    '--fuel': FUEL[p.mode],
    '--fuel-bg': mix(FUEL[p.mode], 16, bg),
    '--focus': c.accent,
    '--focus-bg': c.selection,
  };
}

/** The part of the root element (`<html>`) that the palette touches. */
export type PaletteRoot = {
  readonly style: Pick<CSSStyleDeclaration, 'setProperty' | 'removeProperty'>;
  readonly dataset: DOMStringMap;
};

/**
 * Sets the inline properties and `data-theme` (the palette mode) on `root`. A palette with a bad
 * color or mode is not applied and the properties are removed; `restore` is then the mode of the
 * page. Returns true when the palette is applied.
 */
export function applyPalette(
  root: PaletteRoot,
  palette: Palette,
  restore: PaletteMode = 'dark',
): boolean {
  const entries = Object.entries(paletteVars(palette));
  if (entries.length === 0 || (palette.mode !== 'dark' && palette.mode !== 'light')) {
    removePalette(root, restore);
    return false;
  }
  for (const [name, value] of entries) root.style.setProperty(name, value);
  root.dataset['theme'] = palette.mode;
  return true;
}

/** Removes the inline properties and sets `data-theme` to `mode` (the mode of the user's choice). */
export function removePalette(root: PaletteRoot, mode: PaletteMode): void {
  for (const name of PALETTE_VARS) root.style.removeProperty(name);
  root.dataset['theme'] = mode;
}

/**
 * Shows `palette` on the page, or the normal theme for `null` (or for a palette that is refused).
 * While a palette shows, its mode is the `data-theme` of the page (`setThemeOverride`).
 */
export function showPalette(palette: Palette | null): void {
  if (typeof document === 'undefined') return;
  const root = document.documentElement;
  if (palette !== null && applyPalette(root, palette)) {
    setThemeOverride(palette.mode);
    return;
  }
  setThemeOverride(null);
  removePalette(root, resolvedTheme());
}
