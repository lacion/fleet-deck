import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { Item } from '../session/timeline';
import { ChatView } from './ChatView';
import { Markdown } from './Markdown';

const md = (text: string) => renderToStaticMarkup(<Markdown text={text} />);

describe('Markdown', () => {
  test('renders a GFM table and a task list', () => {
    const html = md('| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n');
    expect(html).toContain('<div class="fd-md">');
    expect(html).toContain('<div class="fd-md-table"><table>');
    expect(html).toContain('<td>1</td>');
    expect(html).toContain('type="checkbox"');
  });

  test('raw HTML does not render as elements', () => {
    const html = md('<script>alert(1)</script>\n\nhello <b>x</b> there');
    expect(html).not.toContain('<script');
    expect(html).not.toContain('<b>');
  });

  test('a javascript: link has no javascript href', () => {
    const html = md('[click](javascript:alert(1))');
    expect(html.toLowerCase()).not.toContain('javascript:');
  });

  test('an image renders as a link and never as an img', () => {
    const html = md('![a cat](https://example.com/cat.png)\n\n![](https://example.com/dog.png)');
    expect(html).not.toContain('<img');
    expect(html).toContain('<a href="https://example.com/cat.png"');
    expect(html).toContain('>a cat</a>');
    expect(html).toContain('>https://example.com/dog.png</a>');
  });

  test('links have target _blank and rel noopener noreferrer', () => {
    const html = md('[site](https://example.com)');
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer"');
  });

  test('a code block has a Copy code icon button', () => {
    const html = md('```\nls -la\n```');
    expect(html).toContain('aria-label="Copy code"');
    expect(html).toContain('ls -la');
  });
});

const view = (props: Partial<Parameters<typeof ChatView>[0]> & { items?: readonly Item[] }) =>
  renderToStaticMarkup(
    <ChatView items={[]} working={false} ready={true} error={null} {...props} />,
  );

const tool = (n: number, status: Extract<Item, { type: 'tool' }>['status'] = 'ok'): Item => ({
  type: 'tool',
  key: `t${n}`,
  node_id: `n${n}`,
  tool_kind: 'shell',
  title: `Bash ${n}`,
  input: 'ls',
  output: '',
  status,
  files: [],
  exit_code: null,
});

