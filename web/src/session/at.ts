// DL-174: the `@` file menu of the composer.

/** The most paths the menu shows and the search asks for. */
export const MAX_AT_OPTIONS = 20;

/** The shortest query that the station searches for (`MIN_QUERY_CHARS`). */
export const MIN_AT_QUERY = 2;

/**
 * The `@` word that ends at the cursor: `start` is the index of the `@` and
 * `query` the text between the `@` and the cursor. `null` unless the word
 * starts with `@` (at the start of the text or after a space) and has one
 * character or more after it.
 */
export function matchAt(text: string, cursor: number): { query: string; start: number } | null {
  const end = Math.max(0, Math.min(cursor, text.length));
  let start = end;
  while (start > 0 && !/\s/.test(text.charAt(start - 1))) start--;
  if (text.charAt(start) !== '@' || end - start < 2) return null;
  return { query: text.slice(start + 1, end), start };
}
