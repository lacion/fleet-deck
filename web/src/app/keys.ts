import { useEffect, useRef } from 'react';

/** The parts of a keyboard event that the rules read (a real event fits). */
export type KeyLike = {
  readonly key: string;
  readonly isComposing?: boolean | undefined;
  readonly ctrlKey?: boolean | undefined;
  readonly metaKey?: boolean | undefined;
  readonly altKey?: boolean | undefined;
  readonly target?: unknown;
};

export type KeyAction =
  | 'next'
  | 'prev'
  | 'focus'
  | 'new'
  | 'fleet'
  | 'terminals'
  | 'help'
  | 'palette'
  | 'escape';

const EDITABLE = 'input, textarea, select, [contenteditable], .xterm';

const isPalette = (e: KeyLike) =>
  (e.ctrlKey === true || e.metaKey === true) && e.key.toLowerCase() === 'k';

/**
 * A key is not ours while the owner types: IME composition, an input, a textarea,
 * a select, a contenteditable element or a terminal, or a Ctrl/Meta/Alt chord.
 * Mod+K always works.
 */
export function shouldIgnore(e: KeyLike): boolean {
  if (isPalette(e)) return false;
  if (e.isComposing === true) return true;
  if (e.ctrlKey === true || e.metaKey === true || e.altKey === true) return true;
  const target = e.target as { closest?: (selector: string) => unknown } | null | undefined;
  return typeof target?.closest === 'function' && target.closest(EDITABLE) != null;
}

const KEYS: Readonly<Record<string, KeyAction>> = {
  j: 'next',
  k: 'prev',
  '/': 'focus',
  n: 'new',
  f: 'fleet',
  t: 'terminals',
  '?': 'help',
  Escape: 'escape',
};

/** The action of a key press, or null when the key is not ours. */
export function keyAction(e: KeyLike): KeyAction | null {
  if (e.isComposing === true && !isPalette(e)) return null;
  if (isPalette(e)) return 'palette';
  if (shouldIgnore(e)) return null;
  return KEYS[e.key] ?? null;
}

export type KeyHandlers = Readonly<Record<KeyAction, () => void>>;

/** One document listener for the global keys (DL-140 §2.8). Handlers may change every render. */
export function useGlobalKeys(handlers: KeyHandlers): void {
  const latest = useRef(handlers);
  latest.current = handlers;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const action = keyAction(e);
      if (action === null) return;
      e.preventDefault();
      latest.current[action]();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);
}
