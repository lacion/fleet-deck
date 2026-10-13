import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ToolKind } from '../gen';
import type { Item } from '../session/timeline';
import { ChatView } from './ChatView';
import {
  applyChatFilter,
  type ChatFilter,
  classifyTool,
  compactRuns,
  DEFAULT_FILTER,
  FILTER_STORAGE_KEY,
  foldSummary,
  readFilter,
  writeFilter,
} from './filter';

type Tool = Extract<Item, { type: 'tool' }>;

const tool = (
  n: number,
  title: string,
  tool_kind: ToolKind,
  status: Tool['status'] = 'ok',
): Tool => ({
  type: 'tool',
  key: `t${n}`,
  node_id: `n${n}`,
  tool_kind,
  title,
  input: '',
  output: '',
  status,
  files: [],
  exit_code: null,
});
const read = (n: number, status: Tool['status'] = 'ok') => tool(n, 'Read', 'read', status);
const bash = (n: number, status: Tool['status'] = 'ok') => tool(n, 'Bash', 'shell', status);
const message: Item = { type: 'message', key: 'm', text: 'hello', done: true };
const thinking: Item = { type: 'reasoning', key: 'r', turn_id: 't', text: 'hmm', done: true };
const notice = (tone: 'info' | 'error'): Item => ({
  type: 'notice',
  key: `n-${tone}`,
  text: tone,
  tone,
});
const hide = (...keys: (keyof ChatFilter['show'])[]): ChatFilter => ({
  ...DEFAULT_FILTER,
  show: { ...DEFAULT_FILTER.show, ...Object.fromEntries(keys.map((k) => [k, false])) },
});

describe('classifyTool', () => {
  test('the Claude names', () => {
    for (const n of ['Read', 'Glob', 'Grep', 'LS']) expect(classifyTool(n, 'claude')).toBe('reads');
    for (const n of ['Write', 'Edit', 'MultiEdit', 'NotebookEdit']) {
      expect(classifyTool(n, 'claude')).toBe('edits');
    }
    expect(classifyTool('Bash', 'claude')).toBe('commands');
    expect(classifyTool('mcp__github__list_issues', 'claude')).toBe('mcp');
    expect(classifyTool('WebFetch', 'claude')).toBe('other');
    expect(classifyTool('TodoWrite', 'claude')).toBe('other');
  });

  test('the Codex titles and the tool kind', () => {
    expect(classifyTool('Shell', 'codex', 'shell')).toBe('commands');
    expect(classifyTool('Edit', 'codex', 'edit')).toBe('edits');
    expect(classifyTool('apply_patch', 'codex')).toBe('edits');
    expect(classifyTool('github.list_issues', 'codex', 'mcp')).toBe('mcp');
    expect(classifyTool('Web search', 'codex', 'web')).toBe('other');
  });

  test('an unknown name follows the tool kind; a null provider knows both name sets', () => {
    expect(classifyTool('ViewFile', null, 'read')).toBe('reads');
    expect(classifyTool('Search', null, 'search')).toBe('reads');
    expect(classifyTool('Shell', null)).toBe('commands');
    expect(classifyTool('Bash', null)).toBe('commands');
    // a Claude name does not apply to a Codex session
    expect(classifyTool('Read', 'codex')).toBe('other');
  });
});

describe('applyChatFilter', () => {
  const items: Item[] = [
    message,
    read(1),
    read(2),
    bash(3),
    thinking,
    notice('info'),
    notice('error'),
  ];

  test('the default shows everything and counts the kinds', () => {
    const out = applyChatFilter(items, DEFAULT_FILTER);
    expect(out.items).toHaveLength(items.length);
    expect(out.counts).toEqual({
      reads: 2,
      edits: 0,
      commands: 1,
      mcp: 0,
      other: 0,
      thinking: 1,
      notices: 1,
    });
  });

  test('a hidden kind goes away, with its count kept; messages and errors stay', () => {
    const out = applyChatFilter(items, hide('reads', 'thinking', 'notices'));
    expect(out.items.map((i) => i.key)).toEqual(['m', 't3', 'n-error']);
    expect(out.counts.reads).toBe(2);
    expect(out.counts.thinking).toBe(1);
    expect(out.items).not.toBe(items);
  });

  test('it filters inside a subagent and counts there; the input is not changed', () => {
    const sub: Item = {
      type: 'subagent',
      key: 's',
      node_id: 'ns',
      title: 'Agent',
      input: '',
      status: 'done',
      task_id: null,
      items: [read(1), bash(2)],
    };
    const list = [sub];
    const out = applyChatFilter(list, hide('reads'));
    const first = out.items[0];
    expect(first?.type === 'subagent' && first.items.map((i) => i.key)).toEqual(['t2']);
    expect(out.counts.reads).toBe(1);
    expect(sub.items).toHaveLength(2);
  });
});

