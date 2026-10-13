import { useCallback, useEffect, useState } from 'react';

export type Loaded<T> =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'ok'; readonly value: T };

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

/** One read call of the deck: its state, and a `reload` that asks again (the list stays while it loads). */
export function useLoad<T>(
  load: () => Promise<T>,
  deps: readonly unknown[],
): {
  readonly state: Loaded<T>;
  readonly reload: () => void;
  readonly set: (value: T) => void;
} {
  const [state, setState] = useState<Loaded<T>>({ kind: 'loading' });
  const [again, setAgain] = useState(0);
  useEffect(() => {
    let live = true;
    load().then(
      (value) => live && setState({ kind: 'ok', value }),
      (e: unknown) => live && setState({ kind: 'error', text: message(e) }),
    );
    return () => {
      live = false;
    };
  }, [...deps, again]);
  const reload = useCallback(() => setAgain((n) => n + 1), []);
  const set = useCallback((value: T) => setState({ kind: 'ok', value }), []);
  return { state, reload, set };
}
