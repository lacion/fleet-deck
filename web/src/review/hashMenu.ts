// DL-179: the `#` menu of the composer: it lists the comments of the session; a pick inserts
// `[c-xxxxxx] ` into the text.
import type { ReviewComment } from '../gen';
import type { MenuRow } from '../session/Composer';
import { cutText, firstLine } from './text';

/** The most comments the menu shows. */
export const MAX_HASH_OPTIONS = 20;

/** One comment of the menu. */
export type CommentOption = { readonly shortId: string; readonly label: string };

/**
 * The `#` word that ends at the cursor: `start` is the index of the `#` and `query` the text
 * between the `#` and the cursor. `null` unless the word starts with `#` (at the start of the text
 * or after a space) and has at least one character after it.
 */
export function matchHash(text: string, cursor: number): { query: string; start: number } | null {
  const end = Math.max(0, Math.min(cursor, text.length));
  let start = end;
  while (start > 0 && !/\s/.test(text.charAt(start - 1))) start--;
  if (text.charAt(start) !== '#' || end - start < 2) return null;
  return { query: text.slice(start + 1, end), start };
}

/** The label of a comment: `<path>:<line_start> <first body line cut to 60>`. */
export const commentLabel = (c: ReviewComment): string =>
  `${c.path}:${c.line_start} ${cutText(firstLine(c.body), 60)}`;

/** Every visible comment, newest first. */
export function commentOptions(comments: Iterable<ReviewComment>): CommentOption[] {
  return [...comments]
    .sort((a, b) => b.created_at - a.created_at)
    .map((c) => ({ shortId: c.short_id, label: commentLabel(c) }));
}

/** The rows for `query`: the comments whose short id or label contains it, at most 20. */
export function hashRows(options: readonly CommentOption[], query: string): MenuRow[] {
  const q = query.toLowerCase();
  return options
    .filter((o) => o.shortId.toLowerCase().includes(q) || o.label.toLowerCase().includes(q))
    .slice(0, MAX_HASH_OPTIONS)
    .map((o) => ({ key: o.shortId, text: o.shortId, badge: o.label }));
}

/**
 * The `#` menu for the text and the caret: `null` while the `/` menu is active (it comes first),
 * without a `#` word, or when no comment matches (no menu, no status row).
 */
export function hashMenu(
  text: string,
  cursor: number,
  options: readonly CommentOption[],
  slashActive: boolean,
): { match: { query: string; start: number }; rows: MenuRow[] } | null {
  if (slashActive) return null;
  const match = matchHash(text, cursor);
  if (match === null) return null;
  const rows = hashRows(options, match.query);
  return rows.length === 0 ? null : { match, rows };
}

/** The text and the caret after a pick: `#<query>` is replaced by `[c-xxxxxx] `. */
export function pickComment(
  text: string,
  cursor: number,
  match: { start: number },
  shortId: string,
): { text: string; caret: number } {
  const insert = `[${shortId}] `;
  return {
    text: text.slice(0, match.start) + insert + text.slice(cursor),
    caret: match.start + insert.length,
  };
}