describe('compactRuns', () => {
  const say = (n: number, done = true): Item => ({
    type: 'message',
    key: `m${n}`,
    text: 'a',
    done,
  });
  const run: Item = { type: 'run', key: 'r', run_id: 'r', input: 'go', origin: 'board' };
  const kinds = (rows: ReturnType<typeof compactRuns>) => rows.map((r) => r.kind);

  test('off: a run of 3 or more tool rows folds; 2 stay rows; notes stay', () => {
    expect(kinds(compactRuns([message, read(1), bash(2), message], false))).toEqual([
      'item',
      'item',
      'item',
      'item',
    ]);
    const rows = compactRuns([message, read(1), bash(2), read(3), message], false);
    expect(kinds(rows)).toEqual(['item', 'fold', 'item']);
    const fold = rows[1];
    expect(fold?.kind === 'fold' && fold.summary).toBe('3 tool calls · 2 reads, 1 command');
    expect(kinds(compactRuns([say(1), read(1), say(2), read(2), say(3), read(3)], false))).toEqual(
      Array(6).fill('item'),
    );
  });

  test('the summary counts tools by kind and the notes', () => {
    const tools = [
      ...Array.from({ length: 7 }, (_, i) => read(i)),
      ...Array.from({ length: 3 }, (_, i) => bash(10 + i)),
      tool(20, 'mcp__a__b', 'mcp'),
      tool(21, 'mcp__a__c', 'mcp'),
    ];
    expect(foldSummary(tools)).toBe('12 tool calls · 7 reads, 3 commands, 2 MCP');
    expect(foldSummary([say(1), say(2), say(3), say(4), say(5), ...tools])).toBe(
      '12 tool calls · 5 notes · 7 reads, 3 commands, 2 MCP',
    );
    expect(foldSummary([say(1), read(1), read(2)])).toBe('2 tool calls · 1 note · 2 reads');
  });

  test('on: a run of 2 or more folds, with the notes between the tools', () => {
    const rows = compactRuns([run, say(1), read(1), say(2), bash(3), say(4)], true);
    // the last message of the turn stays; the notes and the tools fold
    expect(kinds(rows)).toEqual(['item', 'fold', 'item']);
    const fold = rows[1];
    expect(fold?.kind === 'fold' && fold.summary).toBe(
      '2 tool calls · 2 notes · 1 read, 1 command',
    );
    expect(kinds(compactRuns([read(1), bash(2)], true))).toEqual(['fold']);
    expect(kinds(compactRuns([say(1), read(1)], true))).toEqual(['fold']);
  });

  test('on: the last message of each turn stays, a prompt ends the turn', () => {
    const rows = compactRuns(
      [run, say(1), read(1), say(2), { ...run, key: 'r2' }, say(3), read(2), say(4)],
      true,
    );
    expect(kinds(rows)).toEqual(['item', 'fold', 'item', 'item', 'fold', 'item']);
  });

  test('on: a message alone, an unfinished message, thinking and a lone tool stay rows', () => {
    expect(kinds(compactRuns([run, say(1)], true))).toEqual(['item', 'item']);
    expect(kinds(compactRuns([run, say(1, false), read(1)], true))).toEqual([
      'item',
      'item',
      'item',
    ]);
    expect(kinds(compactRuns([run, thinking, read(1), thinking, read(2)], true))).toEqual([
      'item',
      'item',
      'item',
      'item',
      'item',
    ]);
  });

  test('a failure is counted', () => {
    const rows = compactRuns([read(1), bash(2, 'failed'), bash(3, 'failed')], false);
    expect(rows).toHaveLength(1);
    expect(rows[0]?.kind === 'fold' && rows[0].failed).toBe(2);
  });

  test('a running tool is never folded and stays after the fold', () => {
    const rows = compactRuns([read(1), read(2), read(3), bash(4, 'running')], false);
    expect(kinds(rows)).toEqual(['fold', 'item']);
    const fold = rows[0];
    expect(fold?.kind === 'fold' && fold.items.map((t) => t.key)).toEqual(['t1', 't2', 't3']);
    expect(kinds(compactRuns([read(1), bash(2, 'running')], true))).toEqual(['item', 'item']);
    expect(kinds(compactRuns([read(1), read(2), bash(3, 'running')], true))).toEqual([
      'fold',
      'item',
    ]);
  });
});

