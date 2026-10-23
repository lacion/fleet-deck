// B12: the workflow state of the board (design `workflow-agents.md` §4.3a and §4.6).
import { describe, expect, test } from 'bun:test';
import type { Event, WorkflowAgent, WorkflowUpdate } from '../gen';
import { envelopes } from '../testing/timeline';
import {
  agentById,
  agentStatus,
  parseWorkflowAgentParam,
  requestAgents,
  type WorkflowsSnapshot,
  workflowAgentParam,
  workflowStates,
} from './workflows';

const state = (more: Partial<WorkflowUpdate> = {}): WorkflowUpdate => ({
  task_id: 't1',
  status: 'running',
  phases: [],
  agents: [],
  running_agents: 0,
  agents_total: 0,
  ...more,
});
const update = (more: Partial<WorkflowUpdate> = {}): Event => ({
  kind: 'workflow.updated',
  ...state(more),
});
const agent = (index: number, more: Partial<WorkflowAgent> = {}): WorkflowAgent => ({
  index,
  label: `Agent ${index}`,
  state: 'running',
  ...more,
});
const taskEvent = (
  task_id: string,
  status: string,
  more: Partial<Extract<Event, { kind: 'task.updated' }>> = {},
): Event => ({ kind: 'task.updated', task_id, task_kind: 'local_workflow', status, ...more });
const list = (...ids: string[]): Event => ({
  kind: 'background.updated',
  tasks: ids.map((task_id) => ({ task_id, task_kind: 'local_workflow' })),
});
/** A `task.updated` of a task that is not a workflow. */
const other = (task_id: string): Event => ({
  kind: 'task.updated',
  task_id,
  task_kind: 'local_bash',
  status: 'running',
});
const seq = (events: readonly Event[]) => envelopes('s', events);

