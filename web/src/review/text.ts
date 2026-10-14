// DL-179: small text helpers of the review UI.
import type { ReviewComment } from '../gen';

/** `text` cut to `max` characters in total, with "…" at the end; unchanged when it fits. */
export function cutText(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

/** The first line of `text`. */
export const firstLine = (text: string): string => text.split('\n')[0] ?? '';

/** "3" or "3-5": the lines of a comment. */
export const lineRange = (c: Pick<ReviewComment, 'line_start' | 'line_end'>): string =>
  c.line_end === c.line_start ? `${c.line_start}` : `${c.line_start}-${c.line_end}`;
