// B12: the agent view of a workflow agent and the choice of an agent (design `workflow-agents.md` §4.6).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, Event, WorkflowAgent, WorkflowUpdate } from '../gen';
import type { Timeline } from '../runtime/state';
import { type AgentChoiceState, useAgentChoice } from '../session/agentChoice';
import { sessionActions } from '../session/commands';
import { SessionView } from '../session/SessionView';
import { station } from '../testing/fleet';
import { summary } from '../testing/summary';
import { claudeCapabilities, envelopes } from '../testing/timeline';

const key = { station_id: 'station-a', session_id: 'wf' };

const session = (): DeckSession => ({
  station_id: 'station-a',
  status: 'working',
  summary: summary('wf', 'working', 1, {
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

const update = (more: Partial<WorkflowUpdate> = {}): Event => ({
  kind: 'workflow.updated',
  task_id: 't1',
  status: 'running',
  phases: [],
  agents: [],
  running_agents: 0,
  agents_total: 0,
  ...more,
});
const agent = (index: number, more: Partial<WorkflowAgent> = {}): WorkflowAgent => ({
  index,
  label: `Agent ${index}`,
  state: 'running',
  ...more,
});

const EVENTS: Event[] = [
  { kind: 'run.started', run_id: 'run-1', input_summary: 'review', origin: 'board' },
  update({
    name: 'Review',
    phases: [{ index: 1, title: 'Alpha' }],
    agents: [
      agent(0, {
        phase_index: 1,
        label: 'Read files',
        model: 'claude-sonnet-4-5',
        tool_calls: 12,
        tokens: 1500,
        prompt_preview: 'Read the files of src.',
        agent_id: 'abc123',
      }),
      agent(1, {
        phase_index: 1,
        label: 'Check',
        state: 'done',
        attempt: 2,
        result_preview: 'All good.',
      }),
      agent(2, { label: 'Lint', state: 'failed', error: 'timeout after 30 s' }),
      agent(3, { label: 'Bare' }),
    ],
    running_agents: 2,
    agents_total: 4,
  }),
];

const view = (agentParam?: string, tab: 'chat' | 'changes' = 'chat') =>
  renderToStaticMarkup(
    <SessionView
      session={session()}
      stations={[station()]}
      freshness="live"
      timeline={timeline(envelopes('wf', EVENTS))}
      actions={sessionActions(async () => ({}), key, ['read', 'drive', 'approve'])}
      tab={tab}
      {...(agentParam === undefined ? {} : { agent: agentParam })}
    />,
  );

describe('the agent view of a workflow agent (B12)', () => {
  test('the main chat has the Agents control with the workflow agents running', () => {
    const html = view();
    expect(html).toContain('Workflow · Review');
    expect(html).toContain('data-wfagent="t1:0"');
    // The running count includes running workflow agents.
    expect(html).toContain('2 running');
    expect(html).toContain('<textarea');
    expect(html).not.toContain('fd-agent-head');
  });

  test('choosing an agent sets agent=wf:<task>:<index> and shows the facts', () => {
    const calls: (string | null)[] = [];
    const box: { state: AgentChoiceState | undefined } = { state: undefined };
    const Probe = () => {
      box.state = useAgentChoice({
        session: session(),
        subagents: [],
        events: envelopes('wf', EVENTS),
        actions: undefined,
        member: false,
        tab: 'chat',
        agent: undefined,
        onAgent: (value) => calls.push(value),
      });
      return null;
    };
    renderToStaticMarkup(<Probe />);
    box.state?.chooseAgent({ kind: 'workflow_agent', task_id: 't1', index: 2 });
    box.state?.chooseAgent({ kind: 'main' });
    box.state?.chooseAgent({ kind: 'agent', node_id: 'node-1' });
    expect(calls).toEqual(['wf:t1:2', null, 'node-1']);

    const html = view('wf:t1:0');
    expect(html).toContain('fd-agent-head');
    expect(html).toContain('‹ Main chat');
    expect(html).toContain('<span class="fd-agent-title">Read files</span>');
    expect(html).toContain('<dt>Workflow</dt><dd>Review</dd>');
    expect(html).toContain('<dt>Phase</dt><dd>1. Alpha</dd>');
    expect(html).toContain('<dt>Model</dt><dd>Sonnet 4.5</dd>');
    expect(html).toContain('<dt>Tool calls</dt><dd>12</dd>');
    expect(html).toContain('<dt>Tokens</dt><dd>1.5K</dd>');
    // One attempt is not worth a row.
    expect(html).not.toContain('<dt>Attempt</dt>');
    expect(html).toContain(
      '<p class="fd-label">Task</p><p class="fd-agent-text">Read the files of src.</p>',
    );
    // No "Show its work" in this task.
    expect(html).not.toContain('Show its work');
    // The Agents switcher stays in the bar of the view.
    expect(html).toContain('fd-chat-toolbar');
    expect(html).toContain('data-wfagent="t1:0" aria-current="true"');
  });

  test('the result, the error and the attempt are shown when there are some', () => {
    const done = view('wf:t1:1');
    expect(done).toContain('<dt>Attempt</dt><dd>2</dd>');
    expect(done).toContain('<p class="fd-label">Result</p><p class="fd-agent-text">All good.</p>');
    expect(done).not.toContain('Task</p>');
    const failed = view('wf:t1:2');
    expect(failed).toContain('<p class="fd-label">Error</p>');
    expect(failed).toContain('data-tone="hazard">timeout after 30 s</p>');
    expect(failed).toContain('data-tone="hazard">failed<');
    // An agent that has no model, no counts, no task and no result shows only what it has.
    const bare = view('wf:t1:3');
    expect(bare).toContain('<span class="fd-agent-title">Bare</span>');
    expect(bare).toContain('<dt>Workflow</dt>');
    // Only the facts of the agent: the Properties panel of the session always has a Model row.
    const facts = /<dl class="fd-agent-facts">.*?<\/dl>/.exec(bare)?.[0] ?? '';
    expect(facts).toContain('<dt>Workflow</dt>');
    for (const missing of [
      '<dt>Phase</dt>',
      '<dt>Model</dt>',
      '<dt>Tool calls</dt>',
      '<dt>Tokens</dt>',
    ]) {
      expect(facts).not.toContain(missing);
    }
    expect(bare).not.toContain('Task</p>');
    expect(bare).not.toContain('Result</p>');
  });

  test('the composer line says a workflow agent takes no input', () => {
    const html = view('wf:t1:0');
    expect(html).toContain('A workflow agent takes no input. Write in the main chat.');
    expect(html).not.toContain('<textarea');
    expect(html).not.toContain('A subagent takes no input');
  });

  test('an unknown wf: choice shows the main chat', () => {
    for (const agentParam of ['wf:t1:99', 'wf:zz:0', 'wf:t1', 'wf:t1:x', 'node-1']) {
      const html = view(agentParam);
      expect(html).not.toContain('fd-agent-head');
      expect(html).not.toContain('A workflow agent takes no input');
      expect(html).toContain('<textarea');
    }
  });

  test('a workflow agent shows only on the chat tab', () => {
    const html = view('wf:t1:0', 'changes');
    expect(html).not.toContain('fd-agent-head');
    expect(html).not.toContain('A workflow agent takes no input');
  });
});