describe('workflowStates (B12)', () => {
  test('workflowStates merges phases and agents by index', () => {
    const w = workflowStates(
      null,
      seq([
        update({
          name: 'Review',
          node_id: 'node-2',
          phases: [{ index: 1, title: 'Alpha' }],
          agents: [agent(0, { phase_index: 1 }), agent(1, { state: 'queued' })],
          running_agents: 1,
          agents_total: 2,
        }),
        update({
          phases: [{ index: 2, title: 'Beta' }],
          agents: [agent(1, { last_tool: 'Bash' }), agent(2)],
          running_agents: 2,
          agents_total: 3,
        }),
      ]),
    ).get('t1');
    expect(w?.source).toBe('station');
    expect(w?.name).toBe('Review');
    expect(w?.node_id).toBe('node-2');
    expect([...(w?.phases ?? [])]).toEqual([
      [1, 'Alpha'],
      [2, 'Beta'],
    ]);
    expect([...(w?.agents.keys() ?? [])]).toEqual([0, 1, 2]);
    expect(w?.agents.get(1)?.state).toBe('running');
    expect(w?.agents.get(1)?.last_tool).toBe('Bash');
    expect(w?.agents.get(0)?.phase_index).toBe(1);
    expect(w?.running_agents).toBe(2);
    expect(w?.agents_total).toBe(3);
  });

  test('workflowStates applies only events after as_of to a snapshot', () => {
    const snapshot: WorkflowsSnapshot = {
      as_of: 2,
      incomplete: false,
      runs: [
        {
          run_order: 10,
          state: state({ name: 'Review', agents: [agent(0, { state: 'done' })], agents_total: 1 }),
        },
      ],
    };
    const w = workflowStates(
      snapshot,
      seq([
        // Events 1 and 2 are inside the snapshot: they must not change agent 0 again.
        update({ agents: [agent(0, { state: 'queued' })] }),
        update({ agents: [agent(0, { state: 'running' })] }),
        update({ agents: [agent(1)], agents_total: 2, running_agents: 1 }),
      ]),
    ).get('t1');
    expect(w?.agents.get(0)?.state).toBe('done');
    expect(w?.agents.get(1)?.state).toBe('running');
    expect(w?.name).toBe('Review');
    expect(w?.agents_total).toBe(2);
  });

  test('a final status stops queued and running agents (R2)', () => {
    const w = workflowStates(
      null,
      seq([
        update({
          agents: [agent(0, { state: 'queued' }), agent(1), agent(2, { state: 'done' })],
          running_agents: 1,
          agents_total: 3,
        }),
        update({ status: 'completed' }),
      ]),
    ).get('t1');
    expect(w?.status).toBe('completed');
    expect(w?.running_agents).toBe(0);
    expect([...(w?.agents.values() ?? [])].map((a) => a.state)).toEqual([
      'stopped',
      'stopped',
      'done',
    ]);
  });

  test('background.updated stops a missing run while another runs (R3)', () => {
    const views = workflowStates(
      null,
      seq([
        update({ task_id: 't1', agents: [agent(0)], running_agents: 1, agents_total: 1 }),
        update({ task_id: 't2', agents: [agent(0)], running_agents: 1, agents_total: 1 }),
        list('t2'),
      ]),
    );
    expect(views.get('t1')?.status).toBe('stopped');
    expect(views.get('t1')?.agents.get(0)?.state).toBe('stopped');
    expect(views.get('t1')?.running_agents).toBe(0);
    expect(views.get('t2')?.status).toBe('running');
    expect(views.get('t2')?.agents.get(0)?.state).toBe('running');
  });

  test('a later running event makes a stopped run running again (R5)', () => {
    const w = workflowStates(
      null,
      seq([
        update({ agents: [agent(0)], running_agents: 1, agents_total: 1 }),
        list(),
        update({ agents: [agent(0, { state: 'done' }), agent(1)], running_agents: 1 }),
      ]),
    ).get('t1');
    expect(w?.status).toBe('running');
    expect(w?.agents.get(0)?.state).toBe('done');
    expect(w?.agents.get(1)?.state).toBe('running');
  });

  test('an older station gives a task_only workflow from task.updated', () => {
    const views = workflowStates(
      null,
      seq([taskEvent('w1', 'running', { description: 'Review the repo', node_id: 'node-3' })]),
    );
    const w = views.get('w1');
    expect(w?.source).toBe('task_only');
    expect(w?.description).toBe('Review the repo');
    expect(w?.node_id).toBe('node-3');
    expect(w?.status).toBe('running');
    expect(w?.agents.size).toBe(0);
    // A task of another kind is not a workflow.
    expect(workflowStates(null, seq([other('b1')])).size).toBe(0);
  });

  test('the status of a task_only workflow follows its task', () => {
    const status = (s: string) =>
      workflowStates(null, seq([taskEvent('w1', 'running'), taskEvent('w1', s)])).get('w1')?.status;
    expect(status('completed')).toBe('completed');
    expect(status('failed')).toBe('failed');
    expect(status('killed')).toBe('failed');
    expect(status('stopped')).toBe('stopped');
    expect(status('cancelled')).toBe('stopped');
  });

  test('a task event does not change the status of a station workflow', () => {
    const w = workflowStates(
      null,
      seq([
        update({ name: 'Review' }),
        {
          kind: 'task.updated',
          task_id: 't1',
          task_kind: 'unknown',
          status: 'stopped',
          description: 'Review the repo',
        },
      ]),
    ).get('t1');
    expect(w?.status).toBe('running');
    expect(w?.source).toBe('station');
    expect(w?.description).toBe('Review the repo');
  });

  test('the end of the process stops running workflows and agents', () => {
    const run = [update({ agents: [agent(0)], running_agents: 1, agents_total: 1 })];
    const ended: Event[] = [
      { kind: 'session.ended', reason: 'exit', exit_code: 0 },
      { kind: 'session.state', state: 'lost' } as unknown as Event,
      { kind: 'session.state_changed', state: 'exited' } as unknown as Event,
    ];
    for (const end of ended) {
      const w = workflowStates(null, seq([...run, end])).get('t1');
      expect(w?.status).toBe('stopped');
      expect(w?.agents.get(0)?.state).toBe('stopped');
      expect(w?.running_agents).toBe(0);
    }
    // A state that is not an end changes nothing.
    const alive = { kind: 'session.state', state: 'running' } as unknown as Event;
    expect(workflowStates(null, seq([...run, alive])).get('t1')?.status).toBe('running');
  });

  test('a run keeps at most 100 phases and 1,000 agents (R1)', () => {
    const w = workflowStates(
      null,
      seq([
        update({
          phases: Array.from({ length: 101 }, (_, index) => ({ index, title: `p${index}` })),
          agents: Array.from({ length: 1001 }, (_, index) => agent(index)),
        }),
        // An index that the run keeps is still replaced.
        update({ agents: [agent(5, { state: 'done' })] }),
      ]),
    ).get('t1');
    expect(w?.phases.size).toBe(100);
    expect(w?.agents.size).toBe(1000);
    expect(w?.agents.get(5)?.state).toBe('done');
  });

  test('the runs keep the order of the pages, then of the events', () => {
    const snapshot: WorkflowsSnapshot = {
      as_of: 0,
      incomplete: false,
      runs: [
        { run_order: 3, state: state({ task_id: 'a' }) },
        { run_order: 9, state: state({ task_id: 'b' }) },
      ],
    };
    const views = workflowStates(snapshot, seq([update({ task_id: 'c' })]));
    expect([...views.keys()]).toEqual(['a', 'b', 'c']);
  });
});