describe('the saved filter', () => {
  const memory = (initial: Record<string, string> = {}) => {
    const data = new Map(Object.entries(initial));
    return {
      getItem: (k: string) => data.get(k) ?? null,
      setItem: (k: string, v: string) => void data.set(k, v),
    } as unknown as Storage;
  };
  const broken = {
    getItem: () => {
      throw new Error('blocked');
    },
    setItem: () => {
      throw new Error('full');
    },
  } as unknown as Storage;

  test('it round-trips', () => {
    const storage = memory();
    const next: ChatFilter = { ...hide('reads', 'mcp'), compact: true };
    writeFilter(next, storage);
    expect(readFilter(storage)).toEqual(next);
  });

  test('it falls back to the default for nothing, junk or a broken storage', () => {
    expect(readFilter(memory())).toEqual(DEFAULT_FILTER);
    expect(readFilter(memory({ [FILTER_STORAGE_KEY]: '{not json' }))).toEqual(DEFAULT_FILTER);
    expect(readFilter(memory({ [FILTER_STORAGE_KEY]: 'null' }))).toEqual(DEFAULT_FILTER);
    expect(readFilter(memory({ [FILTER_STORAGE_KEY]: '{"show":{"reads":"no"}}' }))).toEqual(
      DEFAULT_FILTER,
    );
    expect(readFilter(broken)).toEqual(DEFAULT_FILTER);
    expect(readFilter(undefined)).toEqual(DEFAULT_FILTER);
  });

  test('a write to a broken storage does not throw', () => {
    expect(() => writeFilter(DEFAULT_FILTER, broken)).not.toThrow();
  });
});

describe('the bar and the compact row', () => {
  const view = (items: Item[], initialFilter?: ChatFilter) =>
    renderToStaticMarkup(
      <ChatView
        items={items}
        working={false}
        ready={true}
        error={null}
        {...(initialFilter ? { initialFilter } : {})}
      />,
    );

  test('the bar has a chip for each kind, then Compact tools', () => {
    const html = view([message, read(1)], DEFAULT_FILTER);
    for (const label of [
      'Reads',
      'Edits',
      'Commands',
      'MCP',
      'Other tools',
      'Thinking',
      'Notices',
    ]) {
      expect(html).toContain(`>${label}</button>`);
    }
    expect(html.indexOf('Notices')).toBeLessThan(html.indexOf('Compact tools'));
    expect(html).toContain('aria-pressed="true"');
    expect(html).toMatch(/aria-pressed="false"[^>]*>Compact tools</);
  });

  test('there is no bar for an empty chat', () => {
    expect(view([])).not.toContain('Compact tools');
  });

  test('a hidden kind shows its count on the chip and its rows are gone', () => {
    const html = view([message, read(1), read(2), bash(3)], hide('reads'));
    expect(html).toMatch(/aria-pressed="false"[^>]*>Reads<span[^>]*>2<\/span>/);
    expect(html).toContain('Bash');
    expect(html).not.toContain('>Read<');
  });

  test('Compact tools folds a run, and a run with a failure starts open', () => {
    const compact = { ...DEFAULT_FILTER, compact: true };
    const html = view([read(1), read(2), bash(3)], compact);
    expect(html).toContain('3 tool calls · 2 reads, 1 command');
    expect(html).not.toContain('<details class="fd-chat-toolgroup fd-chat-toolfold" open');
    expect(view([read(1), bash(2)], compact)).toContain('2 tool calls');
    const failed = view([read(1), bash(2, 'failed')], compact);
    expect(failed).toContain('<details class="fd-chat-toolgroup fd-chat-toolfold" open');
    expect(failed).toContain('1 failed');
  });

  test('without Compact tools two tools stay rows, three fold', () => {
    expect(view([read(1), bash(2)], DEFAULT_FILTER)).not.toContain('tool calls');
    expect(view([read(1), bash(2), read(3)], DEFAULT_FILTER)).toContain(
      '3 tool calls · 2 reads, 1 command',
    );
  });

  test('Compact tools folds the narration between tool rows into the run', () => {
    const say = (n: number): Item => ({
      type: 'message',
      key: `m${n}`,
      text: `note ${n}`,
      done: true,
    });
    const items = [say(1), read(1), say(2), bash(3), say(4)];
    const html = view(items, { ...DEFAULT_FILTER, compact: true });
    expect(html).toContain('2 tool calls · 2 notes · 1 read, 1 command');
    expect(html).toContain('note 4');
  });
});
