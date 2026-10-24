// DESIGN-SYSTEM §6: in forced colors the page takes the OS color scheme, not the stored choice or
// an Omarchy palette mode (the kept glyph colors are made for their own theme).
import { afterEach, describe, expect, test } from 'bun:test';
import { resolvedTheme, setTheme, setThemeOverride } from './theme';

type Global = { window?: unknown };

const stubMedia = (on: readonly string[]) => {
  (globalThis as Global).window = {
    matchMedia: (query: string) => ({ matches: on.includes(query), addEventListener() {} }),
  };
};

const FORCED = '(forced-colors: active)';
const LIGHT = '(prefers-color-scheme: light)';

afterEach(() => {
  setThemeOverride(null);
  setTheme('system');
  delete (globalThis as Global).window;
});

describe('resolvedTheme', () => {
  test('without forced colors the palette mode, then the choice, decide', () => {
    stubMedia([LIGHT]);
    setTheme('dark');
    expect(resolvedTheme()).toBe('dark');
    setThemeOverride('light');
    expect(resolvedTheme()).toBe('light');
  });

  test('forced colors take the OS color scheme over the choice and the palette mode', () => {
    stubMedia([FORCED, LIGHT]);
    setTheme('dark');
    setThemeOverride('dark');
    expect(resolvedTheme()).toBe('light');
    stubMedia([FORCED]);
    setTheme('light');
    setThemeOverride('light');
    expect(resolvedTheme()).toBe('dark');
  });
});
