import { describe, expect, test } from 'bun:test';
import { summary } from '../testing/summary';
import type { Event, EventEnvelope } from '../gen';
import { envelopes, nestedSubagents, recorded } from '../testing/timeline';
import {
  buildTimeline,
  type Item,
  runningSubagents,
  type Subagent,
  subagentLabel,
} from './timeline';

const subs = (items: readonly Item[]): Subagent[] =>
  items.filter((i): i is Subagent => i.type === 'subagent');

// Events after `base`, numbered on from its last `session_seq`.
const extend = (base: readonly EventEnvelope[], more: readonly Event[]): EventEnvelope[] => {
  const last = base.at(-1)?.session_seq ?? 0;
  const tail = envelopes(base[0]?.session_id ?? 's', more).map((e) => ({
    ...e,
    session_seq: e.session_seq + last,
    station_seq: e.station_seq + last,
  }));
  return [...base, ...tail];
};
const turnFailed: Event = {
  kind: 'turn.completed',
  turn_id: 'turn-1',
  status: 'failed',
  usage: null,
};
const runFailed: Event = {
  kind: 'run.completed',
  run_id: 'run-1',
  status: 'failed',
  background_pending: false,
};
const lost: Event = {
  kind: 'session.state',
  state: 'lost',
  detail: 'station restart: the provider process is gone',
};
const noTasks: Event = { kind: 'background.updated', tasks: [] };
const agentTask: Event = {
  kind: 'background.updated',
  tasks: [{ task_id: 'task-1', task_kind: 'local_agent' }],
};
// A run whose subagent moves to the background (task-1) and the run ends.
const backgroundRun = (): EventEnvelope[] =>
  envelopes('bg', [
    { kind: 'run.started', run_id: 'run-1', input_summary: 'go', origin: 'board' },
    { kind: 'turn.started', run_id: 'run-1', turn_id: 'turn-1', provider_refs: {} },
    {
      kind: 'tool.started',
      node_id: 'agent-1',
      parent_node_id: null,
      tool_kind: 'subagent',
      title: 'Agent',
      input_summary: 'work in the background',
      files: [],
      provider_refs: {},
    },
    agentTask,
    { kind: 'tool.completed', node_id: 'agent-1', status: 'ok', files: [], exit_code: null },
    { kind: 'turn.completed', turn_id: 'turn-1', status: 'done', usage: null },
    { kind: 'run.completed', run_id: 'run-1', status: 'done', background_pending: true },
  ]);

