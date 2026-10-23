import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { HistoryItem } from '../gen';
import { ChatView } from './ChatView';
import type { HistoryLine, HistoryView } from './history';
import { HistoryBlock } from './HistoryBlock';

const view = (
  items: HistoryItem[],
  extra: { more?: boolean; loading?: boolean; line?: HistoryLine | null } = {},
): HistoryView => ({
  items,
  more: extra.more ?? false,
  loading: extra.loading ?? false,
  line: extra.line ?? null,
  loadEarlier: () => {},
  retry: () => {},
});

const user: HistoryItem = { id: 'u', at: 0, kind: 'user', text: 'Fix the bug\nplease', tool: null };
const reply: HistoryItem = {
  id: 'a',
  at: 0,
  kind: 'assistant',
  text: 'Done, **bold**.',
  tool: null,
};
const tool = (id: string, summary: string, status: 'ok' | 'failed' | 'unknown'): HistoryItem => ({
  id,
  at: 0,
  kind: 'tool',
  text: '',
  tool: { name: 'Bash', summary, status },
});

const block = (v: HistoryView, gap = false) =>
  renderToStaticMarkup(<HistoryBlock view={v} gap={gap} />);

describe('the history block (DL-189)', () => {
  test('user, assistant and tool items with the title', () => {
    const html = block(view([user, reply, tool('t1', 'ls -la', 'ok')]));
    expect(html).toContain('aria-label="Earlier history"');
    expect(html).toContain(
      '<div class="fd-history-divider" role="separator"><span>Earlier, in the terminal</span></div>',
    );
    // the same shapes as the live chat: a bubble for you with no label, Markdown for the agent
    expect(html).toContain(
      '<div class="fd-chat-run" data-history="true"><div class="fd-chat-bubble"><span class="fd-avatar" data-size="sm" aria-hidden="true"><svg',
    );
    expect(html).toContain('Fix the bug\nplease');
    expect(html).not.toContain('>You<');
    expect(html).toContain('<div class="fd-chat-message" data-history="true">');
    expect(html).toContain('<strong>bold</strong>');
    expect(html).toContain('data-kind="tool"');
    expect(html).toContain('<code>Bash</code>');
    expect(html).toContain('ls -la');
    expect(html).not.toContain('failed');
  });

  test('a failed tool shows failed, and an empty summary shows only the name', () => {
    const failed = block(view([tool('t1', 'make', 'failed')]));
    expect(failed).toContain('<span class="fd-history-failed">failed</span>');
    const bare = block(view([tool('t2', '', 'unknown')]));
    expect(bare).toContain('<code>Bash</code>');
    expect(bare).not.toContain('</code><span>');
    expect(bare).not.toContain('failed');
  });

  test('a shell command item shows its line and its output (B8)', () => {
    const shell: HistoryItem = {
      id: 's1',
      at: 0,
      kind: 'tool',
      text: 'a\nb',
      tool: { name: 'Shell command', summary: 'ls', status: 'ok' },
    };
    const html = block(view([shell]));
    expect(html).toContain('<code>Shell command</code>');
    expect(html).toContain('<span>ls</span>');
    expect(html).toContain(
      '<pre class="fd-chat-output" data-history="true" data-kind="shell-output">a\nb</pre>',
    );
    const bare = block(view([{ ...shell, text: '' }]));
    expect(bare).toContain('<code>Shell command</code>');
    expect(bare).not.toContain('shell-output');
  });

  test('a shell output item shows only the output, and an empty one nothing (B8)', () => {
    const out: HistoryItem = {
      id: 's2',
      at: 0,
      kind: 'tool',
      text: 'done',
      tool: { name: 'Shell output', summary: '', status: 'unknown' },
    };
    const html = block(view([out]));
    expect(html).toContain('data-kind="shell-output"');
    expect(html).toContain('>done</pre>');
    expect(html).not.toContain('data-kind="tool"');
    const empty = block(view([{ ...out, text: '' }]));
    expect(empty).not.toContain('shell-output');
    expect(empty).not.toContain('data-kind="tool"');
  });

  test('the time is a title when the transcript has one', () => {
    const html = block(view([{ ...user, at: Date.UTC(2026, 9, 1, 12) }, reply]));
    expect(html.match(/title="/g)).toHaveLength(1);
  });

  test('the partial line, with no title when there are no items', () => {
    const line = {
      text: 'Some lines of the transcript could not be read.',
      action: null,
    } as const;
    const html = block(view([user], { line }));
    expect(html).toContain('Some lines of the transcript could not be read.');
    expect(html).not.toContain('Try again');
    const alone = block(view([], { line }));
    expect(alone).toContain('Some lines of the transcript could not be read.');
    expect(alone).not.toContain('Earlier, in the terminal');
  });

  test('a retry line has the Try again button', () => {
    const html = block(view([], { line: { text: 'The station is busy.', action: 'retry' } }));
    expect(html).toContain('>Try again</button>');
  });

  test('the gap line', () => {
    expect(block(view([user]), true)).toContain(
      'Older messages of this session are not kept on the deck.',
    );
    expect(block(view([user]), false)).not.toContain('Older messages');
  });

  test('no block for an empty answer with no items', () => {
    expect(block(view([]))).toBe('');
  });
});

describe('the history in the chat (DL-189)', () => {
  const chat = (v: HistoryView) =>
    renderToStaticMarkup(
      <ChatView items={[]} working={false} ready error={null} history={{ view: v, gap: false }} />,
    );

  test('Load earlier messages shows only while there is more', () => {
    expect(chat(view([user], { more: true }))).toContain('Load earlier messages');
    expect(chat(view([user], { more: false }))).not.toContain('Load earlier messages');
    const loading = chat(view([user], { more: true, loading: true }));
    expect(loading).toContain('Loading…');
    expect(loading).toContain('disabled=""');
  });

  test('the empty chat text is gone when there is history', () => {
    expect(chat(view([user]))).not.toContain('No messages yet');
    expect(chat(view([]))).toContain('No messages yet. Send a prompt to start.');
  });
});
