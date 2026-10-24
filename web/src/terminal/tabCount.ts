// How many terminal tabs the dock has (moved out of `App.tsx` for the module size rule).
import { useSyncExternalStore } from 'react';
import type { TerminalStore } from './store';

const noSubscribe = () => () => {};

/** How many terminal tabs the dock has. */
export function useTabCount(store: TerminalStore | undefined): number {
  return useSyncExternalStore(
    store?.subscribe ?? noSubscribe,
    () => store?.getState().tabs.length ?? 0,
    () => 0,
  );
}
