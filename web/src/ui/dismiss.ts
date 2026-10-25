import { type RefObject, useEffect, useRef } from 'react';

/** Anything that can tell whether it holds a node: an element, or a fake in a test. */
type Holder = { contains(node: Node | null): boolean };

/** True when `target` is not inside `ref.current` and not inside `ignore?.current`. */
export function isOutside(
  target: EventTarget | null,
  ref: RefObject<Holder | null>,
  ignore?: RefObject<Holder | null>,
): boolean {
  const node = target as Node | null;
  if (ref.current?.contains(node) === true) return false;
  if (ignore?.current?.contains(node) === true) return false;
  return true;
}

/**
 * While `open`, a pointer press outside `ref` (and outside `ignore`) calls `onDismiss`. The
 * listener is on `document`, in the capture phase, so a press on another popover's trigger
 * still closes this one.
 */
export function useDismiss(
  ref: RefObject<HTMLElement | null>,
  open: boolean,
  onDismiss: () => void,
  ignore?: RefObject<HTMLElement | null>,
): void {
  const latest = useRef(onDismiss);
  latest.current = onDismiss;
  useEffect(() => {
    if (!open) return;
    const onDown = (event: PointerEvent) => {
      if (isOutside(event.target, ref, ignore)) latest.current();
    };
    document.addEventListener('pointerdown', onDown, true);
    return () => document.removeEventListener('pointerdown', onDown, true);
  }, [open, ref, ignore]);
}
