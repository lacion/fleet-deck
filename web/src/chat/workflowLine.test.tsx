// B12 (T5b): the card line of a Workflow call, the approval line of a workflow agent and "Show its
// work" in the agent view (design `workflow-agents.md` §4.6).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, Event, SubagentLog, WorkflowAgent, WorkflowUpdate } from '../gen';
import type { Timeline } from '../runtime/state';
import { type AgentChoiceState, useAgentChoice } from '../session/agentChoice';
import { sessionActions } from '../session/commands';
import { SessionView } from '../session/SessionView';
import { station } from '../testing/fleet';
import { summary } from '../testing/summary';
import { claudeCapabilities, envelopes } from '../testing/timeline';
import { workflowStates } from '../session/workflows';
import { WorkflowAgentView } from './WorkflowAgentView';
import { workflowLineText } from './WorkflowLine';

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
  node_id: 'node-1',
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

const workflowTool: Event = {
  kind: 'tool.started',
  node_id: 'node-1',
  parent_node_id: null,
  tool_kind: 'other',
  title: 'Workflow',
  input_summary: 'review',
  files: [],
  provider_refs: {},
};
const other: Event = { ...workflowTool, node_id: 'node-2', title: 'Read' } as Event;

const EVENTS = (more: Partial<WorkflowUpdate> = {}): Event[] => [
  { kind: 'run.started', run_id: 'run-1', input_summary: 'review', origin: 'board' },
  workflowTool,
  other,
  update({
    name: 'Review',
    agents: [
      agent(0, { state: 'done', agent_id: 'abc123' }),
      agent(1, { state: 'done' }),
      agent(2, { state: 'running' }),
      agent(3, { state: 'queued' }),
    ],
    running_agents: 1,
    agents_total: 4,
    ...more,
  }),
];

const view = (
  events: Event[],
  agentParam?: string,
  actions = sessionActions(async () => ({}), key, ['read']),
) =>
  renderToStaticMarkup(
    <SessionView
      session={session()}
      stations={[station()]}
      freshness="live"
      timeline={timeline(envelopes('wf', events))}
      actions={actions}
      tab="chat"
      {...(agentParam === undefined ? {} : { agent: agentParam })}
    />,
  );

const stateOf = (events: Event[]) => workflowStates(null, envelopes('wf', events)).get('t1');

describe('the workflow card line (B12)', () => {
  test('the card line counts done and running agents', () => {
    const w = stateOf(EVENTS());
    expect(w).toBeDefined();
    if (w === undefined) return;
    expect(workflowLineText(w)).toBe('Review · 2 of 4 agents done · 1 running');
    // Only the card of the Workflow call carries the line.
    const html = view(EVENTS());
    expect(html).toContain(
      '<p class="fd-note" data-workflow-line="t1">Review · 2 of 4 agents done · 1 running</p>',
    );
    expect(html.match(/data-workflow-line/g)).toHaveLength(1);
    // No agents yet, and nothing running.
    const empty = stateOf(EVENTS({ agents: [], running_agents: 0, agents_total: 0 }));
    if (empty !== undefined) expect(workflowLineText(empty)).toBe('Review · No agents yet');
  });

  test('a stopped workflow says Stopped', () => {
    const stopped = stateOf([
      ...EVENTS(),
      update({ status: 'stopped', agents: [], running_agents: 0, agents_total: 4 }),
    ]);
    if (stopped === undefined) throw new Error('no workflow');
    expect(workflowLineText(stopped)).toBe('Review · 2 of 4 agents done · Stopped');
    const failed = stateOf([
      ...EVENTS(),
      update({ status: 'failed', running_agents: 0, agents_total: 4 }),
    ]);
    if (failed === undefined) throw new Error('no workflow');
    expect(workflowLineText(failed)).toBe('Review · 2 of 4 agents done · Failed');
    const finished = stateOf([
      ...EVENTS(),
      update({ status: 'completed', running_agents: 0, agents_total: 4 }),
    ]);
    if (finished === undefined) throw new Error('no workflow');
    expect(workflowLineText(finished)).toBe('Review · 2 of 4 agents done');
  });
});

