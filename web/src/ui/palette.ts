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
  '--ok',
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

/**
 * The first percent from `p` up (in steps of 2) where `mix(fg, pct, bg)` reaches `ratio` on every
 * one of `surfaces`.
 */
function rampPct(
  fg: string,
  bg: string,
  p: number,
  surfaces: readonly string[],
  ratio: number,
): number {
  const low = (pct: number) => surfaces.some((s) => contrast(mixHex(fg, pct, bg), s) < ratio);
  let pct = Math.min(100, p);
  while (pct < 100 && low(pct)) pct = Math.min(100, pct + 2);
  return pct;
}

/**
 * `color` as it is, or mixed in 5 % steps toward `ink` (black on a light deck, white on a dark
 * one, so that the hue stays) until it reaches `ratio` on every one of `surfaces`: 4.5:1 for a
 * text (a tone text sits on `--raised-hi` and on every tint, DESIGN-SYSTEM §6), 3:1 for a line.
 */
export function readableAll(
  color: string,
  ink: string,
  surfaces: readonly string[],
  ratio = 4.5,
): string {
  const low = (c: string) => surfaces.some((s) => contrast(c, s) < ratio);
  let pct = 0;
  while (pct < 100 && low(mixHex(ink, pct, color))) pct += 5;
  return mixHex(ink, pct, color);
}

/**
 * The token values of a palette (decision 1), as `#rrggbb` (or `#rrggbbaa` for the glow). Every
 * color is checked again: a palette with a bad color gives `{}`, so that nothing is applied.
 * Palette colors and fixed mix steps do not keep the accessibility floor (DESIGN-SYSTEM §6), so the
 * text tokens step until they reach 4.5:1 on `--raised-hi` (the surface nearest to the text) and
 * on every tint (`--act-bg`, `--hazard-bg`, `--ok-bg`, `--fuel-bg`, `--focus-bg`: a badge or a
 * selected row puts text on them), `--ctl-edge` until it reaches 3:1 on `--raised-hi`, and
 * `--act-line` until it reaches 3:1 on `--bg`, `--raised` and `--act-bg`. A palette whose
 * foreground is under 4.5:1 on `--raised-hi` gives `{}`.
 */
export function paletteVars(p: Palette): Record<string, string> {
  const c = p.colors;
  if (!PALETTE_KEYS.every((key) => isHexColor(c[key]))) return {};
  const fg = c.foreground.toLowerCase();
  const bg = c.background.toLowerCase();
  const yellow = c.yellow.toLowerCase();
  const red = c.red.toLowerCase();
  const accent = c.accent.toLowerCase();
  const raisedHi = mixHex(fg, 12, bg);
  const raised = mixHex(fg, 8, bg);
  if (contrast(fg, raisedHi) < 4.5) return {};
  // The ink extreme on the side of the foreground: semantic colors darken (or lighten) toward it.
  const ink = luminance(fg) < luminance(bg) ? '#000000' : '#ffffff';
  // The tints: 14 % of the tone on --bg (fuel 16 %). --focus-bg tints the accent as it reads on
  // --raised-hi; the palette `selection` color is a terminal selection, not a text surface.
  const actBg = mixHex(yellow, 14, bg);
  const hazardBg = mixHex(red, 14, bg);
  const okBg = mixHex(OK[p.mode], 14, bg);
  const fuelBg = mixHex(FUEL[p.mode], 16, bg);
  const focusBg = mixHex(readableAll(accent, ink, [raisedHi]), 14, bg);
  const surfaces = [raisedHi, actBg, hazardBg, okBg, fuelBg, focusBg];
  // --faint starts at 58 %, --dim at 72 %. When --faint must climb, --dim stays a step over it
  // (14 points, or half the way to the foreground), so that the three text steps stay apart.
  const faintPct = rampPct(fg, bg, 58, surfaces, 4.5);
  const dimPct = rampPct(
    fg,
    bg,
    Math.max(72, faintPct + Math.min(14, Math.round((100 - faintPct) / 2))),
    surfaces,
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
    '--raised': raised,
    '--border': mixHex(fg, 18, bg),
    '--border-soft': mixHex(fg, 10, bg),
    // The edge of a form control: 3:1 on every surface up to --raised-hi (WCAG 1.4.11).
    '--ctl-edge': mixHex(fg, rampPct(fg, bg, 50, [raisedHi], 3), bg),
    '--dim': mixHex(fg, dimPct, bg),
    '--faint': mixHex(fg, faintPct, bg),
    '--faint-decor': mixHex(fg, 40, bg),
    '--hazard': readableAll(red, ink, surfaces),
    '--act': readableAll(yellow, ink, surfaces),
    '--act-bg': actBg,
    '--hazard-bg': hazardBg,
    '--act-ink': actInk,
    '--raised-hi': raisedHi,
    '--act-fill': yellow,
    // The needs-you outline is a state line: 3:1 where a needs-you strip or card sits (1.4.11).
    '--act-line': readableAll(yellow, ink, [bg, raised, actBg], 3),
    '--act-glow': `${yellow}33`, // 20 % alpha
    '--ink-fill': fg,
    '--ink-on': bg,
    '--ok': readableAll(OK[p.mode], ink, surfaces),
    '--ok-bg': okBg,
    '--fuel': readableAll(FUEL[p.mode], ink, surfaces),
    '--fuel-bg': fuelBg,
    '--focus': readableAll(accent, ink, surfaces),
    '--focus-bg': focusBg,
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

const FORCED = '(forced-colors: active)';

/** The forced-colors query of the page, or null without one (a test, a server). */
const forcedQuery = (): MediaQueryList | null =>
  typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia(FORCED)
    : null;

/** The palette of the last `showPalette` call: shown again when forced colors change. */
let wanted: Palette | null = null;
let watching = false;

/**
 * Shows `palette` on the page, or the normal theme for `null` (or for a palette that is refused).
 * While a palette shows, its mode is the `data-theme` of the page (`setThemeOverride`). In forced
 * colors no palette shows: the page takes the OS color scheme (`resolvedTheme`, DESIGN-SYSTEM §6),
 * and the palette comes back when forced colors end.
 */
export function showPalette(palette: Palette | null): void {
  if (typeof document === 'undefined') return;
  wanted = palette;
  const forced = forcedQuery();
  if (forced !== null && !watching) {
    watching = true;
    forced.addEventListener('change', () => showPalette(wanted));
  }
  const root = document.documentElement;
  if (palette !== null && forced?.matches !== true && applyPalette(root, palette)) {
    setThemeOverride(palette.mode);
    return;
  }
  setThemeOverride(null);
  removePalette(root, resolvedTheme());
}