describe('the helpers of the workflow state (B12)', () => {
  test('requestAgents maps a request to its agent', () => {
    const request = (request_id: string, provider_refs: Record<string, string>): Event => ({
      kind: 'approval.requested',
      request_id,
      node_id: null,
      request_kind: 'command',
      title: 'Bash',
      details: 'ls',
      input: null,
      options: [],
      response: 'rpc_reply',
      provider_refs,
    });
    const map = requestAgents(seq([request('r1', { agent_id: 'abc' }), request('r2', {})]));
    expect([...map]).toEqual([['r1', 'abc']]);
  });

  test('agentById finds the agent in any workflow', () => {
    const views = workflowStates(
      null,
      seq([
        update({ task_id: 't1', agents: [agent(0, { agent_id: 'aa' })] }),
        update({ task_id: 't2', agents: [agent(0, { agent_id: 'bb', label: 'Second' })] }),
      ]),
    );
    expect(agentById(views, 'bb')?.label).toBe('Second');
    expect(agentById(views, 'zz')).toBeNull();
  });

  test('queued is info, failed is hazard', () => {
    expect(agentStatus('running')).toEqual({ label: 'running', tone: 'ok' });
    expect(agentStatus('queued')).toEqual({ label: 'queued', tone: 'info' });
    expect(agentStatus('failed')).toEqual({ label: 'failed', tone: 'hazard' });
    expect(agentStatus('done')).toEqual({ label: 'done', tone: 'muted' });
    expect(agentStatus('stopped')).toEqual({ label: 'stopped', tone: 'muted' });
    expect(agentStatus('retrying')).toEqual({ label: 'retrying', tone: 'muted' });
  });

  test('the agent value of the URL round-trips and only wf: values parse', () => {
    expect(workflowAgentParam('t1', 3)).toBe('wf:t1:3');
    expect(parseWorkflowAgentParam('wf:t1:3')).toEqual({ task_id: 't1', index: 3 });
    // A task id may hold a colon: the index is after the last one.
    expect(parseWorkflowAgentParam('wf:a:b:12')).toEqual({ task_id: 'a:b', index: 12 });
    for (const bad of [undefined, 'node-1', 'wf:', 'wf:t1', 'wf:t1:x', 'wf::3', 'wf:t1:-1']) {
      expect(parseWorkflowAgentParam(bad)).toBeNull();
    }
  });
});
