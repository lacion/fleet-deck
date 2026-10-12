import { useSyncExternalStore } from 'react';

export type Layout = 'desktop' | 'tablet' | 'phone';

const DESKTOP = '(min-width: 1024px)';
const TABLET = '(min-width: 768px)';
const COARSE = '(pointer: coarse)';

/** The layout for the two width queries (DL-140: 1024 px and 768 px). */
export const layoutFor = (desktop: boolean, tablet: boolean): Layout =>
  desktop ? 'desktop' : tablet ? 'tablet' : 'phone';

function subscribeTo(queries: readonly string[]) {
  return (cb: () => void): (() => void) => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return () => {};
    const lists = queries.map((q) => window.matchMedia(q));
    for (const list of lists) list.addEventListener('change', cb);
    return () => {
      for (const list of lists) list.removeEventListener('change', cb);
    };
  };
}

const matches = (query: string): boolean =>
  typeof window !== 'undefined' &&
  typeof window.matchMedia === 'function' &&
  window.matchMedia(query).matches;

const subscribeLayout = subscribeTo([DESKTOP, TABLET]);
const subscribeTouch = subscribeTo([COARSE]);

const layoutNow = (): Layout => {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return 'desktop';
  return layoutFor(matches(DESKTOP), matches(TABLET));
};

/** desktop ≥ 1024 px, tablet ≥ 768 px, else phone. Without a window: desktop. */
export function useLayout(): Layout {
  return useSyncExternalStore(subscribeLayout, layoutNow, () => 'desktop');
}

/** True on a coarse pointer (a touch screen). Without a window: false. */
export function useTouch(): boolean {
  return useSyncExternalStore(
    subscribeTouch,
    () => matches(COARSE),
    () => false,
  );
}
