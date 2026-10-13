import { useCallback, useEffect, useRef, useState } from 'react';

export type Load<T> =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'ok'; readonly value: T };

export const message = (e: unknown): string =>
  e instanceof Error ? e.message : 'The deck did not answer.';

/**
 * One read from the deck. It reads again when `key` changes, when `reload` is called, and when
 * the window gets focus again (R3.3: no polling). A value that is already shown stays on the
 * screen while the new read runs.
 */
export function useLoad<T>(
  load: (key: string) => Promise<T>,
  key: string,
  initial?: T,
): { readonly state: Load<T>; readonly reload: () => void } {
  const [state, setState] = useState<Load<T>>(
    initial === undefined ? { kind: 'loading' } : { kind: 'ok', value: initial },
  );
  const [again, setAgain] = useState(0);
  const loader = useRef(load);
  loader.current = load;
  useEffect(() => {
    if (again < 0) return undefined;
    let live = true;
    loader.current(key).then(
      (value) => live && setState({ kind: 'ok', value }),
      (e: unknown) => live && setState({ kind: 'error', text: message(e) }),
    );
    return () => {
      live = false;
    };
  }, [key, again]);
  const reload = useCallback(() => setAgain((n) => n + 1), []);
  useEffect(() => {
    if (typeof window === 'undefined') return undefined;
    const onFocus = () => reload();
    window.addEventListener('focus', onFocus);
    return () => window.removeEventListener('focus', onFocus);
  }, [reload]);
  return { state, reload };
}
