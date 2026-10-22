// DL-141: light, dark or system theme. The choice lives in localStorage; the page shows it
// through `<html data-theme="light|dark">`.
import { useSyncExternalStore } from 'react';

export type ThemeChoice = 'system' | 'light' | 'dark';

const KEY = 'fd-theme';
const QUERY = '(prefers-color-scheme: light)';
const FORCED = '(forced-colors: active)';

const isChoice = (v: unknown): v is ThemeChoice => v === 'system' || v === 'light' || v === 'dark';

export function readTheme(): ThemeChoice {
  try {
    const value = globalThis.localStorage?.getItem(KEY);
    return isChoice(value) ? value : 'system';
  } catch {
    return 'system';
  }
}

let current: ThemeChoice = readTheme();
const listeners = new Set<() => void>();

const media = (query: string = QUERY): MediaQueryList | null =>
  typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia(query)
    : null;

// R13 P4: an Omarchy palette (see `ui/palette.ts`) sets the mode of the page while it is applied.
let override: 'light' | 'dark' | null = null;

/**
 * The mode that the page shows: the palette mode while one is applied, else the choice. In forced
 * colors (Windows High Contrast) the OS color scheme wins over both: the glyphs keep their theme
 * colors there (`forced-color-adjust: none`), and a dark-theme glyph on a white `Canvas` almost
 * disappears (DESIGN-SYSTEM §6).
 */
export function resolvedTheme(): 'light' | 'dark' {
  if (media(FORCED)?.matches === true) return media()?.matches === true ? 'light' : 'dark';
  if (override !== null) return override;
  const light = current === 'light' || (current === 'system' && media()?.matches === true);
  return light ? 'light' : 'dark';
}

/** Sets `data-theme` from the palette mode or the stored choice, then tells the page with `fd:theme`. */
export function applyTheme(): void {
  if (typeof document === 'undefined') return;
  document.documentElement.dataset['theme'] = resolvedTheme();
  window.dispatchEvent(new Event('fd:theme'));
}

/** A palette mode overrides the choice; `null` gives the choice back. */
export function setThemeOverride(mode: 'light' | 'dark' | null): void {
  override = mode;
  applyTheme();
}

export function setTheme(choice: ThemeChoice): void {
  current = choice;
  try {
    globalThis.localStorage?.setItem(KEY, choice);
  } catch {
    // The choice stays in memory.
  }
  applyTheme();
  for (const l of listeners) l();
}

// The board preference "Reduce motion" (WCAG 2.2.2): it stops the needs-you glow, the working arc
// and every transition, the same as the OS `prefers-reduced-motion`. The page shows it through
// `<html data-motion="reduce">` (absent = the OS setting decides).
const MOTION_KEY = 'fd-motion';

export function readMotion(): boolean {
  try {
    return globalThis.localStorage?.getItem(MOTION_KEY) === 'reduce';
  } catch {
    return false;
  }
}

let motion = readMotion();
const motionListeners = new Set<() => void>();

function applyMotion(): void {
  if (typeof document === 'undefined') return;
  if (motion) document.documentElement.dataset['motion'] = 'reduce';
  else delete document.documentElement.dataset['motion'];
}

export function setReduceMotion(reduce: boolean): void {
  motion = reduce;
  try {
    if (reduce) globalThis.localStorage?.setItem(MOTION_KEY, 'reduce');
    else globalThis.localStorage?.removeItem(MOTION_KEY);
  } catch {
    // The choice stays in memory.
  }
  applyMotion();
  for (const l of motionListeners) l();
}

const subscribeMotion = (l: () => void) => {
  motionListeners.add(l);
  return () => motionListeners.delete(l);
};

export function useReduceMotion(): [boolean, (reduce: boolean) => void] {
  const reduce = useSyncExternalStore(
    subscribeMotion,
    () => motion,
    () => motion,
  );
  return [reduce, setReduceMotion];
}

/**
 * Applies the theme and the motion preference once, and follows the system setting while the
 * choice is system (or while forced colors are active), and the forced-colors setting.
 */
export function initTheme(): void {
  current = readTheme();
  applyTheme();
  media()?.addEventListener('change', () => {
    if (current === 'system' || media(FORCED)?.matches === true) applyTheme();
  });
  media(FORCED)?.addEventListener('change', applyTheme);
  motion = readMotion();
  applyMotion();
}

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

export function useTheme(): [ThemeChoice, (choice: ThemeChoice) => void] {
  const choice = useSyncExternalStore(
    subscribe,
    () => current,
    () => current,
  );
  return [choice, setTheme];
}
