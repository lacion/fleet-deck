// DL-141: light, dark or system theme. The choice lives in localStorage; the page shows it
// through `<html data-theme="light|dark">`.
import { useSyncExternalStore } from 'react';

export type ThemeChoice = 'system' | 'light' | 'dark';

const KEY = 'fd-theme';
const QUERY = '(prefers-color-scheme: light)';

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

const media = (): MediaQueryList | null =>
  typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia(QUERY)
    : null;

/** Sets `data-theme` from the stored choice, then tells the page with `fd:theme`. */
export function applyTheme(): void {
  if (typeof document === 'undefined') return;
  const light = current === 'light' || (current === 'system' && media()?.matches === true);
  document.documentElement.dataset['theme'] = light ? 'light' : 'dark';
  window.dispatchEvent(new Event('fd:theme'));
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

/** Applies the theme once and follows the system setting while the choice is system. */
export function initTheme(): void {
  current = readTheme();
  applyTheme();
  media()?.addEventListener('change', () => {
    if (current === 'system') applyTheme();
  });
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
