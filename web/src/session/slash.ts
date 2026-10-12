// DL-140: the slash-command menu of the composer.

/** The most commands the menu shows. */
export const MAX_SLASH_OPTIONS = 8;

/**
 * The commands for the text of the composer: `null` unless the text is a
 * slash and a word without a space. Commands that start with the query come
 * first, then the commands that contain it (case-insensitive).
 */
export function matchSlash(
  text: string,
  commands: readonly string[],
): { query: string; options: string[] } | null {
  const found = /^\/(\S*)$/.exec(text);
  if (!found) return null;
  const query = found[1] ?? '';
  const needle = query.toLowerCase();
  const starts = commands.filter((c) => c.toLowerCase().startsWith(needle));
  const contains = commands.filter(
    (c) => !c.toLowerCase().startsWith(needle) && c.toLowerCase().includes(needle),
  );
  const options = [...new Set([...starts, ...contains])].slice(0, MAX_SLASH_OPTIONS);
  return { query, options };
}
