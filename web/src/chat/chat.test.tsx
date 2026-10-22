import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ReviewComment, ReviewSummary } from '../gen';
import type { Item } from '../session/timeline';
import { ChatView } from './ChatView';
import { runLabel } from './labels';
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

  test('shows no label for a board run, and a muted line for a provider or continuation run', () => {
    const items: Item[] = [
      { type: 'run', key: '1', run_id: 'r1', input: 'fix it', origin: 'continuation' },
      { type: 'run', key: '2', run_id: 'r2', input: 'again', origin: 'board' },
      { type: 'run', key: '3', run_id: 'r3', input: 'provider turn', origin: 'provider' },
    ];
    const html = view({ items });
    // backlog B2: no bubble and no label for the two background origins
    expect(html).toContain(
      '<p class="fd-chat-line" data-origin="continuation">The station continued the session after a restart.</p>',
    );
    expect(html).toContain(
      '<p class="fd-chat-line" data-origin="provider">The agent started a turn on its own.</p>',
    );
    expect(html).not.toContain('Provider turn');
    expect(html).not.toContain('Station continuation');
    expect(html).not.toContain('fix it');
    expect(html).not.toContain('provider turn');
    // a board run has no label: the bubble is the owner's own message
    expect(html).not.toContain('>You<');
    expect(html).not.toContain('fd-chat-run-label');
    expect(html.match(/fd-chat-bubble/g)?.length).toBe(1);
    expect(html).toContain('again');
  });

  test('a shell run has the label Shell command and no line or bubble (B8)', () => {
    expect(runLabel('shell')).toBe('Shell command');
    const items: Item[] = [{ type: 'run', key: '1', run_id: 'r1', input: '!pwd', origin: 'shell' }];
    const html = view({ items });
    expect(html).not.toContain('!pwd');
    expect(html).not.toContain('fd-chat-run');
    expect(html).not.toContain('fd-chat-line');
  });

  test('three finished tools in a row form one fold; a running tool stays outside', () => {
    const html = view({ items: [tool(1), tool(2), tool(3, 'failed'), tool(4, 'running')] });
    expect(html).toContain('3 tool calls · 3 commands');
    expect(html).toContain('1 failed');
    expect(html).toContain('fd-chat-toolgroup');
    expect(html.indexOf('Bash 4')).toBeGreaterThan(html.indexOf('</details></div></details>'));
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
        node_id: null,
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
        node_id: null,
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

describe('ChatView and review comments (DL-179)', () => {
  const comment: ReviewComment = {
    comment_id: '01J0000000000000000000000A',
    short_id: 'c-abc234',
    thread_id: '01J0000000000000000000000A',
    snapshot: 'spawn:base',
    path: 'src/a.ts',
    side: 'new',
    line_start: 3,
    line_end: 5,
    anchor_text: '',
    anchor_kind: 'diff',
    body: `${'x'.repeat(100)}\nsecond`,
    author_kind: 'owner',
    state: 'sent',
    resolved: false,
    created_at: 1,
    updated_at: 1,
  };
  const sent: ReviewSummary = {
    review_id: '01J0000000000000000000000B',
    short_id: 'r-xyz789',
    principal: 'owner',
    delivery: 'delivered',
    attempts: 1,
    input_id: 'in-1',
    sent_at: 1,
    comment_ids: ['c-abc234', 'c-zzz999'],
    command_id: 'cmd',
  };
  const review = (over: Partial<NonNullable<Parameters<typeof ChatView>[0]['review']>> = {}) => ({
    reviews: [sent],
    byShort: new Map([[comment.short_id, comment]]),
    known: new Set([comment.short_id]),
    onOpen: () => {},
    ...over,
  });
  const run = (input: string, input_id?: string): Extract<Item, { type: 'run' }> => ({
    type: 'run',
    key: 'r1',
    run_id: 'run-1',
    input,
    origin: 'board',
    ...(input_id === undefined ? {} : { input_id }),
  });

  test('a run whose input_id matches a review is a Review card', () => {
    const html = view({
      items: [run('Review r-xyz789\n- c-abc234 src/a.ts L3-5: fix', 'in-1')],
      review: review(),
    });
    expect(html).toContain('class="fd-review-card"');
    expect(html).toContain('Review r-xyz789');
    // The count is the number of comment_ids, even when one is not visible.
    expect(html).toContain('2 comments');
    expect(html).toContain('src/a.ts:3-5');
    // The first line of the body is cut to 80 characters in total.
    expect(html).toContain(`${'x'.repeat(79)}…`);
    expect(html).not.toContain('second</span>');
    expect(html).toContain('Show the text that the agent received');
    expect(html).toContain('<pre class="fd-review-text">Review r-xyz789');
    expect(html).not.toContain('fd-chat-bubble');
  });

  test('one comment says "1 comment"', () => {
    const html = view({
      items: [run('text', 'in-1')],
      review: review({ reviews: [{ ...sent, comment_ids: ['c-abc234'] }] }),
    });
    expect(html).toContain('1 comment<');
  });

  test('a run with the same header text but no matching input_id is plain text', () => {
    const text = 'Review r-xyz789\n- c-abc234 src/a.ts L3-5: fix';
    for (const item of [run(text), run(text, 'in-other')]) {
      const html = view({ items: [item], review: review() });
      expect(html).not.toContain('fd-review-card');
      expect(html).toContain('class="fd-chat-bubble"');
    }
  });

  test('a run shows the whole prompt (text) with its line breaks, not the summary', () => {
    const item: Item = { ...run('first…'), text: 'first paragraph\n\nsecond paragraph\nthird' };
    const html = view({ items: [item] });
    expect(html).toContain(
      '<div class="fd-chat-bubble">first paragraph\n\nsecond paragraph\nthird</div>',
    );
    expect(html).not.toContain('first…');
  });

  test('a 30-line prompt is folded in the chat with Show more', () => {
    const text = Array.from({ length: 30 }, (_, i) => `row ${i + 1}`).join('\n');
    const html = view({ items: [{ ...run('row 1…'), text }] });
    expect(html).toContain('row 12');
    expect(html).not.toContain('row 13');
    expect(html).toContain('>Show more</button>');
  });

  test('an old run without text shows the summary', () => {
    const html = view({ items: [run('the summary')] });
    expect(html).toContain('<div class="fd-chat-bubble">the summary</div>');
    expect(html).not.toContain('Show more');
  });

  test('the review chip path and the Review card use the whole prompt', () => {
    const chips = view({
      items: [{ ...run('short…'), text: 'look at c-abc234\nand c-zzz999' }],
      review: review(),
    });
    expect(chips).toContain('data-comment="c-abc234"');
    expect(chips).toContain('and c-zzz999');
    expect(chips).not.toContain('short…');
    const card = view({
      items: [
        { ...run('Review r-xyz789…', 'in-1'), text: 'Review r-xyz789\n- c-abc234 full body' },
      ],
      review: review(),
    });
    expect(card).toContain('<pre class="fd-review-text">Review r-xyz789\n- c-abc234 full body');
  });

  test('a plain run shows a known token as a chip and keeps an unknown one as text', () => {
    const html = view({ items: [run('look at c-abc234 and c-zzz999')], review: review() });
    expect(html).toContain('data-comment="c-abc234"');
    expect(html).not.toContain('data-comment="c-zzz999"');
    expect(html).toContain('c-zzz999');
  });

  test('a message shows a chip that calls onOpen', () => {
    const items: Item[] = [{ type: 'message', key: 'm', text: 'see [c-abc234]', done: true }];
    const html = view({ items, review: review() });
    expect(html).toContain('<button type="button" class="fd-cchip" data-comment="c-abc234">');
  });

  test('without review nothing changes', () => {
    const items: Item[] = [
      run('look at c-abc234', 'in-1'),
      { type: 'message', key: 'm', text: 'see c-abc234', done: true },
    ];
    const html = view({ items });
    expect(html).not.toContain('fd-cchip');
    expect(html).not.toContain('fd-review-card');
  });
});
