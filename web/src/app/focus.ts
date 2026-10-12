import { type KeyboardEvent, type RefObject, useEffect } from 'react';

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), summary, [tabindex]:not([tabindex="-1"])';

/** Keeps Tab and Shift+Tab inside a dialog. */
export function trapTab(event: KeyboardEvent, root: HTMLElement | null): void {
  if (event.key !== 'Tab' || root === null) return;
  const items = [...root.querySelectorAll<HTMLElement>(FOCUSABLE)];
  const first = items[0];
  const last = items[items.length - 1];
  if (first === undefined || last === undefined) {
    event.preventDefault();
    return;
  }
  const at = document.activeElement;
  if (event.shiftKey && (at === first || !root.contains(at))) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && (at === last || !root.contains(at))) {
    event.preventDefault();
    first.focus();
  }
}

/**
 * While mounted: moves focus to `target` (or the first focusable thing of `root`) and
 * gives the focus back to the element that had it before.
 */
export function useFocusReturn(
  root: RefObject<HTMLElement | null>,
  target?: RefObject<HTMLElement | null>,
): void {
  useEffect(() => {
    const before = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const first = root.current?.querySelector<HTMLElement>(FOCUSABLE) ?? null;
    (target?.current ?? first ?? root.current)?.focus();
    return () => {
      if (before?.isConnected) before.focus();
    };
  }, [root, target]);
}