describe('ChatView', () => {
  test('shows the empty, loading and error states', () => {
    expect(view({})).toContain('No messages yet. Send a prompt to start.');
    expect(view({ ready: false })).toContain('Loading the conversation…');
    const html = view({ error: 'The station is gone.' });
    expect(html).toContain('role="alert"');
    expect(html).toContain('The station is gone.');
  });

  test('has a labelled conversation log', () => {
    const html = view({});
    expect(html).toContain('aria-label="Conversation"');
    expect(html).toContain('role="log"');
  });

  test('shows the run label', () => {
    const items: Item[] = [
      { type: 'run', key: '1', run_id: 'r1', input: 'fix it', origin: 'continuation' },
      { type: 'run', key: '2', run_id: 'r2', input: 'again', origin: 'board' },
    ];
    const html = view({ items });
    expect(html).toContain('Station continuation');
    // a board run has no label: the bubble is the owner's own message
    expect(html).not.toContain('>You<');
    expect(html.match(/fd-chat-run-label/g)?.length).toBe(1);
    expect(html).toContain('fix it');
    expect(html).toContain('again');
  });

  test('three tools in a row form one group', () => {
    const html = view({ items: [tool(1), tool(2, 'running'), tool(3, 'failed')] });
    expect(html).toContain('3 tool calls · 1 running · 1 failed');
    expect(html).toContain('fd-chat-toolgroup');
    expect(view({ items: [tool(1), tool(2)] })).not.toContain('tool calls');
  });

  test('a turn with a diff shows Review changes only with onOpenChanges', () => {
    const items: Item[] = [
      {
        type: 'turn',
        key: '1',
        status: 'done',
        turn_id: 't1',
        diff: { files_changed: 2, insertions: 5, deletions: 1 },
      },
    ];
    const html = view({ items });
    expect(html).toContain('<span>2 files changed</span>');
    expect(html).toContain('<span class="fd-add">+5</span>');
    expect(html).toContain('<span class="fd-del">−1</span>');
    expect(html).not.toContain('Review changes');
    expect(view({ items, onOpenChanges: () => {} })).toContain('Review changes');
  });

  test('a done turn with 0 changed files renders no diff line', () => {
    const items: Item[] = [
      {
        type: 'turn',
        key: '1',
        status: 'done',
        turn_id: 't1',
        diff: { files_changed: 0, insertions: 0, deletions: 0 },
      },
    ];
    const html = view({ items, onOpenChanges: () => {} });
    expect(html).not.toContain('fd-chat-diff');
    expect(html).not.toContain('Review changes');
  });

  test('a turn with 1 file and no line counts says 1 file changed and hides +0 and −0', () => {
    const items: Item[] = [
      {
        type: 'turn',
        key: '1',
        status: 'done',
        turn_id: 't1',
        diff: { files_changed: 1, insertions: 0, deletions: 0 },
      },
    ];
    const html = view({ items });
    expect(html).toContain('1 file changed');
    expect(html).not.toContain('1 files');
    expect(html).not.toContain('+0');
    expect(html).not.toContain('−0');
  });

  test('a turn with 3 files +5 −2 shows all parts and Review changes', () => {
    const items: Item[] = [
      {
        type: 'turn',
        key: '1',
        status: 'done',
        turn_id: 't1',
        diff: { files_changed: 3, insertions: 5, deletions: 2 },
      },
    ];
    const html = view({ items, onOpenChanges: () => {} });
    expect(html).toContain('3 files changed');
    expect(html).toContain('<span class="fd-add">+5</span>');
    expect(html).toContain('<span class="fd-del">−2</span>');
    expect(html).toContain('Review changes');
  });

  test('a done turn without a diff renders nothing; a failed turn says so', () => {
    const turn = (status: 'done' | 'failed'): Item => ({
      type: 'turn',
      key: '1',
      status,
      turn_id: 't1',
      diff: null,
    });
    expect(view({ items: [turn('done')] })).not.toContain('Turn');
    expect(view({ items: [turn('failed')] })).toContain('Turn failed');
  });

  test('an open approval is one line: Waiting for your answer · title', () => {
    const items: Item[] = [
      {
        type: 'approval',
        key: '1',
        request_id: 'q1',
        title: 'Bash rm -rf dist',
        details: 'rm -rf dist',
        options: [],
        state: 'open',
        outcome: null,
      },
    ];
    const html = view({ items });
    expect(html).toContain('Waiting for your answer · Bash rm -rf dist');
  });

  test('an answered approval is one line with the outcome', () => {
    const items: Item[] = [
      {
        type: 'approval',
        key: '1',
        request_id: 'q1',
        title: 'Bash ls',
        details: null,
        options: [],
        state: 'answered',
        outcome: 'Allow (by owner)',
      },
    ];
    const html = view({ items });
    expect(html).toContain('Bash ls · Allow (by owner)');
    expect(html).not.toContain('Waiting for your answer');
  });

  test('Working… shows unless an unfinished message is last', () => {
    const message = (done: boolean): Item => ({ type: 'message', key: '1', text: 'hi', done });
    expect(view({ items: [message(true)], working: true })).toContain('Working');
    const streaming = view({ items: [message(false)], working: true });
    expect(streaming).not.toContain('Working');
    expect(streaming).toContain('fd-chat-streaming');
  });

  test('reasoning is closed with a Thinking summary', () => {
    const items: Item[] = [{ type: 'reasoning', key: '1', turn_id: 't', text: 'hmm', done: true }];
    const html = view({ items });
    expect(html).toContain('>Thinking</summary>');
    expect(html).not.toContain(' open');
  });
});
