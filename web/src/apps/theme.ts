// B15 (design `docs/v2/design/mcp-apps.md` §4.8.1): the theme variables that the board sends to an
// app. The app cannot read the board's CSS variables, so the bridge sends the computed values as
// literal colors. Pure: it reads through a small interface, so a test needs no DOM.

/** The part of `CSSStyleDeclaration` that the mapping reads. */
export type StyleReader = { getPropertyValue(name: string): string };

/** The theme of the board. */
export type BoardTheme = 'light' | 'dark';

/** The spec variable and the board token that gives its value (§4.8.1). */
export const THEME_MAP: readonly (readonly [spec: string, token: string])[] = [
  ['--color-background-primary', '--bg'],
  ['--color-background-secondary', '--surface'],
  ['--color-background-tertiary', '--raised'],
  ['--color-text-primary', '--text'],
  ['--color-text-secondary', '--dim'],
  ['--color-text-tertiary', '--faint'],
  ['--color-border-primary', '--border'],
  ['--color-border-secondary', '--border-soft'],
  ['--color-text-danger', '--hazard'],
  ['--color-background-danger', '--hazard-bg'],
  ['--color-text-success', '--ok'],
  ['--color-background-success', '--ok-bg'],
  ['--color-text-info', '--focus'],
  ['--color-background-info', '--focus-bg'],
  ['--color-ring-primary', '--focus'],
  ['--color-text-warning', '--act'],
];

/** The font variables: the names only, the fonts are not served to the app (§4.8.1). */
export const FONT_MAP: readonly (readonly [spec: string, token: string])[] = [
  ['--font-sans', '--font-ui'],
  ['--font-mono', '--font-data'],
];

/** `styles.variables` of the host context: literal values of the board tokens. */
export function themeVariables(style: StyleReader): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [spec, token] of [...THEME_MAP, ...FONT_MAP]) {
    const value = style.getPropertyValue(token).trim();
    if (value !== '') out[spec] = value;
  }
  return out;
}

/** The board theme: `data-theme="light"` on the root element, else dark. */
export function themeOf(root: { getAttribute(name: string): string | null }): BoardTheme {
  return root.getAttribute('data-theme') === 'light' ? 'light' : 'dark';
}
