import { countText, homeShort, pathParts } from '../ui/format';
import { BUILTIN_WORDS, toolParts } from '../watch/tooltext';

/** Who started a run; `continuation` is the station's restart input (ADR-007 §2.5). */
export function runLabel(origin: string): string {
  if (origin === 'provider') return 'Provider turn';
  if (origin === 'continuation') return 'Station continuation';
  if (origin === 'shell') return 'Shell command';
  return 'You';
}

/**
 * The name of a tool row: the one parser of tool names (`toolParts` of watch/tooltext.ts). An MCP
 * tool (`mcp__<server>__<tool>`) reads as its tool in sentence case, with its server apart
 * (`mcp__chrome-devtools__take_screenshot` → "Take screenshot", "chrome-devtools"; the deck's own
 * server is no server); a built-in id reads as its word (`AskUserQuestion` → "Question",
 * `BUILTIN_WORDS`); any other title stays as it is. The row keeps the raw id in its `title`.
 */
export function toolName(title: string): { name: string; server: string | null } {
  const { name, server } = toolParts(title);
  return { name, server };
}

/**
 * The title of a request line ("Bash rm -rf dist", "AskUserQuestion"): a built-in tool id at its
 * start reads as its word ("Question"); any other title stays as it is.
 */
export function requestTitle(title: string): string {
  const cut = title.indexOf(' ');
  const word = BUILTIN_WORDS.get(cut < 0 ? title : title.slice(0, cut));
  return word === undefined ? title : `${word}${cut < 0 ? '' : title.slice(cut)}`;
}

const firstLine = (text: string): string => text.split('\n')[0] ?? '';

/** The text of a JSON string body; a body that a cut left with a broken escape at its end loses it. */
function unescapeJson(body: string): string {
  for (const text of [body, body.replace(/\\(?:u[0-9a-fA-F]{0,3})?(…?)$/, '$1')]) {
    try {
      return JSON.parse(`"${text}"`) as string;
    } catch {
      // try the next form
    }
  }
  return body;
}

type JsonEntry = {
  /** The key of the value; in an array, the key of the array. */
  readonly key: string;
  readonly value: string;
  /** 1 for a value of the outer object. */
  readonly depth: number;
  readonly inArray: boolean;
  readonly isString: boolean;
  /** The key of the outer object under which the value sits. */
  readonly top: string;
};

/**
 * The plain values (strings, numbers, booleans) of a JSON object text at every depth, with their
 * keys, in their order. The station cuts a long input at 200 characters (`…`): such a text gives
 * the values before the cut, the last one with its `…`.
 */
function jsonEntries(text: string): JsonEntry[] {
  const entries: JsonEntry[] = [];
  const open: ('{' | '[')[] = [];
  let value = false; // after a `:` in an object
  let key = ''; // the last key
  let top = ''; // the last key of the outer object
  const push = (shown: string, isString: boolean) =>
    entries.push({
      key,
      value: shown,
      depth: open.length,
      inArray: open.at(-1) === '[',
      isString,
      top,
    });
  for (let i = 0; i < text.length; i++) {
    const c = text[i] ?? '';
    const inArray = open.at(-1) === '[';
    if (c === '"') {
      let end = i + 1;
      while (end < text.length && text[end] !== '"') end += text[end] === '\\' ? 2 : 1;
      const body = text.slice(i + 1, end);
      if (value || inArray) push(unescapeJson(body), true);
      else {
        key = body;
        if (open.length === 1) top = body;
      }
      value = false;
      i = end;
    } else if (c === '{' || c === '[') {
      open.push(c);
      value = false;
    } else if (c === '}' || c === ']') {
      open.pop();
    } else if (c === ':') {
      value = open.at(-1) === '{';
    } else if (c === ',') {
      value = false;
    } else if ((value || inArray) && /[-\w.]/.test(c)) {
      const token = /^[-\w.+]+/.exec(text.slice(i))?.[0] ?? c;
      if (token !== 'null') push(token, false);
      value = false;
      i += token.length - 1;
    }
  }
  return entries;
}

