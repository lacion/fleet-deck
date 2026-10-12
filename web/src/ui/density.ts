// DL-141: cozy or compact cards. The choice lives in localStorage; the page shows it
// through `<html data-density="compact">` (absent = cozy).
import { useSyncExternalStore } from 'react';

export type Density = 'cozy' | 'compact';

const KEY = 'fd-density';

export function readDensity(): Density {
  try {
    return globalThis.localStorage?.getItem(KEY) === 'compact' ? 'compact' : 'cozy';
  } catch {
    return 'cozy';
  }
}

let current: Density = readDensity();
const listeners = new Set<() => void>();

export function applyDensity(): void {
  if (typeof document === 'undefined') return;
  if (current === 'compact') document.documentElement.dataset['density'] = 'compact';
  else delete document.documentElement.dataset['density'];
}

export function setDensity(density: Density): void {
  current = density;
  try {
    globalThis.localStorage?.setItem(KEY, density);
  } catch {
    // The choice stays in memory.
  }
  applyDensity();
  for (const l of listeners) l();
}

export function initDensity(): void {
  current = readDensity();
  applyDensity();
}

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

export function useDensity(): [Density, (density: Density) => void] {
  const density = useSyncExternalStore(
    subscribe,
    () => current,
    () => current,
  );
  return [density, setDensity];
}