describe('session loss and exit end the tree (M2 gate correction)', () => {
  test('the restart recovery ends nested subagents, tools, messages and requests', () => {
    // Three subagents run, a grandchild tool waits for an open approval,
    // and node-1 has an open message; then the station recovery events.
    const tree = buildTimeline(extend(nestedSubagents(10), [turnFailed, runFailed, lost]));
    expect(tree.running).toBe(0);
    expect(tree.subagents.map((s) => s.status)).toEqual(['ended', 'ended', 'ended']);
    const [explore] = subs(tree.items);
    // node-3 finished before the loss and keeps `ok`; node-5 was running.
    expect(explore?.items[0]).toMatchObject({ type: 'tool', status: 'ok' });
    expect(explore?.items[1]).toMatchObject({ type: 'message', text: 'main is small', done: true });
    const docs = tree.subagents[2];
    expect(docs?.items[0]).toMatchObject({ type: 'tool', status: 'ended' });
    expect(docs?.items[1]).toMatchObject({
      type: 'approval',
      state: 'expired',
      outcome: 'session lost',
    });
    const running = (items: readonly Item[]): number =>
      items.reduce(
        (n, i) =>
          n +
          (i.type === 'tool' && i.status === 'running' ? 1 : 0) +
          (i.type === 'subagent' ? (i.status === 'running' ? 1 : 0) + running(i.items) : 0),
        0,
      );
    expect(running(tree.items)).toBe(0);
    expect(tree.items.at(-1)).toMatchObject({ type: 'turn', status: 'failed' });
  });

  test('the new recovery order ends a background subagent before the list empties', () => {
    const events = extend(backgroundRun(), [lost, noTasks]);
    expect(buildTimeline(events.slice(0, -1)).subagents[0]?.status).toBe('ended');
    const tree = buildTimeline(events);
    expect(tree.running).toBe(0);
    expect(tree.subagents[0]?.status).toBe('ended');
  });

  test('the old recovery order (empty list, then loss) also ends the background subagent', () => {
    const tree = buildTimeline(extend(backgroundRun(), [noTasks, lost]));
    expect(tree.running).toBe(0);
    expect(tree.subagents[0]?.status).toBe('ended');
  });

  test('a background subagent that finished earlier stays done after a loss', () => {
    const user: Event = { kind: 'mode.changed', mode: 'plan' };
    const tree = buildTimeline(extend(backgroundRun(), [noTasks, user, lost]));
    expect(tree.subagents[0]?.status).toBe('done');
    expect(tree.running).toBe(0);
  });

  test('a background subagent runs on across run completion and a later run', () => {
    const events = extend(backgroundRun(), [
      { kind: 'run.started', run_id: 'run-2', input_summary: 'next', origin: 'board' },
      { kind: 'turn.started', run_id: 'run-2', turn_id: 'turn-2', provider_refs: {} },
      { kind: 'turn.completed', turn_id: 'turn-2', status: 'done', usage: null },
      { kind: 'run.completed', run_id: 'run-2', status: 'done', background_pending: true },
    ]);
    const between = buildTimeline(events);
    expect(between.subagents[0]).toMatchObject({ status: 'background', task_id: 'task-1' });
    expect(between.running).toBe(1);
    const finished = buildTimeline(extend(events, [noTasks]));
    expect(finished.subagents[0]?.status).toBe('done');
    expect(finished.running).toBe(0);
  });

  test('a session exit ends running work like session.ended, and later events revive nothing', () => {
    const exited: Event = { kind: 'session.state', state: 'exited', detail: null };
    const tree = buildTimeline(
      extend(nestedSubagents(10), [
        exited,
        agentTask,
        { kind: 'session.ended', reason: 'process_exit', exit_code: 0 },
      ]),
    );
    expect(tree.running).toBe(0);
    expect(tree.subagents.map((s) => s.status)).toEqual(['ended', 'ended', 'ended']);
    expect(tree.subagents[2]?.items[1]).toMatchObject({
      state: 'expired',
      outcome: 'session ended',
    });
    expect(tree.items.at(-1)).toMatchObject({ type: 'notice', text: 'The session ended.' });
  });

  test('the label of an ended subagent is neutral', () => {
    expect(subagentLabel('ended')).toBe('stopped');
  });
});

