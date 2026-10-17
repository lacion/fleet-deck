// R6.1 and R6.4 (DL-169): the unread mark and the focus line that the cards, the
// session rows and the watch tiles share.
import type { DeckSession, SessionSummary } from '../gen';

/** The events after the read cursor of this browser's login; absent means none. */
export const unreadOf = (card: DeckSession): number => card.unread ?? 0;

/** "3" in a small pill, "9+" above nine. Nothing at zero. */
export function UnreadMark({ count }: { readonly count: number }) {
  if (count <= 0) return null;
  const text = `${count} unread`;
  return (
    <span className="fd-unread" role="img" aria-label={text} title={text}>
      {count > 9 ? '9+' : count}
    </span>
  );
}

/**
 * The one-line focus that the agent set with `set_focus` (R6.4). The field comes from
 * another worker's event: read it defensively and show nothing when it is absent.
 */
export function focusOf(summary: SessionSummary): string | null {
  const text = (summary as { focus?: string | null }).focus?.trim();
  return text === undefined || text === '' ? null : text;
}

/** The focus as one muted line. */
export function FocusLine({ summary }: { readonly summary: SessionSummary }) {
  const text = focusOf(summary);
  if (text === null) return null;
  return (
    <span className="fd-focus" title={text}>
      {text}
    </span>
  );
}
