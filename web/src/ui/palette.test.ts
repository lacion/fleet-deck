// R13 P4 task F2: the token mapping of an Omarchy palette, and how it sets and removes the inline
// properties. The pure parts run without a DOM: the root is a fake object.
import { describe, expect, test } from 'bun:test';
import { ApiError } from '../runtime/api';
import type { PaletteColors } from '../gen';
import {
  applyPalette,
  PALETTE_VARS,
  type Palette,
  paletteVars,
  parsePalette,
  readPalette,
  removePalette,
  settlePalette,
  showPalette,
  stationPalettePath,
} from './palette';

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
const dark: Palette = { mode: 'dark', colors };
const light: Palette = {
  mode: 'light',
  colors: { ...colors, background: '#e1e2e7', foreground: '#3760bf', red: '#f52a65' },
};

const fakeRoot = () => {
  const props = new Map<string, string>();
  const dataset: Record<string, string | undefined> = {};
  return {
    props,
    dataset,
    root: {
      style: {
        setProperty: (name: string, value: string) => void props.set(name, value),
        removeProperty: (name: string) => {
          const old = props.get(name) ?? '';
          props.delete(name);
          return old;
        },
      },
      dataset,
    },
  };
};

describe('paletteVars', () => {
  test('a dark palette gives the exact token values', () => {
    expect(paletteVars(dark)).toEqual({
      '--bg': '#1a1b26',
      '--text': '#c0caf5',
      '--code': '#16161e',
      '--surface': 'color-mix(in srgb, #c0caf5 4%, #1a1b26)',
      '--raised': 'color-mix(in srgb, #c0caf5 8%, #1a1b26)',
      '--border': 'color-mix(in srgb, #c0caf5 18%, #1a1b26)',
      '--border-soft': 'color-mix(in srgb, #c0caf5 10%, #1a1b26)',
      '--dim': 'color-mix(in srgb, #c0caf5 72%, #1a1b26)',
      '--faint': 'color-mix(in srgb, #c0caf5 58%, #1a1b26)',
      '--faint-decor': 'color-mix(in srgb, #c0caf5 40%, #1a1b26)',
      '--hazard': '#f7768e',
      '--act': '#e0af68',
      '--act-bg': 'color-mix(in srgb, #e0af68 14%, #1a1b26)',
      '--hazard-bg': 'color-mix(in srgb, #f7768e 14%, #1a1b26)',
      '--act-ink': '#1a1b26',
      '--raised-hi': 'color-mix(in srgb, #c0caf5 12%, #1a1b26)',
      '--act-fill': '#e0af68',
      '--act-line': '#e0af68',
      '--act-glow': 'color-mix(in srgb, #e0af68 20%, transparent)',
      '--ink-fill': '#c0caf5',
      '--ink-on': '#1a1b26',
      '--ok-bg': 'color-mix(in srgb, var(--ok) 14%, #1a1b26)',
      '--fuel': '#b79cff',
      '--fuel-bg': 'color-mix(in srgb, #b79cff 16%, #1a1b26)',
      '--focus': '#7aa2f7',
      '--focus-bg': '#283457',
    });
  });

  test('a light palette gives the exact token values', () => {
    expect(paletteVars(light)).toEqual({
      '--bg': '#e1e2e7',
      '--text': '#3760bf',
      '--code': '#16161e',
      '--surface': 'color-mix(in srgb, #3760bf 4%, #e1e2e7)',
      '--raised': 'color-mix(in srgb, #3760bf 8%, #e1e2e7)',
      '--border': 'color-mix(in srgb, #3760bf 18%, #e1e2e7)',
      '--border-soft': 'color-mix(in srgb, #3760bf 10%, #e1e2e7)',
      '--dim': 'color-mix(in srgb, #3760bf 72%, #e1e2e7)',
      '--faint': 'color-mix(in srgb, #3760bf 58%, #e1e2e7)',
      '--faint-decor': 'color-mix(in srgb, #3760bf 40%, #e1e2e7)',
      '--hazard': '#f52a65',
      '--act': '#e0af68',
      '--act-bg': 'color-mix(in srgb, #e0af68 14%, #e1e2e7)',
      '--hazard-bg': 'color-mix(in srgb, #f52a65 14%, #e1e2e7)',
      '--act-ink': '#e1e2e7',
      '--raised-hi': 'color-mix(in srgb, #3760bf 12%, #e1e2e7)',
      '--act-fill': '#e0af68',
      '--act-line': '#e0af68',
      '--act-glow': 'color-mix(in srgb, #e0af68 20%, transparent)',
      '--ink-fill': '#3760bf',
      '--ink-on': '#e1e2e7',
      '--ok-bg': 'color-mix(in srgb, var(--ok) 14%, #e1e2e7)',
      '--fuel': '#6e4fe0',
      '--fuel-bg': 'color-mix(in srgb, #6e4fe0 16%, #e1e2e7)',
      '--focus': '#7aa2f7',
      '--focus-bg': '#283457',
    });
  });

  test('the names are the list that removePalette clears', () => {
    expect(Object.keys(paletteVars(dark)).sort()).toEqual([...PALETTE_VARS].sort());
  });

  test('a bad color gives no token', () => {
    for (const bad of ['red', '#fff', '#12345g', 'url(x)', '#1a1b2600', '']) {
      expect(paletteVars({ mode: 'dark', colors: { ...colors, red: bad } })).toEqual({});
    }
    // Any of the ten colors counts, also one that no token uses.
    expect(paletteVars({ mode: 'dark', colors: { ...colors, muted: 'x' } })).toEqual({});
  });
});

