import { useEffect } from 'react';

/** How many overlays (dialogs, menus that cover the board, the drawer) are open now. */
let open = 0;

/** Marks one overlay as open; the returned function marks it closed. */
export function enterOverlay(): () => void {
  open += 1;
  let released = false;
  return () => {
    if (released) return;
    released = true;
    open -= 1;
  };
}

/** True while any overlay is open: the board hotkeys are off (UI-DESIGN "Esc and overlay order"). */
export const overlayOpen = (): boolean => open > 0;

/** Every overlay component calls this once: the board hotkeys stay off while it is mounted. */
export function useOverlay(): void {
  useEffect(enterOverlay, []);
}
