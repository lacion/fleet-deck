import { describe, expect, test } from 'bun:test';
import type { Event, EventEnvelope } from '../gen';
import { envelopes, recorded } from '../testing/timeline';
import {
  commandTasks,
  hasAgents,
  runningCount,
  shownStatus,
  type TaskState,
  taskStates,
  taskStatus,
  toolTitles,
} from './tasks';
import { buildTimeline, type Subagent } from './timeline';

const update = (
  task_id: string,
  status: string,
  more: Partial<Extract<Event, { kind: 'task.updated' }>> = {},
): Event => ({
  kind: 'task.updated',
  task_id,
  task_kind: 'local_agent',
  status,
  ...more,
});

const agent = (node_id: string, status: Subagent['status'] = 'done'): Subagent => ({
  type: 'subagent',
  key: node_id,
  node_id,
  title: 'Agent',
  input: 'work',
  status,
  task_id: null,
  tool_use_id: null,
  items: [],
});

const state = (extra: Partial<TaskState>): TaskState => ({
  task_id: 't',
  node_id: 'n',
  kind: 'local_agent',
  status: 'running',
  description: null,
  summary: null,
  ...extra,
});

const tasksOf = (events: readonly EventEnvelope[]) => taskStates(events);

describe('taskStates', () => {
  test('a lost or ended session stops its running tasks and keeps finished ones', () => {
    const lost = taskStates(
      envelopes('s', [
        update('a', 'running'),
        update('b', 'completed', { summary: 'done' }),
        { kind: 'session.state', state: 'lost' } as unknown as Event,
      ]),
    );
    expect(lost.get('a')?.status).toBe('stopped');
    expect(lost.get('b')?.status).toBe('completed');
    const ended = taskStates(
      envelopes('s', [update('a', 'running'), { kind: 'session.ended' } as unknown as Event]),
    );
    expect(ended.get('a')?.status).toBe('stopped');
  });

  test('the last event wins for the status; the summary and the description are kept', () => {
    const tasks = taskStates(
      envelopes('s', [
        update('a', 'running', { node_id: 'n1', description: 'Test subagent' }),
        update('a', 'completed', { summary: 'sub-ok' }),
        update('a', 'completed'),
        update('b', 'running', { task_kind: 'local_bash' }),
      ]),
    );
    expect(tasks.get('a')).toEqual({
      task_id: 'a',
      node_id: 'n1',
      kind: 'local_agent',
      status: 'completed',
      description: 'Test subagent',
      summary: 'sub-ok',
    });
    expect(tasks.get('b')?.node_id).toBeNull();
    expect([...tasks.keys()]).toEqual(['a', 'b']);
  });

  test('the recorded Claude fixture ends completed with the summary', () => {
    const tasks = tasksOf(recorded('subagent'));
    expect([...tasks.values()]).toEqual([
      {
        task_id: 'aec7cbebd3902b8ce',
        node_id: 'node-2',
        kind: 'local_agent',
        status: 'completed',
        description: 'Test subagent',
        summary: 'sub-ok',
      },
    ]);
  });

  test('the recorded Codex fixture ends completed with the summary', () => {
    const [task] = tasksOf(recorded('codex-subagent')).values();
    expect(task?.kind).toBe('codex_agent');
    expect(task?.status).toBe('completed');
    expect(task?.summary).toBe('sub-ok');
  });
});

describe('the shown status', () => {
  test('a task maps its status to the chip', () => {
    const cases: [string, string, string][] = [
      ['running', 'running', 'ok'],
      ['completed', 'done', 'muted'],
      ['failed', 'failed', 'hazard'],
      ['killed', 'failed', 'hazard'],
      ['stopped', 'stopped', 'muted'],
      ['cancelled', 'stopped', 'muted'],
      ['paused', 'paused', 'muted'],
    ];
    for (const [status, label, tone] of cases) {
      expect(taskStatus(status)).toEqual({ label, tone: tone as 'ok' });
    }
  });

  test('a running task of a background subagent keeps the background word and counts as running', () => {
    const tasks = new Map([['t', state({ status: 'running' })]]);
    expect(shownStatus(agent('n', 'background'), tasks)).toEqual({
      label: 'running in the background',
      tone: 'info',
    });
    expect(runningCount([agent('n', 'background')], tasks)).toBe(1);
    expect(shownStatus(agent('n', 'running'), tasks).label).toBe('running');
  });

  test('a subagent takes the status of the task of its node', () => {
    const tasks = new Map([['t', state({ status: 'completed' })]]);
    expect(shownStatus(agent('n', 'background'), tasks)).toEqual({ label: 'done', tone: 'muted' });
  });

  test('a subagent without a task keeps the card status', () => {
    expect(shownStatus(agent('n', 'background'), new Map())).toEqual({
      label: 'running in the background',
      tone: 'info',
    });
    expect(shownStatus(agent('n', 'failed'), new Map())).toEqual({
      label: 'failed',
      tone: 'hazard',
    });
  });

  test('a running task of the node wins over a later finished one', () => {
    const tasks = new Map([
      ['a', state({ task_id: 'a', status: 'running' })],
      ['b', state({ task_id: 'b', status: 'completed' })],
    ]);
    expect(shownStatus(agent('n'), tasks).label).toBe('running');
  });

  test('the running count: running subagents, background ones without a task, running commands', () => {
    const tasks = new Map([
      ['a', state({ task_id: 'a', node_id: 'n1', status: 'running' })],
      ['b', state({ task_id: 'b', node_id: 'n2', status: 'completed' })],
      ['c', state({ task_id: 'c', node_id: 'bash', kind: 'local_bash', status: 'running' })],
      ['d', state({ task_id: 'd', node_id: 'bash2', kind: 'local_bash', status: 'completed' })],
    ]);
    const subs = [agent('n1', 'done'), agent('n2', 'running'), agent('n3', 'background')];
    expect(runningCount(subs, tasks)).toBe(3);
    expect(runningCount([], new Map())).toBe(0);
  });
});

describe('the agents of a session', () => {
  test('hasAgents needs a subagent or a background command', () => {
    expect(hasAgents([], new Map())).toBe(false);
    expect(hasAgents([agent('n')], new Map())).toBe(true);
    const bash = new Map([['c', state({ kind: 'local_bash' })]]);
    expect(hasAgents([], bash)).toBe(true);
    expect(commandTasks(bash)).toHaveLength(1);
    expect(hasAgents([], new Map([['a', state({})]]))).toBe(false);
  });

  test('toolTitles reads the input of every tool card', () => {
    const tree = buildTimeline(recorded('subagent'));
    expect(toolTitles(tree.items).size).toBe(0);
    const nested = buildTimeline(
      envelopes('s', [
        {
          kind: 'tool.started',
          node_id: 'b1',
          parent_node_id: null,
          tool_kind: 'shell',
          title: 'Bash',
          input_summary: 'sleep 60',
          files: [],
          provider_refs: {},
        },
      ]),
    );
    expect(toolTitles(nested.items).get('b1')).toBe('sleep 60');
  });
});
