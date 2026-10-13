// DL-140: the slash-command menu of the composer.

/** The most commands the menu shows. */
export const MAX_SLASH_OPTIONS = 8;

/** One row of the menu; `skill` is true for a command that is also a skill (DL-174). */
export type SlashOption = { name: string; skill: boolean };

/**
 * The commands for the text of the composer: `null` unless the text is a
 * slash and a word without a space. Commands that start with the query come
 * first, then the commands that contain it (case-insensitive).
 */
export function matchSlash(
  text: string,
  commands: readonly string[],
  skills: readonly string[] = [],
): { query: string; options: SlashOption[] } | null {
  const found = /^\/(\S*)$/.exec(text);
  if (!found) return null;
  const query = found[1] ?? '';
  const needle = query.toLowerCase();
  const starts = commands.filter((c) => c.toLowerCase().startsWith(needle));
  const contains = commands.filter(
    (c) => !c.toLowerCase().startsWith(needle) && c.toLowerCase().includes(needle),
  );
  const skillNames = new Set(skills);
  const options = [...new Set([...starts, ...contains])]
    .slice(0, MAX_SLASH_OPTIONS)
    .map((name) => ({ name, skill: skillNames.has(name) }));
  return { query, options };
}
