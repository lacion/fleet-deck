// DL-171: the first visit shows a one-time, dismissible notice that points to the help. The help
// dialog never opens by itself. The key `fd-help-seen` remembers the dismissal in this browser.
import { useState } from 'react';
import { Icon, X } from '../ui/icons';

export const HELP_SEEN_KEY = 'fd-help-seen';
export const HELP_NOTICE_TEXT = 'Press ? for the keys and a short guide.';

type Store = Pick<Storage, 'getItem' | 'setItem'>;

const browserStore = (): Store | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

/** True when the notice is due: the board is live and this browser has not seen it. */
export function helpNoticeDue(live: boolean, store: Store | null = browserStore()): boolean {
  if (!live || store === null) return false;
  try {
    return store.getItem(HELP_SEEN_KEY) !== '1';
  } catch {
    return false;
  }
}

/** Remembers that the notice was seen (dismissed, or the help was opened). */
export function markHelpSeen(store: Store | null = browserStore()): void {
  try {
    store?.setItem(HELP_SEEN_KEY, '1');
  } catch {
    // Storage is blocked: the notice shows again on the next visit.
  }
}

/** The notice and the way to close it from outside (the help dialog counts as seen). */
export function useHelpNotice(live: boolean): {
  readonly show: boolean;
  readonly dismiss: () => void;
} {
  const [gone, setGone] = useState(false);
  const show = !gone && helpNoticeDue(live);
  return {
    show,
    dismiss: () => {
      markHelpSeen();
      setGone(true);
    },
  };
}

export function HelpNotice({ onDismiss }: { readonly onDismiss: () => void }) {
  return (
    <div className="fd-notices">
      <div className="fd-hintnotice" role="status" data-notice="help">
        <span>{HELP_NOTICE_TEXT}</span>
        <button
          type="button"
          className="fd-iconbtn fd-sm"
          aria-label="Dismiss the help notice"
          onClick={onDismiss}
        >
          <Icon of={X} size={14} />
        </button>
      </div>
    </div>
  );
}
