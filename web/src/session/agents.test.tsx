// B6: the agent view of the chat tab and the status of a subagent card.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession } from '../gen';
import type { Timeline } from '../runtime/state';
import { station } from '../testing/fleet';
import { summary } from '../testing/summary';
import { claudeCapabilities, envelopes, nestedSubagents, recorded } from '../testing/timeline';
import { sessionActions } from './commands';
import { SessionView } from './SessionView';

const key = { station_id: 'station-a', session_id: 'nested' };

const session = (): DeckSession => ({
  station_id: 'station-a',
  status: 'working',
  summary: summary('nested', 'working', 1, {
    title: 'Review',
    capabilities: claudeCapabilities(),
    mode: 'default',
    current_run: null,
  }),
});

const timeline = (events: Timeline['events']): Timeline => ({
  incarnation: 'inc',
  cursor: 0,
  events,
  freshness: 'live',
  error: null,
  ready: true,
});

const view = (events: Timeline['events'], agent?: string) =>
  renderToStaticMarkup(
    <SessionView
      session={session()}
      stations={[station()]}
      freshness="live"
      timeline={timeline(events)}
      actions={sessionActions(async () => ({}), key, ['read', 'drive', 'approve'])}
      tab="chat"
      {...(agent === undefined ? {} : { agent })}
    />,
  );

describe('the agent view (B6)', () => {
  test('the main chat has the Agents control and the composer', () => {
    const html = view(nestedSubagents(10));
    expect(html).toContain('Agents');
    expect(html).toContain('3 running');
    expect(html).toContain('<textarea');
    expect(html).not.toContain('fd-agent-head');
  });

  test('a session without a subagent or a command has no Agents control', () => {
    const html = view(
      envelopes('nested', [
        { kind: 'run.started', run_id: 'r', input_summary: 'x', origin: 'board' },
      ]),
    );
    expect(html).not.toContain('fd-chat-toolbar');
  });

  test('a chosen subagent shows its own items, the header, the no-input line and no composer', () => {
    const html = view(nestedSubagents(17), 'node-1');
    expect(html).toContain('fd-agent-head');
    expect(html).toContain('‹ Main chat');
    expect(html).toContain('A subagent takes no input. Write in the main chat.');
    expect(html).not.toContain('<textarea');
    // Its own text and its child tool are shown.
    expect(html).toContain('main is small');
    expect(html).toContain('src/main.rs');
    expect(html).not.toContain('No messages yet. Send a prompt to start.');
  });

  test('an unknown agent shows the main chat', () => {
    const html = view(nestedSubagents(10), 'nope');
    expect(html).not.toContain('fd-agent-head');
    expect(html).toContain('<textarea');
  });

  test('a node that is not a subagent shows the main chat', () => {
    const html = view(nestedSubagents(10), 'node-3');
    expect(html).not.toContain('fd-agent-head');
  });

  test('an agent with no items says so', () => {
    const events = recorded('subagent').filter((e) => e.session_seq <= 11);
    const html = view(events, 'node-2');
    expect(html).toContain('fd-agent-head');
    expect(html).toContain('This subagent has no messages yet.');
  });

  test('a subagent card shows the status of its task', () => {
    const html = view(recorded('subagent'));
    expect(html).toContain('data-subagent="node-2"');
    expect(html).toMatch(/data-tone="muted">done</);
  });
});