/** The plain values at the top level of a JSON object text; nested objects and arrays are left out. */
const topValues = (text: string): JsonEntry[] =>
  jsonEntries(text).filter((e) => e.depth === 1 && !e.inArray);

/** The keys of a question one level down (a Claude AskUserQuestion call), in the order of preference. */
const QUESTION_KEYS = ['question', 'header'];

/**
 * A question one level down (`{"questions":[{"header":"Color","question":"Which color?"}]}` →
 * "Which color?"; a text that the station cut before the question gives its header), with
 * " · N questions" when the whole input asks more than one; '' when there is none.
 */
function nestedQuestion(text: string): string {
  const entries = jsonEntries(text).filter((e) => e.depth > 1 && e.isString && !e.inArray);
  for (const name of QUESTION_KEYS) {
    const found = entries.find((e) => e.key === name && e.value.trim() !== '');
    if (found === undefined) continue;
    const shown = found.value.replace(/\s+/g, ' ').trim();
    let count = 0;
    try {
      const list = (JSON.parse(text) as Record<string, unknown> | null)?.[found.top];
      if (Array.isArray(list)) count = list.length;
    } catch {
      // A cut text: the number of questions is not known.
    }
    return count > 1 ? `${shown} · ${countText(count)} questions` : shown;
  }
  return '';
}

/** The keys whose value names a call for the owner, in the order of preference. */
const NAME_KEYS = ['description', 'name', 'title'];

/**
 * The file name of a path in the folder of the agent CLI (`~/.claude/…`: a workflow script, a
 * transcript), where the folders are ids, not words; `null` for any other value. The chat rows,
 * the Activity rows and the wall tiles show such a path the same way.
 */
export function agentFile(value: string): string | null {
  const path = homeShort(value);
  return /^~\/\.claude\/\S+$/.test(path) ? pathParts(path).base : null;
}

/** A value as the row shows it. */
const shownValue = (value: string): string => agentFile(value) ?? value;

/**
 * The input of a tool call as one row. A JSON object (whole, or cut by the station) reads as its
 * name (a `description`, `name` or `title` value), else as its plain values
 * (`{"task_id":"bv93ogg6d"}` → "bv93ogg6d"), else as a question one level down (a Claude
 * AskUserQuestion call), else as nothing: never as JSON text. Any other input is its first line.
 * The expanded row keeps the raw input.
 */
export function argText(input: string): string {
  if (!input.trimStart().startsWith('{')) return firstLine(input);
  const values = topValues(input)
    .map(({ key, value }) => ({ key, value: value.replace(/\s+/g, ' ').trim() }))
    .filter(({ value }) => value !== '');
  for (const name of NAME_KEYS) {
    const named = values.find(({ key }) => key === name);
    if (named !== undefined) return shownValue(named.value);
  }
  return values.length > 0
    ? values.map(({ value }) => shownValue(value)).join(' · ')
    : nestedQuestion(input);
}

/** A duration in the words of the board (DESIGN-SYSTEM §5): "12 s", "2 min", "3 h". */
export function durationText(ms: number): string {
  const seconds = Math.max(1, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds} s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  return `${Math.floor(minutes / 60)} h`;
}

/**
 * The summary of a Thinking row (DESIGN-SYSTEM §4.5): "Thinking…" while it streams, "Thinking ·
 * 12 s" when it is done. The time runs from the first to the last delta (Unix ms); without a
 * span (one delta, or no times) the row says only "Thinking".
 */
export function thinkingText(item: {
  readonly done: boolean;
  readonly started_at?: number;
  readonly ended_at?: number;
}): string {
  if (!item.done) return 'Thinking…';
  const { started_at: start, ended_at: end } = item;
  if (start === undefined || end === undefined || end <= start) return 'Thinking';
  return `Thinking · ${durationText(end - start)}`;
}
