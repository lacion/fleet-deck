// R13 P4 task F2: the token mapping of an Omarchy palette, and how it sets and removes the inline
// properties. The pure parts run without a DOM: the root is a fake object.
import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ApiError } from '../runtime/api';
import type { PaletteColors } from '../gen';
import {
  applyPalette,
  contrast,
  mixHex,
  PALETTE_ALIASES,
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
  colors: {
    ...colors,
    background: '#eff1f5',
    foreground: '#4c4f69',
    accent: '#1e66f5',
    dark_background: '#dce0e8',
    selection: '#acb0be',
    red: '#d20f39',
    yellow: '#df8e1d',
  },
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
  test('a dark palette (Tokyo Night) gives the exact token values', () => {
    expect(paletteVars(dark)).toEqual({
      '--bg': '#1a1b26',
      '--text': '#c0caf5',
      '--code': '#16161e',
      '--surface': '#21222e',
      '--raised': '#272937',
      '--border': '#383b4b',
      '--border-soft': '#2b2d3b',
      '--ctl-edge': '#747a96',
      '--dim': '#a9b2d8',
      '--faint': '#9299bb',
      '--faint-decor': '#5c6179',
      '--hazard': '#f7768e',
      '--act': '#e0af68',
      '--act-bg': '#36302f',
      '--hazard-bg': '#392835',
      '--act-ink': '#1a1b26',
      '--raised-hi': '#2e303f',
      '--act-fill': '#e0af68',
      '--act-line': '#e0af68',
      '--act-glow': '#e0af6833',
      '--ink-fill': '#c0caf5',
      '--ink-on': '#1a1b26',
      '--ok': '#3ddc97',
      '--ok-bg': '#1f3636',
      '--fuel': '#b79cff',
      '--fuel-bg': '#333049',
      '--focus': '#7aa2f7',
      '--focus-bg': '#272e43',
    });
  });

  test('a light palette (Catppuccin Latte) gives the exact token values', () => {
    expect(paletteVars(light)).toEqual({
      '--bg': '#eff1f5',
      '--text': '#4c4f69',
      '--code': '#dce0e8',
      '--surface': '#e8ebef',
      '--raised': '#e2e4ea',
      '--border': '#d2d4dc',
      '--border-soft': '#dfe1e7',
      '--ctl-edge': '#7a7c90',
      '--dim': '#53556f',
      '--faint': '#595c74',
      '--faint-decor': '#aeb0bd',
      '--hazard': '#b30d30',
      '--act': '#7b4e10',
      '--act-bg': '#ede3d7',
      '--hazard-bg': '#ebd1db',
      '--act-ink': '#000000',
      '--raised-hi': '#dbdee4',
      '--act-fill': '#df8e1d',
      '--act-line': '#b27217',
      '--act-glow': '#df8e1d33',
      '--ink-fill': '#4c4f69',
      '--ink-on': '#eff1f5',
      '--ok': '#0a6a44',
      '--ok-bg': '#cfdfdd',
      '--fuel': '#5f43cb',
      '--fuel-bg': '#d9d6f0',
      '--focus': '#1852c4',
      '--focus-bg': '#d1dbf0',
    });
  });

  test('mixHex and contrast follow color-mix(in srgb) and WCAG 2', () => {
    expect(mixHex('#ffffff', 50, '#000000')).toBe('#808080');
    expect(mixHex('#c0caf5', 4, '#1a1b26')).toBe('#21222e');
    expect(contrast('#000000', '#ffffff')).toBeCloseTo(21, 5);
    expect(contrast('#ffffff', '#000000')).toBeCloseTo(21, 5);
    expect(contrast('#777777', '#ffffff')).toBeCloseTo(4.48, 2);
  });

  // DESIGN-SYSTEM §6: text 4.5:1 and control edges 3:1, also with an Omarchy palette. Each of
  // these palettes failed with the fixed mix steps (Latte --act 2.3:1, Rose Pine Dawn --act
  // 1.8:1, Tokyo Night --faint 3.1:1 on --raised), and with the tints of the palette (Latte: every
  // text on the `selection` color under 3.7:1, --hazard on --hazard-bg 4.48:1, --fuel on --fuel-bg
  // 4.34:1; the needs-you outline 2.31:1 on --bg).
  const floor: Record<string, Palette> = {
    'Catppuccin Latte': light,
    'Rose Pine Dawn': {
      mode: 'light',
      colors: {
        ...colors,
        background: '#faf4ed',
        foreground: '#575279',
        accent: '#286983',
        red: '#b4637a',
        yellow: '#ea9d34',
      },
    },
    'Tokyo Night': dark,
  };
  // A text sits on --raised-hi (the surface nearest to it) and on every tint: a tone badge, a
  // tinted row, a selected option.
  const TEXTS = ['--text', '--dim', '--faint', '--act', '--hazard', '--ok', '--fuel', '--focus'];
  const SURFACES = ['--raised-hi', '--act-bg', '--hazard-bg', '--ok-bg', '--fuel-bg', '--focus-bg'];
  for (const [name, palette] of Object.entries(floor)) {
    test(`${name}: every text token is 4.5:1 on --raised-hi and on every tint`, () => {
      const v = paletteVars(palette);
      for (const surface of SURFACES) {
        for (const token of TEXTS) {
          const ratio = contrast(v[token] ?? '', v[surface] ?? '');
          expect({ token, surface, pass: ratio >= 4.5 }).toEqual({ token, surface, pass: true });
        }
      }
    });

    test(`${name}: --ctl-edge and --act-line are 3:1, --act-ink 4.5:1 on the fill`, () => {
      const v = paletteVars(palette);
      const surface = v['--raised-hi'] ?? '';
      expect(contrast(v['--ctl-edge'] ?? '', surface)).toBeGreaterThanOrEqual(3);
      expect(contrast(v['--ctl-edge'] ?? '', v['--bg'] ?? '')).toBeGreaterThanOrEqual(3);
      // The needs-you outline (.fd-glow) on a strip or card on --bg, --raised or its own tint.
      for (const under of ['--bg', '--raised', '--act-bg']) {
        expect({ under, pass: contrast(v['--act-line'] ?? '', v[under] ?? '') >= 3 }).toEqual({
          under,
          pass: true,
        });
      }
      expect(contrast(v['--act-ink'] ?? '', v['--act-fill'] ?? '')).toBeGreaterThanOrEqual(4.5);
      // The three text steps stay apart: --dim reads above --faint.
      expect(contrast(v['--dim'] ?? '', surface)).toBeGreaterThan(
        contrast(v['--faint'] ?? '', surface),
      );
    });
  }

  test('a foreground under 4.5:1 on --raised-hi gives no token', () => {
    // Tokyo Night Day: #3760bf on #e1e2e7 is 4.9:1, but only 4.0:1 on its --raised-hi.
    const low: Palette = {
      mode: 'light',
      colors: { ...colors, background: '#e1e2e7', foreground: '#3760bf' },
    };
    expect(paletteVars(low)).toEqual({});
  });

  test('an upper-case color gives the same lower-case tokens', () => {
    const upper: Palette = {
      mode: 'dark',
      colors: { ...colors, background: '#1A1B26', yellow: '#E0AF68' },
    };
    expect(paletteVars(upper)).toEqual(paletteVars(dark));
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

describe('the alias tokens follow a palette', () => {
  const css = readFileSync(join(import.meta.dir, '..', 'styles', 'tokens.css'), 'utf8');
  /** The declarations of one theme block of tokens.css, by name. */
  const block = (head: string): Map<string, string> => {
    const start = css.indexOf(head);
    const body = css.slice(css.indexOf('{', start) + 1, css.indexOf('\n}', start));
    const out = new Map<string, string>();
    for (const m of body.replace(/\/\*[\s\S]*?\*\//g, '').matchAll(/(--[\w-]+):\s*([^;]+);/g)) {
      out.set(m[1] ?? '', (m[2] ?? '').trim());
    }
    return out;
  };
  const known = new Set<string>([...PALETTE_VARS, ...PALETTE_ALIASES]);

  test('each alias is made of palette tokens in both themes, never a raw color', () => {
    const wrong: string[] = [];
    for (const head of [':root,\n.fd-scope-dark {', '[data-theme="light"] {']) {
      const decls = block(head);
      for (const name of PALETTE_ALIASES) {
        const value = decls.get(name) ?? '';
        const refs = [...value.matchAll(/var\((--[\w-]+)\)/g)].map((m) => m[1] ?? '');
        if (value === '' || /#|rgba?\(|hsla?\(/.test(value) || refs.length === 0) {
          wrong.push(`${head} ${name}: ${value}`);
        }
        for (const ref of refs) if (!known.has(ref)) wrong.push(`${head} ${name} → ${ref}`);
      }
    }
    expect(wrong).toEqual([]);
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
    expect(props.get('--bg')).toBe('#eff1f5');
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
