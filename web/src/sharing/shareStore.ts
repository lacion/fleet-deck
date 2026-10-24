// R11 (DL-166): "Share…" opens one dialog for the whole board. The entry points (the session
// menu, the fleet home, the spaces page) call `openShare`; `ShareHost` renders the dialog.
import { useSyncExternalStore } from 'react';
import type { ShareKind } from '../gen';

export type ShareRequest = {
  readonly kind: ShareKind;
  /** `<station>/<session>` for a session, the space id for a space, empty for the board. */
  readonly target: string;
  /** What the person shares, for the title ("Release review", "Space Alpha"). */
  readonly label: string;
};

let current: ShareRequest | null = null;
let version = 0;
const listeners = new Set<() => void>();
const emit = () => {
  for (const l of listeners) l();
};

/** Opens the share dialog for `request`. */
export function openShare(request: ShareRequest): void {
  current = request;
  emit();
}

export function closeShare(): void {
  if (current === null) return;
  current = null;
  emit();
}

/** The share that the dialog shows now, or `null`. */
export const useShareRequest = (): ShareRequest | null =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => {
        listeners.delete(l);
      };
    },
    () => current,
    () => null,
  );

// A change of the shares (a new link, a revoke) asks the lists that the board keeps to load again.
const changeListeners = new Set<() => void>();

/** Tells the board that the shares changed. */
export function sharesChanged(): void {
  version += 1;
  for (const l of changeListeners) l();
}

/** Changes whenever a share was made or revoked. */
export const useSharesVersion = (): number =>
  useSyncExternalStore(
    (l) => {
      changeListeners.add(l);
      return () => {
        changeListeners.delete(l);
      };
    },
    () => version,
    () => 0,
  );
