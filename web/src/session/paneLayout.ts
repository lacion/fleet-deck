// The layout state of the session pane (the wide window, the Files side panel) and the reason of
// a composer that is off, moved out of `SessionView.tsx` for the module size rule.
import { useCallback, useState, useSyncExternalStore } from 'react';
import type { SessionActions } from './commands';

const WIDE = '(min-width: 1280px)';
const PANEL_KEY = 'fd-files-panel';

/** True while the window is at least 1280 px wide (the Files side panel needs it). */
export function useWide(): boolean {
  return useSyncExternalStore(
    (notify) => {
      if (typeof window.matchMedia !== 'function') return () => {};
      const query = window.matchMedia(WIDE);
      query.addEventListener('change', notify);
      return () => query.removeEventListener('change', notify);
    },
    () => typeof window.matchMedia !== 'function' || window.matchMedia(WIDE).matches,
    () => true,
  );
}

/** The stored open state of the Files side panel: only a stored `1` opens it; unreadable means closed. */
export function readFilesOpen(storage: Pick<Storage, 'getItem'>): boolean {
  try {
    return storage.getItem(PANEL_KEY) === '1';
  } catch {
    return false;
  }
}

/** The open state of the Files side panel, kept in `localStorage`; the default is closed. */
export function useFilesOpen(): readonly [boolean, () => void] {
  const [open, setOpen] = useState(() => {
    try {
      return readFilesOpen(localStorage);
    } catch {
      return false;
    }
  });
  const toggle = useCallback(() => {
    setOpen((now) => {
      try {
        localStorage.setItem(PANEL_KEY, now ? '0' : '1');
      } catch {
        // the choice stays in memory
      }
      return !now;
    });
  }, []);
  return [open, toggle];
}

/**
 * Why the composer is off, or `null`. A lost session says that its process stopped (its status
 * says "Lost"); an exited one says that it ended.
 */
export function disabledReason(
  flags: { live: boolean; held: boolean; ended: boolean },
  lost: boolean,
  actions: SessionActions | undefined,
): string | null {
  if (!flags.live) return 'Controls are off until the board and the station are live.';
  if (flags.held)
    return 'A terminal holds this session. Return it to board control to send prompts.';
  if (flags.ended) {
    return lost
      ? 'The session was lost: its process stopped. Resume it to send prompts.'
      : 'The session ended. Resume it to send prompts.';
  }
  if (actions?.canDrive !== true) return 'Sending prompts needs the drive scope.';
  return null;
}
