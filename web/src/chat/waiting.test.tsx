import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { Item } from '../session/timeline';
import { ChatView } from './ChatView';
import { hasOpenRequest, pairWaiting } from './waiting';

type Tool = Extract<Item, { type: 'tool' }>;

const tool = (patch: Partial<Tool> = {}): Tool => ({
  type: 'tool',
  key: 't1',
  node_id: 'node-9',
  tool_kind: 'shell',
  title: 'Bash',
  input: 'cargo nextest run',
  output: '',
  status: 'running',
  files: [],
  exit_code: null,
  ...patch,
});

const request = (state: 'open' | 'answered' = 'open', title = 'Bash'): Item => ({
  type: 'approval',
  key: 'a1',
  request_id: 'req-1',
  title,
  details: 'cargo nextest run',
  options: [],
  state,
  outcome: state === 'open' ? null : 'Allow (by owner)',
});

const view = (items: readonly Item[], working = true) =>
  renderToStaticMarkup(<ChatView items={items} working={working} ready error={null} />);

describe('a call that waits for your answer', () => {
  test('the open request pairs with the running call of the same tool before it', () => {
    const pairs = pairWaiting([tool(), request()]);
    expect(pairs.tools.get('t1')).toBe('Bash');
    expect(pairs.merged.has('a1')).toBe(true);
    // a title with the command still names the tool
    expect(pairWaiting([tool(), request('open', 'Bash rm -rf dist')]).tools.has('t1')).toBe(true);
  });

  test('no pair for a finished call, another tool, or an answered request', () => {
    expect(pairWaiting([tool({ status: 'ok' }), request()]).tools.size).toBe(0);
    expect(pairWaiting([tool({ title: 'Edit' }), request()]).tools.size).toBe(0);
    expect(pairWaiting([tool(), request('answered')]).tools.size).toBe(0);
    expect(pairWaiting([request(), tool()]).tools.size).toBe(0);
  });

  test('one row: the yellow ring and the text at the right, no separate line, no Working', () => {
    const html = view([tool(), request()]);
    expect(html).toContain('data-waiting=""');
    expect(html).toContain('aria-label="waiting" data-status="waiting"');
    expect(html).not.toContain('data-status="working"');
    expect(html).toContain('<span class="fd-vh">Waiting for your answer · Bash</span>');
    expect(html).not.toContain('fd-chat-ask');
    expect(html).not.toContain('fd-chat-working');
  });

  test('a request without its call shown keeps its own line', () => {
    const html = view([request()]);
    expect(html).toContain('fd-chat-ask');
    expect(html).toContain('Waiting for your answer · Bash');
    expect(html).not.toContain('fd-chat-working');
  });

  test('an open request inside a subagent counts', () => {
    const sub: Item = {
      type: 'subagent',
      key: 's1',
      node_id: 'node-1',
      title: 'Agent',
      input: 'Read the docs',
      status: 'running',
      task_id: null,
      tool_use_id: null,
      items: [tool(), request()],
    };
    expect(hasOpenRequest([sub])).toBe(true);
    expect(hasOpenRequest([tool()])).toBe(false);
  });
});

describe('the result of a call', () => {
  test('a finished call says ok at the right', () => {
    expect(view([tool({ status: 'ok' })], false)).toContain(
      '<span class="fd-chat-tool-result">ok</span>',
    );
  });

  test('a file that the target names is not repeated as a badge', () => {
    const path = 'crates/station/src/shim/reaper.rs';
    const edit = tool({ status: 'ok', tool_kind: 'edit', title: 'Edit', input: path });
    expect(view([{ ...edit, files: [path] }], false)).not.toContain('>reaper.rs<');
    expect(view([{ ...edit, files: [path, 'b/other.rs'] }], false)).toContain('>reaper.rs<');
  });
});
