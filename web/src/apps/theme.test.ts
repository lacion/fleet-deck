// B15 T7: the theme mapping of the app bridge (design §4.8.1).
import { describe, expect, test } from 'bun:test';
import { FONT_MAP, THEME_MAP, themeOf, themeVariables } from './theme';

const tokens: Record<string, string> = {
  '--bg': ' #111821 ',
  '--surface': '#161e29',
  '--raised': '#1c2532',
  '--text': '#e9eef4',
  '--dim': '#a5b0bf',
  '--faint': '#8896a7',
  '--border': '#2a3545',
  '--border-soft': '#212b39',
  '--hazard': '#ff6257',
  '--hazard-bg': '#2c1517',
  '--ok': '#3ddc97',
  '--ok-bg': '#10291f',
  '--focus': '#66b8ff',
  '--focus-bg': '#13253a',
  '--act': '#ffd43b',
  '--font-ui': '"Mona Sans Variable", sans-serif',
  '--font-data': '"JetBrains Mono Variable", monospace',
};
const style = { getPropertyValue: (n: string): string => tokens[n] ?? '' };

describe('the theme variables', () => {
  test('theme variables are literal colors of the tokens', () => {
    const vars = themeVariables(style);
    expect(vars['--color-background-primary']).toBe('#111821');
    expect(vars['--color-background-secondary']).toBe('#161e29');
    expect(vars['--color-background-tertiary']).toBe('#1c2532');
    expect(vars['--color-text-danger']).toBe('#ff6257');
    expect(vars['--color-text-warning']).toBe('#ffd43b');
    expect(vars['--color-ring-primary']).toBe('#66b8ff');
    expect(vars['--font-sans']).toContain('Mona Sans');
    expect(vars['--font-mono']).toContain('JetBrains Mono');
    for (const value of Object.values(vars)) expect(value.includes('var(')).toBe(false);
    expect(Object.keys(vars).length).toBe(THEME_MAP.length + FONT_MAP.length);
  });

  test('a token without a value is left out', () => {
    const vars = themeVariables({ getPropertyValue: () => '' });
    expect(Object.keys(vars)).toEqual([]);
  });

  test('the board theme is light only for data-theme light', () => {
    expect(themeOf({ getAttribute: () => 'light' })).toBe('light');
    expect(themeOf({ getAttribute: () => 'dark' })).toBe('dark');
    expect(themeOf({ getAttribute: () => null })).toBe('dark');
  });
});
