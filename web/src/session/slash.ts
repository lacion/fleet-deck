// B11 (DL-412, DL-413): the command menu of the composer. Replaces matchSlash (DL-140, DL-174).
import type { EventEnvelope, SessionSummary } from '../gen';
import { countText } from '../ui/format';
import type { MenuRow } from './Composer';
import { slashCommands } from './timeline';

/** The most rows of the command menu; more give a status row. */
export const MAX_COMMAND_ROWS = 200;

/** Claude: `/name` in the first word. Codex: `$name` in any word, and `/` in the first word. */
export type CommandProvider = 'claude' | 'codex';

export type CommandList = {
  readonly provider: CommandProvider;
  /** Sorted unique names: commands and skills together. `null`: no `session.slash_commands` yet. */
  readonly names: readonly string[] | null;
  /** The names that are skills (the badge "skill"; Claude only). */
  readonly skills: ReadonlySet<string>;
};

/** Case-insensitive order of two names. */
const byName = (a: string, b: string): number => a.toLowerCase().localeCompare(b.toLowerCase());

/** The list of a session: the summary, else the newest timeline event (DL-174). */
export function commandList(
  s: Pick<SessionSummary, 'provider' | 'slash_commands' | 'skills'>,
  events: readonly EventEnvelope[],
): CommandList {
  const provider: CommandProvider = s.provider === 'codex' ? 'codex' : 'claude';
  const commands = s.slash_commands ?? slashCommands(events);
  const names =
    commands === null && s.skills === undefined
      ? null
      : [...new Set([...(commands ?? []), ...(s.skills ?? [])])].sort(byName);
  return { provider, names, skills: new Set(s.skills ?? []) };
}

/** The command word at the caret: a pick replaces text[start, end). */
export type CommandMatch = {
  readonly trigger: '/' | '$';
  readonly start: number;
  readonly end: number;
  /** The text between the trigger and the caret. */
  readonly query: string;
};

/** The index of the first whitespace character at or after `from`, else the text length. */
function wordEnd(text: string, from: number): number {
  let end = from;
  while (end < text.length && !/\s/.test(text.charAt(end))) end++;
  return end;
}

export function matchCommand(
  text: string,
  cursor: number,
  provider: CommandProvider,
): CommandMatch | null {
  const caret = Math.max(0, Math.min(cursor, text.length));
  // `/`: the first word of the text (after leading whitespace), with the caret inside it.
  let first = 0;
  while (first < text.length && /\s/.test(text.charAt(first))) first++;
  if (text.charAt(first) === '/') {
    const end = wordEnd(text, first);
    if (first < caret && caret <= end) {
      return { trigger: '/', start: first, end, query: text.slice(first + 1, caret) };
    }
  }
  if (provider !== 'codex') return null;
  // `$` (Codex only): a word that starts with `$` and ends at the caret.
  let start = caret;
  while (start > 0 && !/\s/.test(text.charAt(start - 1))) start--;
  if (text.charAt(start) !== '$' || caret <= start) return null;
  return { trigger: '$', start, end: wordEnd(text, caret), query: text.slice(start + 1, caret) };
}

/** Names for a query: prefix, then a part prefix, then contains; each group sorted. */
export function rankNames(names: readonly string[], query: string): string[] {
  const q = query.toLowerCase();
  const prefix: string[] = [];
  const part: string[] = [];
  const inside: string[] = [];
  for (const name of names) {
    const lower = name.toLowerCase();
    if (lower.startsWith(q)) prefix.push(name);
    else if (
      lower
        .split(/[:\-_]/)
        .slice(1)
        .some((p) => p.startsWith(q))
    )
      part.push(name);
    else if (lower.includes(q)) inside.push(name);
  }
  return [...prefix.sort(byName), ...part.sort(byName), ...inside.sort(byName)];
}

const AS_PROMPT = 'The text goes to the agent as a prompt.';
const CODEX_AS_PROMPT = 'The text goes to Codex as a prompt.';
const CODEX_HINT =
  'Codex runs a skill that the prompt names with $. Codex terminal commands such as /compact do not run here.';

/** The rows and the status row of a match; `null`: no menu. */
export function commandMenu(
  list: CommandList,
  match: CommandMatch,
): {
  readonly label: 'Commands' | 'Skills';
  readonly rows: MenuRow[];
  readonly status: string | null;
} | null {
  const codex = list.provider === 'codex';
  const label = codex ? 'Skills' : 'Commands';
  const names = list.names;
  const ranked = names === null ? [] : rankNames(names, match.query);
  // A `$` word can be `$HOME` or `$5`: it opens a menu only with matching skills.
  if (match.trigger === '$' && ranked.length === 0) return null;
  if (ranked.length === 0) {
    const status =
      names === null
        ? codex
          ? `No skill list from Codex yet. ${CODEX_AS_PROMPT}`
          : `No command list from the agent yet. ${AS_PROMPT}`
        : names.length === 0
          ? codex
            ? `Codex reports no skills for this folder. ${CODEX_AS_PROMPT}`
            : `This session has no commands. ${AS_PROMPT}`
          : codex
            ? `No matching skill. ${CODEX_AS_PROMPT}`
            : `No matching command. ${AS_PROMPT}`;
    return { label, rows: [], status };
  }
  const rows: MenuRow[] = ranked.slice(0, MAX_COMMAND_ROWS).map((name) =>
    codex
      ? { key: name, text: `$${name}` }
      : {
          key: name,
          text: `/${name}`,
          ...(list.skills.has(name) ? { badge: 'skill' } : {}),
        },
  );
  const more = ranked.length - rows.length;
  const status =
    more > 0
      ? `${countText(more)} more. Type more letters to filter.`
      : codex && match.trigger === '/'
        ? CODEX_HINT
        : null;
  return { label, rows, status };
}

/** The text and the caret after a pick of `name`. */
export function pickCommand(
  text: string,
  match: CommandMatch,
  name: string,
  provider: CommandProvider,
): { readonly text: string; readonly caret: number } {
  const insert = `${provider === 'codex' ? '$' : '/'}${name}`;
  const after = text.slice(match.end);
  const space = /^\s/.test(after) ? '' : ' ';
  return {
    text: text.slice(0, match.start) + insert + space + after,
    caret: match.start + insert.length + 1,
  };
}
