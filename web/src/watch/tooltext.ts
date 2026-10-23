// The words of a tool call for the owner (DESIGN-SYSTEM §4.1, §4.5): a readable tool name and one
// target, never a raw MCP id or a JSON payload. The Watch tiles (and the wall tiles, which reuse
// them) and the Activity rows use it, so one call reads the same everywhere.
import type { ActivityKind, ToolKind } from '../gen';
import { homeShort } from '../ui/format';

/** How a target reads: a file path (mono, `PathTail`), a command or an id (mono) or words (sans). */
export type TargetForm = 'path' | 'code' | 'words';

export type ToolText = {
  /** The tool name for the owner: "Read", "Take screenshot · chrome-devtools", "Publish artifact". */
  readonly name: string;
  /** The command, the path, the query or the description; '' when there is none. */
  readonly target: string;
  readonly form: TargetForm;
};

/** The tools whose input is a command or a path (the list of `ui/format.ts` `toolParts`). */
const CODE_TOOLS: ReadonlySet<string> = new Set([
  'Bash',
  'Shell',
  'shell',
  'exec_command',
  'Read',
  'Edit',
  'MultiEdit',
  'Write',
  'NotebookEdit',
  'Grep',
  'Glob',
  'LS',
  'WebFetch',
  'apply_patch',
  // The shell tools that watch or stop a command: their input is a command or a shell id.
  'Monitor',
  'BashOutput',
  'KillShell',
]);

/** The tools whose input is one file. */
const FILE_TOOLS: ReadonlySet<string> = new Set([
  'Read',
  'Edit',
  'MultiEdit',
  'Write',
  'NotebookEdit',
]);

const MCP = /^mcp__(.+?)__(.+)$/;

/** The deck's own MCP server: its tools are words of the board, so they need no server name. */
const OWN_SERVER = 'fleetdeck';

/** `publish_artifact`, `takeScreenshot`, `list-pages` → `Publish artifact`, `Take screenshot`, … */
function words(id: string): string {
  const text = id
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .replace(/[_-]+/g, ' ')
    .trim()
    .toLowerCase();
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/**
 * The name of a tool for the owner: `mcp__<server>__<tool>` gives the tool in words and the server
 * after a dot ("Take screenshot · chrome-devtools"; a plugin server loses its `plugin_` prefix).
 * A tool of the deck's own server is the words only ("Publish artifact"). Every other name stays as
 * it is.
 */
export function toolLabel(name: string): string {
  const m = MCP.exec(name);
  if (m === null) return name;
  const server = (m[1] ?? '').replace(/^plugin_/, '');
  const tool = words(m[2] ?? '');
  return server === '' || server === OWN_SERVER ? tool : `${tool} · ${server}`;
}

/**
 * The name of a tool without its MCP server ("Take screenshot"): the short form of a tile line,
 * where the server would take the room of the target. The title of the line keeps `toolLabel`.
 */
export function toolWords(name: string): string {
  const m = MCP.exec(name.trim());
  return m === null ? name.trim() : words(m[2] ?? '');
}

/** The keys of a JSON input that name its target, in the order of preference, with their form. */
const TARGET_KEYS: readonly (readonly [string, TargetForm])[] = [
  ['description', 'words'],
  ['command', 'code'],
  ['file_path', 'path'],
  ['notebook_path', 'path'],
  ['pattern', 'code'],
  ['query', 'words'],
  ['url', 'code'],
  ['path', 'path'],
  ['prompt', 'words'],
  ['name', 'words'],
  ['title', 'words'],
];

/** An id key (`task_id`, `shell_id`, `id`): its value is an id, in mono. */
const ID_KEY = /^(?:\w+_)?id$/;

const firstLine = (text: string): string =>
  text
    .split('\n')
    .map((l) => l.trim())
    .find((l) => l !== '') ?? '';

/** The string values of a JSON object text. A cut text (the deck and the station cut long inputs)
 * does not parse: its complete `"key":"value"` pairs still count. */
function jsonStrings(text: string): ReadonlyMap<string, string> {
  const out = new Map<string, string>();
  try {
    const value: unknown = JSON.parse(text);
    if (value !== null && typeof value === 'object' && !Array.isArray(value)) {
      for (const [k, v] of Object.entries(value)) if (typeof v === 'string') out.set(k, v);
      return out;
    }
  } catch {
    // A cut text: read the pairs one by one.
  }
  for (const m of text.matchAll(/"([A-Za-z_][\w-]*)"\s*:\s*"((?:[^"\\]|\\.)*)"/g)) {
    const key = m[1];
    const raw = m[2];
    if (key === undefined || raw === undefined || out.has(key)) continue;
    try {
      out.set(key, JSON.parse(`"${raw}"`) as string);
    } catch {
      out.set(key, raw);
    }
  }
  return out;
}

/** A text that is one absolute or home path ("/a/b.ts", "~/x"), with no space. */
const PATH_LIKE = /^(?:~|\.{1,2})?\/\S*$/;