describe('the approval line of a workflow agent (B12)', () => {
  const choiceOf = (events: Event[]) => {
    const box: { state: AgentChoiceState | undefined } = { state: undefined };
    const Probe = () => {
      box.state = useAgentChoice({
        session: session(),
        subagents: [],
        events: envelopes('wf', events),
        actions: undefined,
        member: false,
        tab: 'chat',
        agent: undefined,
        onAgent: () => {},
      });
      return null;
    };
    renderToStaticMarkup(<Probe />);
    return box.state;
  };
  const ask = (request_id: string, agent_id?: string): Event => ({
    kind: 'approval.requested',
    request_id,
    node_id: null,
    request_kind: 'command',
    title: 'Bash',
    details: 'ls',
    input: null,
    options: [{ option_id: 'allow', label: 'Allow', kind: 'allow_once' }],
    response: 'rpc_reply',
    provider_refs: agent_id === undefined ? {} : { agent_id },
  });

  test('an approval of a workflow agent says From workflow agent <label>', () => {
    const state = choiceOf([
      ...EVENTS(),
      ask('req-1', 'abc123'),
      ask('req-2', 'unknown'),
      ask('req-3'),
    ]);
    expect(state?.requestFrom.get('req-1')).toBe('Agent 0');
    // An agent id that no workflow names: the label is not known.
    expect(state?.requestFrom.get('req-2')).toBeNull();
    // A request that names no workflow agent has no line.
    expect(state?.requestFrom.has('req-3')).toBe(false);
  });
});

describe('Show its work (B12)', () => {
  const log: SubagentLog = { items: [], state: { kind: 'empty' } } as unknown as SubagentLog;
  const agentOf = (agent_id: string | null) => {
    const w = stateOf(EVENTS());
    const a = w?.agents.get(0);
    if (w === undefined || a === undefined) throw new Error('no agent');
    return { workflow: w, agent: { ...a, agent_id } };
  };
  const shown = (
    agent_id: string | null,
    loadWork: ((id: string) => Promise<SubagentLog>) | null,
  ) =>
    renderToStaticMarkup(
      <WorkflowAgentView {...agentOf(agent_id)} loadWork={loadWork} onBack={() => {}} />,
    );

  test('Show its work calls GET …/workflow-agents/<id>', async () => {
    const paths: string[] = [];
    const actions = sessionActions(
      async () => ({}),
      key,
      ['read'],
      undefined,
      async (path) => {
        paths.push(path);
        return log;
      },
    );
    expect(actions.workflowAgent).toBeTruthy();
    await actions.workflowAgent?.('abc123');
    expect(paths).toEqual(['/api/sessions/station-a/wf/workflow-agents/abc123']);
    // The owner's view carries the button.
    const html = view(EVENTS(), 'wf:t1:0', actions);
    expect(html).toContain('Show its work');
    expect(shown('abc123', async () => log)).toContain('Show its work');
  });

  test('no agent id yet says Its work shows after it starts.', () => {
    const html = shown(null, async () => log);
    expect(html).toContain('Its work shows after it starts.');
    expect(html).not.toContain('Show its work');
  });

  test('a member sees no Show its work', () => {
    const none = shown('abc123', null);
    expect(none).not.toContain('Show its work');
    expect(none).not.toContain('Its work shows after it starts.');
    // The choice of a member has no reader.
    const box: { state: AgentChoiceState | undefined } = { state: undefined };
    const actions = sessionActions(
      async () => ({}),
      key,
      ['read'],
      undefined,
      async () => log,
    );
    const Probe = () => {
      box.state = useAgentChoice({
        session: session(),
        subagents: [],
        events: envelopes('wf', EVENTS()),
        actions,
        member: true,
        tab: 'chat',
        agent: 'wf:t1:0',
        onAgent: () => {},
      });
      return null;
    };
    renderToStaticMarkup(<Probe />);
    expect(box.state?.wf).not.toBeNull();
    expect(box.state?.wf?.loadWork).toBeNull();
  });
});
