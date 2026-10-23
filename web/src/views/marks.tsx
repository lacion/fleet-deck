// R6.1 and R6.4 (DL-169): the unread mark and the focus line that the cards, the
// session rows and the watch tiles share.
import type { DeckSession, SessionSummary } from '../gen';

/** The events after the read cursor of this browser's login; absent means none. */
export const unreadOf = (card: DeckSession): number => card.unread ?? 0;

/** "3" in a small info pill, "9+" above nine; `dot`: a 6px --focus dot (strips and sidebar rows,
 * where a bare number reads as a count of something else). The count stays in the name and the
 * title ("3 unread"). Nothing at zero. */
export function UnreadMark({
  count,
  dot = false,
}: {
  readonly count: number;
  readonly dot?: boolean;
}) {
  if (count <= 0) return null;
  const text = `${count} unread`;
  return (
    <span
      className="fd-count fd-unread"
      data-tone="info"
      data-shape={dot ? 'dot' : undefined}
      role="img"
      aria-label={text}
      title={text}
    >
      {dot ? null : count > 9 ? '9+' : count}
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