describe('parsePalette', () => {
  test('a good body parses and a bad one is null', () => {
    expect(parsePalette({ mode: 'dark', colors, updated_at: 5 })).toEqual(dark);
    expect(parsePalette(null)).toBeNull();
    expect(parsePalette({ mode: 'dim', colors })).toBeNull();
    expect(parsePalette({ mode: 'dark' })).toBeNull();
    expect(parsePalette({ mode: 'dark', colors: { ...colors, red: 'nope' } })).toBeNull();
    const { yellow: _y, ...missing } = colors;
    expect(parsePalette({ mode: 'dark', colors: missing })).toBeNull();
  });
});

describe('apply and remove on a root', () => {
  test('apply sets the properties and the mode; remove clears them', () => {
    const { root, props, dataset } = fakeRoot();
    expect(applyPalette(root, light, 'dark')).toBe(true);
    expect(props.size).toBe(PALETTE_VARS.length);
    expect(props.get('--bg')).toBe('#e1e2e7');
    expect(dataset['theme']).toBe('light');
    removePalette(root, 'dark');
    expect(props.size).toBe(0);
    expect(dataset['theme']).toBe('dark');
  });

  test('a bad color is not applied and the page returns to the given mode', () => {
    const { root, props, dataset } = fakeRoot();
    applyPalette(root, dark);
    const bad: Palette = { mode: 'dark', colors: { ...colors, accent: 'javascript:1' } };
    expect(applyPalette(root, bad, 'light')).toBe(false);
    expect(props.size).toBe(0);
    expect(dataset['theme']).toBe('light');
  });

  test('showPalette needs a document and does nothing without one', () => {
    expect(() => showPalette(dark)).not.toThrow();
    expect(() => showPalette(null)).not.toThrow();
  });
});

describe('reading a palette', () => {
  const body = { mode: 'dark', colors, updated_at: 1 };

  test('the path encodes the station id', () => {
    expect(stationPalettePath('a/b c')).toBe('/api/stations/a%2Fb%20c/palette');
  });

  test('200 gives a palette; 404 and a body that is not a palette give none', async () => {
    expect(await readPalette(async () => body)).toEqual({ kind: 'palette', palette: dark });
    expect(
      await readPalette(async () => {
        throw new ApiError(404, 'no palette', 'no_palette');
      }),
    ).toEqual({ kind: 'none' });
    expect(
      await readPalette(async () => ({ mode: 'dark', colors: { ...colors, red: 'x' } })),
    ).toEqual({
      kind: 'none',
    });
  });

  test('401 ends; another failure is an error', async () => {
    expect(
      await readPalette(async () => {
        throw new ApiError(401, 'login', 'login_required');
      }),
    ).toEqual({ kind: 'ended' });
    expect(
      await readPalette(async () => {
        throw new ApiError(500, 'boom');
      }),
    ).toEqual({ kind: 'error' });
    expect(
      await readPalette(async () => {
        throw new Error('network');
      }),
    ).toEqual({ kind: 'error' });
  });

  test('settle: a palette shows, none returns the default only when one showed, an error keeps', () => {
    const calls: (Palette | null)[] = [];
    const show = (p: Palette | null) => void calls.push(p);
    let ended = 0;
    const onEnded = () => void ended++;
    expect(settlePalette({ kind: 'palette', palette: dark }, false, show, onEnded)).toBe(true);
    expect(settlePalette({ kind: 'error' }, true, show, onEnded)).toBe(true);
    expect(settlePalette({ kind: 'none' }, true, show, onEnded)).toBe(false);
    expect(settlePalette({ kind: 'none' }, false, show, onEnded)).toBe(false);
    expect(settlePalette({ kind: 'ended' }, true, show, onEnded)).toBe(true);
    expect(calls).toEqual([dark, null]);
    expect(ended).toBe(1);
  });
});
