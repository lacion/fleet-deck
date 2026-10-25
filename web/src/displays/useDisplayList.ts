// R13 P2c: the list of displays and pending pairings. It loads at once and reads again every 5 s
// while the board is live; a deck without displays (404) is its own state.
import { useCallback, useEffect, useRef, useState } from 'react';
import type { DisplayList } from '../gen';
import { describe } from '../settings/http';
import { type DisplayActions, noDisplays } from './actions';

/** The wait between two reads of the list. */
export const DISPLAY_LIST_MS = 5_000;

export type ListState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'none' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'ready'; readonly list: DisplayList };

export function useDisplayList(
  actions: DisplayActions,
  live: boolean,
  everyMs: number = DISPLAY_LIST_MS,
): { readonly state: ListState; readonly refresh: () => void } {
  const [state, setState] = useState<ListState>({ kind: 'loading' });
  const on = useRef(true);
  const load = useCallback(() => {
    actions.list().then(
      (list) => on.current && setState({ kind: 'ready', list }),
      (e: unknown) => {
        if (!on.current) return;
        if (noDisplays(e)) setState({ kind: 'none' });
        else setState((now) => (now.kind === 'ready' ? now : { kind: 'error', text: describe(e) }));
      },
    );
  }, [actions]);
  useEffect(() => {
    on.current = true;
    load();
    const timer = live ? setInterval(load, everyMs) : null;
    return () => {
      on.current = false;
      if (timer !== null) clearInterval(timer);
    };
  }, [load, live, everyMs]);
  return { state, refresh: load };
}
