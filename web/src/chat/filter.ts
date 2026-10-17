// DL-175: what the chat shows. The filters only choose which items of the timeline
// render; they never change the stored timeline. Pure functions, and the per-browser
// state in localStorage (`fd-chat-filter`).
import { useState } from 'react';
import type { ProviderKind, ToolKind } from '../gen';
import type { Item } from '../session/timeline';

export type Tool = Extract<Item, { type: 'tool' }>;

/** The kinds that a chip toggles. Messages, approvals, errors, plans and chips have no kind. */
export type FilterKey = 'reads' | 'edits' | 'commands' | 'mcp' | 'other' | 'thinking' | 'notices';
export type ToolClass = Extract<FilterKey, 'reads' | 'edits' | 'commands' | 'mcp' | 'other'>;

export const FILTER_KEYS: readonly FilterKey[] = [
  'reads',
  'edits',
  'commands',
  'mcp',
  'other',
  'thinking',
  'notices',
];

export const FILTER_LABEL: Readonly<Record<FilterKey, string>> = {
  reads: 'Reads',
  edits: 'Edits',
  commands: 'Commands',
  mcp: 'MCP',
  other: 'Other tools',
  thinking: 'Thinking',
  notices: 'Notices',
};

export type ChatFilter = {
  readonly show: Readonly<Record<FilterKey, boolean>>;
  /** Fold each run of 2 or more tool rows into one row. */
  readonly compact: boolean;
};

export const DEFAULT_FILTER: ChatFilter = {
  show: {
    reads: true,
    edits: true,
    commands: true,
    mcp: true,
    other: true,
    thinking: true,
    notices: true,
  },
  compact: false,
};

export type FilterCounts = Readonly<Record<FilterKey, number>>;

const norm = (name: string): string => name.toLowerCase().replace(/[^a-z]/g, '');

// The tool names of each provider, normalised (lower case, letters only).
const CLAUDE_NAMES: Readonly<Record<string, ToolClass>> = {
  read: 'reads',
  glob: 'reads',
  grep: 'reads',
  ls: 'reads',
  notebookread: 'reads',
  view: 'reads',
  write: 'edits',
  edit: 'edits',
  multiedit: 'edits',
  notebookedit: 'edits',
  bash: 'commands',
  bashoutput: 'commands',
  killshell: 'commands',
  monitor: 'commands',
};
const CODEX_NAMES: Readonly<Record<string, ToolClass>> = {
  imageview: 'reads',
  viewimage: 'reads',
  filechange: 'edits',
  applypatch: 'edits',
  shell: 'commands',
  exec: 'commands',
  commandexecution: 'commands',
};

const BY_KIND: Readonly<Partial<Record<ToolKind, ToolClass>>> = {
  read: 'reads',
  search: 'reads',
  edit: 'edits',
  shell: 'commands',
  mcp: 'mcp',
};

/**
 * The filter class of a tool. The name decides first (the Claude names, and the Codex
 * titles `Shell` and `Edit`); the timeline's tool kind decides when the name is unknown.
 * A `null` provider matches the names of both providers.
 */
export function classifyTool(
  name: string,
  provider: ProviderKind | null,
  toolKind?: ToolKind,
): ToolClass {
  if (name.startsWith('mcp__')) return 'mcp';
  const key = norm(name);
  const named =
    (provider !== 'codex' ? CLAUDE_NAMES[key] : undefined) ??
    (provider !== 'claude' ? CODEX_NAMES[key] : undefined);
  if (named !== undefined) return named;
  return (toolKind === undefined ? undefined : BY_KIND[toolKind]) ?? 'other';
}

/** The chip of an item, or null when the item always shows. */
function filterKey(item: Item, provider: ProviderKind | null): FilterKey | null {
  switch (item.type) {
    case 'tool':
      return classifyTool(item.title, provider, item.tool_kind);
    case 'reasoning':
      return 'thinking';
    case 'notice':
      return item.tone === 'error' ? null : 'notices';
    default:
      return null;
  }
}

/**
 * The items that the filter shows (also inside subagents), and the number of rows of each
 * kind in the whole tree, hidden or not.
 */
export function applyChatFilter(
  items: readonly Item[],
  filter: ChatFilter,
  provider: ProviderKind | null = null,
): { items: Item[]; counts: FilterCounts } {
  const counts: Record<FilterKey, number> = {
    reads: 0,
    edits: 0,
    commands: 0,
    mcp: 0,
    other: 0,
    thinking: 0,
    notices: 0,
  };
  const walk = (list: readonly Item[]): Item[] => {
    const out: Item[] = [];
    for (const item of list) {
      const key = filterKey(item, provider);
      if (key !== null) counts[key] += 1;
      if (key !== null && !filter.show[key]) continue;
      out.push(item.type === 'subagent' ? { ...item, items: walk(item.items) } : item);
    }
    return out;
  };
  return { items: walk(items), counts };
}

export type CompactRow =
  | { kind: 'item'; item: Item }
  | { kind: 'fold'; items: Item[]; failed: number; summary: string };

