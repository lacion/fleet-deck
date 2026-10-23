// The words of a tool call for the owner (DESIGN-SYSTEM §4.1, §4.5): a readable tool name and one
// target, never a raw MCP id or a JSON payload. The Watch tiles (and the wall tiles, which reuse
// them) and the Activity rows use it, so one call reads the same everywhere.
import { agentFile } from '../chat/labels';
import type { ActivityKind, ToolKind } from '../gen';
import { CODE_TOOLS, homeShort } from '../ui/format';

/** How a target reads: a file path (mono, `PathTail`), a command or an id (mono) or words (sans). */
export type TargetForm = 'path' | 'code' | 'words';

export type ToolText = {
  /** The tool name for the owner: "Read", "Take screenshot · chrome-devtools", "Publish artifact". */
  readonly name: string;
  /** The command, the path, the query or the description; '' when there is none. */
  readonly target: string;
  readonly form: TargetForm;
};

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
 * The built-in tools whose id is not a word for the owner: the words of the board for them (a
 * question is "Question" on the fleet strips and the approval card too). The names that already
 * are words (Bash, Read, Edit, Workflow, …) stay as they are.
 */
const BUILT_IN: ReadonlyMap<string, string> = new Map([
  ['AskUserQuestion', 'Question'],
  ['ExitPlanMode', 'Plan'],
  ['TodoWrite', 'To-do list'],
  ['BashOutput', 'Command output'],
  ['KillShell', 'Stop command'],
]);

/**
 * A tool name in its parts for the owner: a built-in id in words (`BUILT_IN`), an MCP tool
 * (`mcp__<server>__<tool>`) as the tool in words and its server apart (a plugin server loses its
 * `plugin_` prefix; the deck's own server is no server: its tools are words of the board). Every
 * other name stays as it is. The one parser of tool names: `toolLabel`, `toolWords` and the chat
 * rows read it.
 */
export function toolParts(name: string): { readonly name: string; readonly server: string | null } {
  const id = name.trim();
  const own = BUILT_IN.get(id);
  if (own !== undefined) return { name: own, server: null };
  const m = MCP.exec(id);
  if (m === null) return { name: id, server: null };
  const server = (m[1] ?? '').replace(/^plugin_/, '');
  return {
    name: words(m[2] ?? ''),
    server: server === '' || server === OWN_SERVER ? null : server,
  };
}

/**
 * The name of a tool for the owner: the tool in words and its MCP server after a dot ("Take
 * screenshot · chrome-devtools"), a built-in id in words ("Question"), every other name as it is.
 */
export function toolLabel(name: string): string {
  const { name: tool, server } = toolParts(name);
  return server === null ? tool : `${tool} · ${server}`;
}

/**
 * The name of a tool without its MCP server ("Take screenshot"): the short form of a tile line,
 * where the server would take the room of the target. The title of the line keeps `toolLabel`.
 */
export function toolWords(name: string): string {
  return toolParts(name).name;
}

/**
 * The word after a call that has no target, so that a line never shows a lone tool name (a lone
 * "Workflow" read as a heading): "running" while it runs, "done" after it, "stopped" when its
 * process ended. A call that failed or that the owner denied has its outcome word instead.
 */
export function statusWord(status: string): string {
  if (status === 'running') return 'running';
  if (status === 'ended') return 'stopped';
  return 'done';
}

/** The keys of a JSON input that name its target, in the order of preference, with their form:
 * a description, a name or a title first (the rule of the chat rows, chat/labels.ts `argText`). */
const TARGET_KEYS: readonly (readonly [string, TargetForm])[] = [
  ['description', 'words'],
  ['name', 'words'],
  ['title', 'words'],
  ['command', 'code'],
  ['file_path', 'path'],
  ['notebook_path', 'path'],
  ['pattern', 'code'],
  ['query', 'words'],
  ['url', 'code'],
  ['path', 'path'],
  ['prompt', 'words'],
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

/** The name in the `meta` of a workflow script: `name: 'ui-audit'` (the quotes of any kind). */
const SCRIPT_NAME = /\bname\s*:\s*\\?['"`]([^'"`\\\n]{1,80})\\?['"`]/;

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
    const file = agentFile(value);
    if (file !== null) return { target: file, form: 'path' };
    if (value !== '') return { target: value, form };
  }
  for (const [key, value] of fields) {
    const line = firstLine(value);
    if (ID_KEY.test(key) && line !== '') return { target: line, form: 'code' };
  }
  // A path in the folder of the agent CLI (the script of a Workflow call): its file name.
  for (const value of fields.values()) {
    const file = agentFile(firstLine(value));
    if (file !== null) return { target: file, form: 'path' };
  }
  // The script of a Workflow call names itself in its `meta` (`name: 'ui-audit'`); a cut input
  // (no whole "script" value) is read as it is, its quotes escaped.
  const script = fields.get('script') ?? (/^\{\s*"script"\s*:/.test(text) ? text : '');
  const named = SCRIPT_NAME.exec(script)?.[1]?.trim() ?? '';
  if (named !== '') return { target: named, form: 'words' };
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

const knownTool = (name: string): boolean =>
  CODE_TOOLS.has(name) || MCP.test(name) || BUILT_IN.has(name);

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