/**
 * The target of a tool input. A JSON object input (it starts with `{`) gives the first known string
 * field (a description, a command, a path, a query, …) or an id, else nothing: never the JSON text.
 * Any other input gives its first line.
 */
export function toolTarget(input: string): { readonly target: string; readonly form: TargetForm } {
  const text = input.trim();
  if (!text.startsWith('{')) {
    const line = firstLine(text);
    return { target: line, form: PATH_LIKE.test(line) ? 'path' : 'words' };
  }
  const fields = jsonStrings(text);
  for (const [key, form] of TARGET_KEYS) {
    const value = firstLine(fields.get(key) ?? '');
    if (value !== '') return { target: value, form };
  }
  for (const [key, value] of fields) {
    const line = firstLine(value);
    if (ID_KEY.test(key) && line !== '') return { target: line, form: 'code' };
  }
  return { target: '', form: 'words' };
}

/**
 * A tool call for the owner. `hint` is what a timeline row knows: its kind and its files (the
 * chat's rule: a read or edit call, or one file that is the whole input, is a path).
 */
export function toolText(
  name: string,
  input: string,
  hint: { readonly kind?: ToolKind; readonly files?: readonly string[] } = {},
): ToolText {
  const label = toolLabel(name.trim());
  const json = input.trim().startsWith('{');
  const { target, form } = toolTarget(input);
  if (target === '' || json) return { name: label, target, form };
  const { kind, files } = hint;
  const oneFile = files !== undefined && files.length === 1 && input.trim() === files[0];
  const fileTarget = FILE_TOOLS.has(name) && !/\s/.test(target);
  if (kind === 'read' || kind === 'edit' || oneFile || fileTarget) {
    return { name: label, target, form: 'path' };
  }
  if (form === 'path') return { name: label, target, form };
  const code =
    kind === 'shell' ||
    kind === 'search' ||
    CODE_TOOLS.has(name) ||
    (kind === 'web' && /^https?:\/\//.test(target));
  return { name: label, target, form: code ? 'code' : 'words' };
}

/** A path for the eye: the home folder of a user ("/home/ana/", "/Users/ana/") becomes "~/"
 * (`homeShort` of ui/format, DESIGN-SYSTEM §5). */
export const homePath = (path: string): string => homeShort(path);

/** How a finished call of an activity row ended, when it did not end well. */
export type ToolOutcome = 'failed' | 'denied' | null;

export type ActivityTool = ToolText & {
  /** The words before the tool name of a request row ("Needs you:", "Permission:"); '' for a call. */
  readonly lead: string;
  readonly outcome: ToolOutcome;
};

const OUTCOME = / \((ok|failed|denied)\)$/;
const REQUEST_LEAD = /^((?:Permission|Needs you): )/;
const COLON_CALL = /^([A-Za-z_][\w-]*): (\S.*)$/s;
const SPACE_CALL = /^([A-Za-z_][\w-]*) (\S.*)$/s;
const NAME_ONLY = /^[A-Za-z_][\w-]*$/;

const knownTool = (name: string): boolean => CODE_TOOLS.has(name) || MCP.test(name);

/**
 * A tool or request row of the activity stream in its parts. The deck writes a finished call as
 * `Title: input (ok)` (or `Title (failed)`), a request as `Needs you: title`. The trailing "(ok)"
 * goes; "failed" and "denied" become the outcome. A request row splits only for a known tool, so
 * "Needs you: Pick a database" stays words. `null`: the row is words only.
 */
export function activityTool(kind: ActivityKind, text: string): ActivityTool | null {
  if (kind !== 'tool' && kind !== 'approval') return null;
  const outcomeMatch = kind === 'tool' ? OUTCOME.exec(text) : null;
  const word = outcomeMatch?.[1];
  const outcome: ToolOutcome = word === 'failed' || word === 'denied' ? word : null;
  let rest = outcomeMatch === null ? text : text.slice(0, outcomeMatch.index);
  let lead = '';
  if (kind === 'approval') {
    const m = REQUEST_LEAD.exec(rest);
    if (m === null) return null;
    lead = (m[1] ?? '').trimEnd();
    rest = rest.slice(m[0].length);
  }
  const colon = COLON_CALL.exec(rest);
  const space = colon === null ? SPACE_CALL.exec(rest) : null;
  let name: string;
  let input = '';
  if (colon !== null && (kind === 'tool' || knownTool(colon[1] ?? ''))) {
    name = colon[1] ?? '';
    input = colon[2] ?? '';
  } else if (space !== null && knownTool(space[1] ?? '')) {
    name = space[1] ?? '';
    input = space[2] ?? '';
  } else if (NAME_ONLY.test(rest) && (kind === 'tool' || knownTool(rest))) {
    name = rest;
  } else if (kind === 'tool' && rest.trim() !== '') {
    // A call without a title ("Tool call (ok)"): the words are its name.
    return { name: rest.trim(), target: '', form: 'words', lead, outcome };
  } else {
    return null;
  }
  return { ...toolText(name, input), lead, outcome };
}