const CLASS_ORDER: readonly ToolClass[] = ['reads', 'edits', 'commands', 'mcp', 'other'];
const CLASS_WORD: Readonly<Record<ToolClass, readonly [string, string]>> = {
  reads: ['read', 'reads'],
  edits: ['edit', 'edits'],
  commands: ['command', 'commands'],
  mcp: ['MCP', 'MCP'],
  other: ['other', 'other'],
};

const plural = (n: number, one: string, many: string): string => `${n} ${n === 1 ? one : many}`;

/** "12 tool calls · 5 notes · 7 reads, 3 commands, 2 MCP" for the folded items. */
export function foldSummary(items: readonly Item[], provider: ProviderKind | null = null): string {
  const per: Record<ToolClass, number> = { reads: 0, edits: 0, commands: 0, mcp: 0, other: 0 };
  let tools = 0;
  let notes = 0;
  for (const item of items) {
    if (item.type === 'tool') {
      tools += 1;
      per[classifyTool(item.title, provider, item.tool_kind)] += 1;
    } else {
      notes += 1;
    }
  }
  const kinds = CLASS_ORDER.filter((c) => per[c] > 0).map((c) =>
    plural(per[c], CLASS_WORD[c][0], CLASS_WORD[c][1]),
  );
  return [plural(tools, 'tool call', 'tool calls'), notes > 0 && plural(notes, 'note', 'notes')]
    .concat(kinds.join(', '))
    .filter(Boolean)
    .join(' · ');
}

/** Messages that the compact mode folds: the agent texts of a turn but the last one. */
function foldableNotes(items: readonly Item[]): Set<Item> {
  const out = new Set<Item>();
  // A turn is the stretch after a prompt; a subagent list is one turn.
  let turn: Item[] = [];
  const close = () => {
    const last = turn.findLastIndex((i) => i.type === 'message');
    turn.forEach((item, at) => {
      if (item.type !== 'message' || !item.done) return;
      if (at !== last || turn.slice(at + 1).some((i) => i.type === 'tool')) out.add(item);
    });
    turn = [];
  };
  for (const item of items) {
    if (item.type === 'run') close();
    else turn.push(item);
  }
  close();
  return out;
}

/**
 * Folds runs of consecutive tool rows into one row.
 * - `compact` off: a run of 3 or more tool rows folds.
 * - `compact` on: the agent's intermediate messages fold with the tools, and a run of 2 or
 *   more items that holds a tool folds.
 * A running tool never folds: it stays a row after the fold. Prompts, approvals, errors, plans, artifacts
 * and the last agent message of a turn never fold.
 */
export function compactRuns(
  items: readonly Item[],
  compact: boolean,
  provider: ProviderKind | null = null,
): CompactRow[] {
  const out: CompactRow[] = [];
  const notes = compact ? foldableNotes(items) : new Set<Item>();
  const min = compact ? 2 : 3;
  let run: Item[] = [];
  let running: Item[] = []; // after the fold
  const flush = () => {
    if (run.length >= min && run.some((i) => i.type === 'tool')) {
      const failed = run.filter((i) => i.type === 'tool' && i.status === 'failed').length;
      out.push({ kind: 'fold', items: run, failed, summary: foldSummary(run, provider) });
    } else {
      for (const item of run) out.push({ kind: 'item', item });
    }
    for (const item of running) out.push({ kind: 'item', item });
    run = [];
    running = [];
  };
  for (const item of items) {
    if (item.type === 'tool' && item.status === 'running') {
      running.push(item);
    } else if (item.type === 'tool' || notes.has(item)) {
      run.push(item);
    } else {
      flush();
      out.push({ kind: 'item', item });
    }
  }
  flush();
  return out;
}

export const FILTER_STORAGE_KEY = 'fd-chat-filter';

const store = (): Storage | undefined => {
  try {
    return globalThis.localStorage ?? undefined;
  } catch {
    return undefined;
  }
};

/** The saved filter. A missing, broken or blocked storage gives the default. */
export function readFilter(storage: Storage | undefined = store()): ChatFilter {
  try {
    const raw = storage?.getItem(FILTER_STORAGE_KEY);
    if (!raw) return DEFAULT_FILTER;
    const value = JSON.parse(raw) as { show?: Record<string, unknown>; compact?: unknown } | null;
    const show = { ...DEFAULT_FILTER.show };
    for (const key of FILTER_KEYS) {
      const v = value?.show?.[key];
      if (typeof v === 'boolean') show[key] = v;
    }
    return { show, compact: value?.compact === true };
  } catch {
    return DEFAULT_FILTER;
  }
}

/** Saves the filter; a blocked or full storage keeps it in memory only. */
export function writeFilter(filter: ChatFilter, storage: Storage | undefined = store()): void {
  try {
    storage?.setItem(FILTER_STORAGE_KEY, JSON.stringify(filter));
  } catch {
    // The filter stays in memory.
  }
}

/** The filter of this browser; `initial` replaces the saved one (tests). */
export function useChatFilter(initial?: ChatFilter): [ChatFilter, (next: ChatFilter) => void] {
  const [filter, setFilter] = useState<ChatFilter>(() => initial ?? readFilter());
  return [
    filter,
    (next) => {
      setFilter(next);
      writeFilter(next);
    },
  ];
}
