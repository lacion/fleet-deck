// §4.5: the chat Show menu sits at the right end of the session tab row (chat.css moves it up
// into the row). The row keeps room for it: its end padding is the measured width from the left
// edge of the menu to the end of the row, so "Show (2 hidden)" never covers the last tab.
import { type RefObject, useLayoutEffect, useRef } from 'react';

/** The Show menu when it sits in the tab row (the same selector as chat.css). */
const IN_ROW_MENU = '.fd-pane-chat > .fd-chat:first-child > .fd-chat-filters';

/** The CSS variable on the tab row that `session.css` adds to its end padding. */
export const TAB_RESERVE = '--fd-tab-reserve';

/** The room that the menu needs at the end of the row, in whole pixels (0 without a menu). */
export function reserveWidth(row: DOMRect, menu: DOMRect | null): number {
  if (menu === null || menu.width === 0) return 0;
  return Math.max(0, Math.ceil(row.right - menu.left));
}

/**
 * Keeps `--fd-tab-reserve` of the tab row equal to the room of the Show menu. The menu comes and
 * goes with a render of the session pane; its width changes with its text ("Show (2 hidden)") and
 * its place with the width of the chat panel: a resize observer follows both.
 */
export function useTabReserve(row: RefObject<HTMLElement | null>): void {
  const watched = useRef<{ observer: ResizeObserver; menu: Element | null } | null>(null);
  // Every render: the menu can appear, go away or be a new element (another chat).
  useLayoutEffect(() => {
    const el = row.current;
    const pane = el?.parentElement;
    if (!el || !pane || typeof ResizeObserver === 'undefined') return;
    const menu = pane.querySelector(IN_ROW_MENU);
    const update = () => {
      const now = watched.current?.menu ?? null;
      const width = reserveWidth(el.getBoundingClientRect(), now?.getBoundingClientRect() ?? null);
      el.style.setProperty(TAB_RESERVE, `${width}px`);
    };
    if (watched.current === null) {
      const observer = new ResizeObserver(update);
      observer.observe(el);
      const panel = pane.querySelector('[role="tabpanel"]');
      if (panel) observer.observe(panel);
      watched.current = { observer, menu: null };
    }
    const current = watched.current;
    if (current.menu === menu) return;
    if (current.menu) current.observer.unobserve(current.menu);
    current.menu = menu;
    if (menu) current.observer.observe(menu);
    update();
  });
  // The observer ends with the pane.
  useLayoutEffect(
    () => () => {
      watched.current?.observer.disconnect();
      watched.current = null;
    },
    [],
  );
}
