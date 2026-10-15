import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { HistoryItem, SubagentLog } from '../gen';
import type { Item, Subagent } from '../session/timeline';
import { ChatView } from './ChatView';
import { SubagentWorkView, type SubagentState } from './SubagentWork';

const user: HistoryItem = { id: 'u', at: 0, kind: 'user', text: 'Look at <b>docs</b>', tool: null };
const reply: HistoryItem = {
  id: 'a',
  at: 0,
  kind: 'assistant',
  text: 'Found **two**.',
  tool: null,
};
const bash: HistoryItem = {
  id: 't',
  at: 0,
  kind: 'tool',
  text: '',
  tool: { name: 'Bash', summary: 'ls docs', status: 'ok' },
};

const show = (state: Partial<SubagentState>) =>
  renderToStaticMarkup(
    <SubagentWorkView
      state={{ log: null, loading: false, error: null, ...state }}
      onRun={() => {}}
    />,
  );
const log = (items: HistoryItem[], state: SubagentLog['state']): SubagentLog => ({ items, state });

describe('"Show its work" (DL-191)', () => {
  test('before the first answer there is only the button', () => {
    const html = show({});
    expect(html).toContain('>Show its work</button>');
    expect(html).toContain('fd-ghost fd-sm');
    expect(html).not.toContain('role="log"');
  });

  test('while it loads the button reads Loading and is disabled', () => {
    const html = show({ loading: true });
    expect(html).toMatch(/disabled=""[^>]*>Loading…<\/button>/);
  });

  test('the items show in a log, as text only, with Refresh', () => {
    const html = show({ log: log([user, reply, bash], { kind: 'complete' }) });
    expect(html).toContain('role="log" aria-label="Subagent work"');
    expect(html).toContain('Look at &lt;b&gt;docs&lt;/b&gt;');
    expect(html).not.toContain('<b>docs</b>');
    expect(html).toContain('<strong>two</strong>');
    expect(html).toContain('<code>Bash</code>');
    expect(html).toContain('>Refresh</button>');
    expect(html).not.toContain('Show its work');
  });

  test('a refresh keeps the old items and shows Loading', () => {
    const html = show({ log: log([reply], { kind: 'complete' }), loading: true });
    expect(html).toContain('Found');
    expect(html).toMatch(/disabled=""[^>]*>Loading…<\/button>/);
  });

  test('an empty answer says the subagent wrote nothing yet', () => {
    const html = show({ log: log([], { kind: 'empty' }) });
    expect(html).toContain('The subagent wrote nothing yet.');
    expect(html).not.toContain('role="log"');
  });

  test('partial and unavailable states add the history line', () => {
    expect(show({ log: log([reply], { kind: 'partial', skipped: 2 }) })).toContain(
      'Some lines of the transcript could not be read.',
    );
    const gone = show({ log: log([], { kind: 'unavailable', reason: 'not_found' }) });
    expect(gone).toContain(
      'The earlier history is not available: the transcript file was not found.',
    );
  });

  test('an error shows as an alert and the button reads Show its work again', () => {
    const html = show({ error: 'The station did not answer.' });
    expect(html).toContain('role="alert"');
    expect(html).toContain('The station did not answer.');
    expect(html).toContain('>Show its work</button>');
  });
});

const card = (tool_use_id: string | null): Subagent => ({
  type: 'subagent',
  key: 'k',
  node_id: 'n',
  title: 'Agent',
  input: 'explore',
  status: 'done',
  task_id: null,
  tool_use_id,
  items: [],
});
const chat = (item: Item, loadSubagent?: (() => Promise<SubagentLog>) | null) =>
  renderToStaticMarkup(
    <ChatView
      items={[item]}
      working={false}
      ready={true}
      error={null}
      loadSubagent={loadSubagent}
    />,
  );

describe('the subagent card (DL-191)', () => {
  const load = async () => log([], { kind: 'empty' });

  test('the owner reader and a tool use give the button', () => {
    expect(chat(card('toolu_1'), load)).toContain('Show its work');
  });

  test('a member (no reader) or a card without a tool use shows none', () => {
    expect(chat(card('toolu_1'), null)).not.toContain('Show its work');
    expect(chat(card('toolu_1'))).not.toContain('Show its work');
    expect(chat(card(null), load)).not.toContain('Show its work');
  });
});