describe('session timeline tree (PLAN D15)', () => {
  test('the recorded background subagent runs on after its tool call and then finishes', () => {
    const events = recorded('subagent');
    const at = (seq: number) => buildTimeline(events.filter((e) => e.session_seq <= seq));
    const started = at(6);
    expect(started.running).toBe(1);
    expect(started.subagents.map((s) => [s.title, s.input, s.status])).toEqual([
      ['Agent', 'Test subagent', 'running'],
    ]);
    // seq 7 announces a local_agent task; seq 8 completes the tool call.
    const background = at(8).subagents[0];
    expect(background?.status).toBe('background');
    expect(background?.task_id).toBe('aec7cbebd3902b8ce');
    expect(at(19).running).toBe(1);
    // seq 20: the task leaves the list, so the child is done.
    const after = at(20);
    expect(after.running).toBe(0);
    expect(after.subagents[0]?.status).toBe('done');
    const full = buildTimeline(events);
    const texts = full.items.filter((i) => i.type === 'message').map((i) => i.text);
    expect(texts).toEqual([
      "The subagent is running in the background. I'll wait for it to complete.",
      'sub-ok',
    ]);
    expect(full.items.map((i) => i.type)).toEqual([
      'run',
      'subagent',
      'message',
      'run',
      'message',
      'notice',
    ]);
  });

  test('nested foreground subagents get their own timeline, status and approvals', () => {
    const mid = buildTimeline(nestedSubagents(10));
    expect(mid.running).toBe(3);
    const [explore, check] = subs(mid.items);
    expect(explore?.input).toBe('Explore the code');
    expect(check?.status).toBe('running');
    expect(explore?.items.map((i) => i.type)).toEqual(['tool', 'message', 'subagent']);
    const docs = subs(explore?.items ?? [])[0];
    expect(docs?.input).toBe('Read the docs');
    // The approval of a grandchild tool sits in the grandchild timeline.
    expect(docs?.items.map((i) => i.type)).toEqual(['tool', 'approval']);
    const approval = docs?.items[1];
    expect(approval?.type === 'approval' && approval.state).toBe('open');
    const text = explore?.items[1];
    expect(text?.type === 'message' && text.text).toBe('main is small');
    // No child event leaks into the parent timeline.
    expect(mid.items.map((i) => i.type)).toEqual(['run', 'subagent', 'subagent']);

    const later = buildTimeline(nestedSubagents(17));
    expect(later.running).toBe(1);
    expect(later.subagents.map((s) => [s.input, s.status])).toEqual([
      ['Explore the code', 'running'],
      ['Check the tests', 'failed'],
      ['Read the docs', 'done'],
    ]);
    const answered = later.subagents[2]?.items[1];
    expect(answered?.type === 'approval' && answered.outcome).toBe('Allow (by board)');
    const shell = later.subagents[2]?.items[0];
    expect(shell?.type === 'tool' && [shell.status, shell.output]).toEqual(['ok', 'PLAN.md\n']);

    const full = buildTimeline(nestedSubagents());
    expect(full.running).toBe(0);
    expect(full.items.at(-1)).toMatchObject({ type: 'message', text: 'All subagents reported.' });
  });

  test('a session end stops running children and expires open approvals', () => {
    const events = nestedSubagents(10);
    const end = {
      ...events[0],
      session_seq: 99,
      event: { kind: 'session.ended', reason: 'process_exit', exit_code: 1 },
    } as (typeof events)[number];
    const tree = buildTimeline([...events, end]);
    expect(tree.running).toBe(0);
    expect(tree.subagents.map((s) => s.status)).toEqual(['ended', 'ended', 'ended']);
    const approval = tree.subagents[2]?.items[1];
    expect(approval?.type === 'approval' && approval.state).toBe('expired');
  });

  test('the recorded approval shows the answer and the tool result', () => {
    const tree = buildTimeline(recorded('tool_call_approve'));
    expect(tree.subagents).toEqual([]);
    const approval = tree.items.find((i) => i.type === 'approval');
    expect(approval).toMatchObject({ state: 'answered', outcome: 'Allow once (by board)' });
    expect(tree.items.find((i) => i.type === 'tool')).toMatchObject({
      title: 'Bash',
      status: 'ok',
    });
  });

  test('a recorded Codex approval and subagent build the same tree shapes (P5-3)', () => {
    const approve = buildTimeline(recorded('codex-tool_call_approve'));
    expect(approve.items.find((i) => i.type === 'approval')).toMatchObject({
      state: 'answered',
      outcome: 'Allow once (by board)',
    });
    expect(approve.items.find((i) => i.type === 'tool')).toMatchObject({ status: 'ok' });
    // Codex reports the child's message under the spawnAgent node (D15).
    const sub = buildTimeline(recorded('codex-subagent'));
    expect(sub.running).toBe(0);
    // spawnAgent holds the child's message; the wait tool has no events.
    expect(sub.subagents.map((s) => [s.status, s.items.length])).toEqual([
      ['done', 1],
      ['done', 0],
    ]);
  });

  test('a child completion leaves its parent subagent active', () => {
    // Index 15 completes the grandchild node-4; node-1 runs on.
    const tree = buildTimeline(nestedSubagents(15));
    expect(tree.subagents.map((s) => [s.input, s.status])).toEqual([
      ['Explore the code', 'running'],
      ['Check the tests', 'running'],
      ['Read the docs', 'done'],
    ]);
    expect(tree.running).toBe(2);
  });

  test('unknown event kinds are skipped', () => {
    const events = nestedSubagents(2);
    const odd = { ...events[0], session_seq: 50, event: { kind: 'future.thing' } };
    const tree = buildTimeline([...events, odd as (typeof events)[number]]);
    expect(tree.items).toHaveLength(2);
  });

  test('a card counts foreground subagent tools and background agents', () => {
    const s = summary('a', 'working', 1, {
      running_tools: [
        { node_id: 'n1', tool_kind: 'subagent', title: 'Agent', input_summary: '' },
        { node_id: 'n2', tool_kind: 'shell', title: 'Bash', input_summary: '' },
      ],
      background_agents: 2,
    });
    expect(runningSubagents(s)).toBe(3);
  });
});
