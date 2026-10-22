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
  '--ctl-edge',
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
const FUEL: Record<PaletteMode, string> = { dark: '#b79cff', light: '#6447d6' };

/** The `--ok` of each mode (tokens.css): a palette has no green, so `--ok-bg` tints this one. */
const OK: Record<PaletteMode, string> = { dark: '#3ddc97', light: '#0a7048' };

const channels = (hex: string): [number, number, number] => [
  Number.parseInt(hex.slice(1, 3), 16),
  Number.parseInt(hex.slice(3, 5), 16),
  Number.parseInt(hex.slice(5, 7), 16),
];

const toHex = (rgb: readonly number[]): string =>
  `#${rgb.map((v) => Math.round(v).toString(16).padStart(2, '0')).join('')}`;

/** `color-mix(in srgb, a pct%, b)`, computed: `#rrggbb`. */
export function mixHex(a: string, pct: number, b: string): string {
  const ca = channels(a);
  const cb = channels(b);
  return toHex(ca.map((v, i) => (v * pct + (cb[i] ?? 0) * (100 - pct)) / 100));
}

/** The WCAG 2 relative luminance of `#rrggbb`. */
function luminance(hex: string): number {
  const [r, g, b] = channels(hex).map((v) => {
    const s = v / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * (r ?? 0) + 0.7152 * (g ?? 0) + 0.0722 * (b ?? 0);
}

/** The WCAG 2 contrast ratio of two `#rrggbb` colors (1 to 21). */
export function contrast(a: string, b: string): number {
  const la = luminance(a);
  const lb = luminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

/** The first percent from `p` up (in steps of 2) where `mix(fg, pct, bg)` reaches `ratio` on `surface`. */
function rampPct(fg: string, bg: string, p: number, surface: string, ratio: number): number {
  let pct = Math.min(100, p);
  while (pct < 100 && contrast(mixHex(fg, pct, bg), surface) < ratio) pct = Math.min(100, pct + 2);
  return pct;
}

/**
 * `color` as it is, or mixed in 5 % steps toward `ink` (black on a light deck, white on a dark
 * one, so that the hue stays) until it reaches 4.5:1 on `surface`.
 */
function readable(color: string, ink: string, surface: string): string {
  let pct = 0;
  while (pct < 100 && contrast(mixHex(ink, pct, color), surface) < 4.5) pct += 5;
  return mixHex(ink, pct, color);
}

/**
 * The token values of a palette (decision 1), as `#rrggbb` (or `#rrggbbaa` for the glow). Every
 * color is checked again: a palette with a bad color gives `{}`, so that nothing is applied.
 * Palette colors and fixed mix steps do not keep the accessibility floor (DESIGN-SYSTEM §6), so the
 * text tokens step until they reach 4.5:1 on `--raised-hi` (the surface nearest to the text), and
 * `--ctl-edge` until it reaches 3:1 there. A palette whose foreground is under 4.5:1 on
 * `--raised-hi` gives `{}`.
 */
export function paletteVars(p: Palette): Record<string, string> {
  const c = p.colors;
  if (!PALETTE_KEYS.every((key) => isHexColor(c[key]))) return {};
  const fg = c.foreground.toLowerCase();
  const bg = c.background.toLowerCase();
  const yellow = c.yellow.toLowerCase();
  const red = c.red.toLowerCase();
  const raisedHi = mixHex(fg, 12, bg);
  if (contrast(fg, raisedHi) < 4.5) return {};
  // The ink extreme on the side of the foreground: semantic colors darken (or lighten) toward it.
  const ink = luminance(fg) < luminance(bg) ? '#000000' : '#ffffff';
  // --faint starts at 58 %, --dim at 72 %. When --faint must climb, --dim stays a step over it
  // (14 points, or half the way to the foreground), so that the three text steps stay apart.
  const faintPct = rampPct(fg, bg, 58, raisedHi, 4.5);
  const dimPct = rampPct(
    fg,
    bg,
    Math.max(72, faintPct + Math.min(14, Math.round((100 - faintPct) / 2))),
    raisedHi,
    4.5,
  );
  // --act-ink: bg or fg on the yellow fill, whichever reads better; else black or white.
  const inks = [bg, fg, '#000000', '#ffffff'].map((ink) => ({ ink, ratio: contrast(ink, yellow) }));
  const pair = inks.slice(0, 2).sort((a, b) => b.ratio - a.ratio)[0];
  const pure = inks.slice(2).sort((a, b) => b.ratio - a.ratio)[0];
  const actInk = pair !== undefined && pair.ratio >= 4.5 ? pair.ink : (pure?.ink ?? bg);
  return {
    '--bg': bg,
    '--text': fg,
    '--code': c.dark_background.toLowerCase(),
    '--surface': mixHex(fg, 4, bg),
    '--raised': mixHex(fg, 8, bg),
    '--border': mixHex(fg, 18, bg),
    '--border-soft': mixHex(fg, 10, bg),
    // The edge of a form control: 3:1 on every surface up to --raised-hi (WCAG 1.4.11).
    '--ctl-edge': mixHex(fg, rampPct(fg, bg, 50, raisedHi, 3), bg),
    '--dim': mixHex(fg, dimPct, bg),
    '--faint': mixHex(fg, faintPct, bg),
    '--faint-decor': mixHex(fg, 40, bg),
    '--hazard': readable(red, ink, raisedHi),
    '--act': readable(yellow, ink, raisedHi),
    '--act-bg': mixHex(yellow, 14, bg),
    '--hazard-bg': mixHex(red, 14, bg),
    '--act-ink': actInk,
    '--raised-hi': raisedHi,
    '--act-fill': yellow,
    '--act-line': yellow,
    '--act-glow': `${yellow}33`, // 20 % alpha
    '--ink-fill': fg,
    '--ink-on': bg,
    '--ok-bg': mixHex(OK[p.mode], 14, bg),
    '--fuel': readable(FUEL[p.mode], ink, raisedHi),
    '--fuel-bg': mixHex(FUEL[p.mode], 16, bg),
    '--focus': readable(c.accent.toLowerCase(), ink, raisedHi),
    '--focus-bg': c.selection.toLowerCase(),
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
