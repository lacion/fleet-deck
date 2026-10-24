import { describe, expect, test } from 'bun:test';
import { claudeCaps } from '../testing/capabilities';
import { summary } from '../testing/summary';
import type { Event, EventEnvelope } from '../gen';
import { envelopes, nestedSubagents, recorded } from '../testing/timeline';
import {
  buildTimeline,
  type Item,
  runningSubagents,
  sessionModels,
  slashCommands,
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
    const started = at(10);
    expect(started.running).toBe(1);
    expect(started.subagents.map((s) => [s.title, s.input, s.status])).toEqual([
      ['Agent', 'Test subagent', 'running'],
    ]);
    // seq 11 announces a local_agent task, seq 12 is its task.updated; seq 13 completes the tool call.
    const background = at(13).subagents[0];
    expect(background?.status).toBe('background');
    expect(background?.task_id).toBe('aec7cbebd3902b8ce');
    expect(at(24).running).toBe(1);
    // seq 26: the task leaves the list, so the child is done.
    const after = at(26);
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
      'notice',
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
    // The board is the owner: the outcome names no one.
    expect(answered?.type === 'approval' && answered.outcome).toBe('Allow');
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
    expect(approval).toMatchObject({ state: 'answered', outcome: 'Allow once' });
    expect(tree.items.find((i) => i.type === 'tool')).toMatchObject({
      title: 'Bash',
      status: 'ok',
    });
  });

  test('a recorded Codex approval and subagent build the same tree shapes (P5-3)', () => {
    const approve = buildTimeline(recorded('codex-tool_call_approve'));
    expect(approve.items.find((i) => i.type === 'approval')).toMatchObject({
      state: 'answered',
      outcome: 'Allow once',
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

const toolStarted = (node_id: string, files: string[] = []): Event => ({
  kind: 'tool.started',
  node_id,
  parent_node_id: null,
  tool_kind: 'edit',
  title: 'Edit',
  input_summary: 'x',
  files,
  provider_refs: {},
});

describe('chat timeline items (DL-140)', () => {
  test('reasoning deltas of one turn merge and end when another item follows', () => {
    const reasoning = (turn_id: string, text: string): Event => ({
      kind: 'reasoning.delta',
      turn_id,
      text,
    });
    const open = buildTimeline(envelopes('r', [reasoning('t1', 'a'), reasoning('t1', 'b')]));
    expect(open.items).toEqual([
      {
        type: 'reasoning',
        key: '1',
        turn_id: 't1',
        text: 'ab',
        done: false,
        started_at: 1_760_000_001_000,
        ended_at: 1_760_000_002_000,
      },
    ]);
    const closed = buildTimeline(
      envelopes('r', [reasoning('t1', 'a'), toolStarted('n1'), reasoning('t1', 'b')]),
    );
    expect(closed.items.map((i) => i.type)).toEqual(['reasoning', 'tool', 'reasoning']);
    expect(closed.items[0]).toMatchObject({ text: 'a', done: true });
    expect(closed.items[2]).toMatchObject({ text: 'b', done: false });
    const ended = buildTimeline(
      envelopes('r', [reasoning('t1', 'a'), { kind: 'session.ended', reason: 'exit' } as Event]),
    );
    expect(ended.items[0]).toMatchObject({ type: 'reasoning', done: true });
  });

  test('a plan proposal is a plan item', () => {
    const tree = buildTimeline(
      envelopes('p', [
        { kind: 'plan.proposed', plan_id: 'plan-1', turn_id: null, markdown: '# Go' },
      ]),
    );
    expect(tree.items).toEqual([{ type: 'plan', key: '1', plan_id: 'plan-1', markdown: '# Go' }]);
  });

  test('tool files merge from started, completed and file.changed; exit code is kept', () => {
    const tree = buildTimeline(
      envelopes('f', [
        toolStarted('n1', ['a.ts', 'b.ts']),
        { kind: 'file.changed', path: 'c.ts', change: 'added', node_id: 'n1' },
        { kind: 'file.changed', path: 'a.ts', change: 'modified', node_id: 'n1' },
        {
          kind: 'tool.completed',
          node_id: 'n1',
          status: 'failed',
          files: ['b.ts', 'd.ts'],
          exit_code: 2,
        },
      ]),
    );
    expect(tree.items).toHaveLength(1);
    expect(tree.items[0]).toMatchObject({
      type: 'tool',
      files: ['a.ts', 'b.ts', 'c.ts', 'd.ts'],
      exit_code: 2,
      status: 'failed',
    });
  });

  test('turn.finalized sets the diff on an existing turn item', () => {
    const diff = { files_changed: 2, insertions: 5, deletions: 1 };
    const tree = buildTimeline(
      envelopes('d', [
        { kind: 'turn.completed', turn_id: 't1', status: 'failed', usage: null },
        { kind: 'turn.finalized', turn_id: 't1', checkpoint: null, diff_summary: diff },
      ]),
    );
    expect(tree.items).toEqual([{ type: 'turn', key: '1', status: 'failed', turn_id: 't1', diff }]);
  });

  test('turn.finalized for a missing turn adds a done turn item', () => {
    const diff = { files_changed: 1, insertions: 1, deletions: 0 };
    const tree = buildTimeline(
      envelopes('d', [
        { kind: 'turn.completed', turn_id: 't1', status: 'done', usage: null },
        { kind: 'turn.finalized', turn_id: 't1', checkpoint: null, diff_summary: diff },
        { kind: 'turn.finalized', turn_id: 't2', checkpoint: null, diff_summary: null },
      ]),
    );
    expect(tree.items).toMatchObject([
      { type: 'turn', status: 'done', turn_id: 't1', diff },
      { type: 'turn', status: 'done', turn_id: 't2', diff: null },
    ]);
  });

  test('file.changed without a known node is a notice', () => {
    const tree = buildTimeline(
      envelopes('n', [
        { kind: 'file.changed', path: 'x.ts', change: 'deleted', node_id: null },
        { kind: 'file.changed', path: 'y.ts', change: 'added', node_id: 'ghost' },
      ]),
    );
    expect(tree.items).toMatchObject([
      { type: 'notice', text: 'x.ts deleted', tone: 'info' },
      { type: 'notice', text: 'y.ts added', tone: 'info' },
    ]);
  });

  test('model.changed, output_gap and session.terminal are notices', () => {
    const tree = buildTimeline(
      envelopes('n', [
        { kind: 'model.changed', model: 'opus' },
        { kind: 'session.output_gap', from: 4, to: 9, reason: 'below_floor' },
        { kind: 'session.terminal', shell_id: 'sh-1', pid: 7 },
        { kind: 'session.terminal', reason: 'tui exited' },
        { kind: 'session.terminal' },
      ]),
    );
    expect(tree.items.map((i) => (i.type === 'notice' ? i.text : i.type))).toEqual([
      'Model changed to Opus',
      'Some output is missing here (events 4–9).',
      'Opened in a terminal.',
      'Returned to board control: the terminal closed.',
      'Returned to board control.',
    ]);
  });

  test('the notices use words, and keep the raw text of the station in their title', () => {
    const lost =
      'the station cannot tell whether input 01M5KXDYADTF7EEG7DYNPJSG1X reached the provider (process_lost); it does not send it again';
    const tree = buildTimeline(
      envelopes('w', [
        { kind: 'model.changed', model: 'claude-opus-5-5[1m]' },
        { kind: 'mode.changed', mode: 'bypassPermissions' },
        { kind: 'error', code: 'delivery_unknown', message: lost, retry_at: null },
        { kind: 'error', code: 'delivery_unknown', message: lost, retry_at: null },
      ]),
    );
    expect(tree.items).toEqual([
      expect.objectContaining({
        text: 'Model changed to Opus 5.5 (1M context)',
        title: 'claude-opus-5-5[1m]',
      }),
      expect.objectContaining({ text: 'Mode: Bypass permissions', title: 'bypassPermissions' }),
      // Two lost inputs in a row are one notice with a count.
      expect.objectContaining({
        text: 'A message may not have reached the agent: the agent process stopped. The station does not send it again.',
        title: `delivery_unknown: ${lost}`,
        tone: 'error',
        repeat: 2,
      }),
    ]);
  });

  test('a request outcome says who answered and why it expired in words', () => {
    const request = (id: string): Parameters<typeof envelopes>[1][number] => ({
      kind: 'approval.requested',
      request_id: id,
      node_id: null,
      request_kind: 'question',
      title: 'AskUserQuestion',
      details: '',
      input: null,
      options: [{ option_id: 'allow', label: 'Allow', kind: 'allow_once' }],
      response: 'message',
      provider_refs: {},
    });
    const tree = buildTimeline(
      envelopes('o', [
        request('q1'),
        { kind: 'approval.expired', request_id: 'q1', reason: 'process_lost' },
        request('q2'),
        { kind: 'approval.resolved', request_id: 'q2', option_id: 'allow', by: 'policy' },
      ]),
    );
    expect(tree.items.map((i) => (i.type === 'approval' ? i.outcome : i.type))).toEqual([
      'expired: the agent process stopped',
      'Allow, by a rule',
    ]);
  });

  test('session.notice is an info notice with its text', () => {
    const tree = buildTimeline(
      envelopes('n', [{ kind: 'session.notice', code: 'v1_plugin', text: 'The v1 plugin is on.' }]),
    );
    expect(tree.items).toMatchObject([
      { type: 'notice', text: 'The v1 plugin is on.', tone: 'info' },
    ]);
  });

  test('slashCommands reads the newest event, else null', () => {
    expect(slashCommands(envelopes('s', [{ kind: 'model.changed', model: 'm' }]))).toBeNull();
    const events = envelopes('s', [
      { kind: 'session.slash_commands', commands: ['old'], skills: [] },
      { kind: 'model.changed', model: 'm' },
      { kind: 'session.slash_commands', commands: ['init', 'review'], skills: [] },
    ]);
    expect(slashCommands(events)).toEqual(['init', 'review']);
  });

  test('sessionModels reads the newest event, else null', () => {
    expect(sessionModels(envelopes('s', [{ kind: 'model.changed', model: 'm' }]))).toBeNull();
    const old = [{ value: 'old', label: 'Old', description: null }];
    const last = [
      { value: 'default', label: 'Default', description: 'The default model' },
      { value: 'opus', label: 'Opus', description: null },
    ];
    const events = envelopes('s', [
      { kind: 'session.models', models: old },
      { kind: 'model.changed', model: 'm' },
      { kind: 'session.models', models: last },
    ]);
    expect(sessionModels(events)).toEqual(last);
  });
});

describe('hibernation (R6.6)', () => {
  test('a hibernated session shows a notice with the idle time', () => {
    const items = buildTimeline(
      envelopes('s', [{ kind: 'session.hibernated', after_min: 30 }]),
    ).items;
    expect(items).toMatchObject([
      { type: 'notice', text: 'Hibernated after 30 min idle. Send a prompt to resume it.' },
    ]);
  });
});

describe('the run item and input_id (DL-179)', () => {
  const runOf = (extra: Record<string, unknown>) => {
    const tree = buildTimeline(
      envelopes('s', [
        { kind: 'run.started', run_id: 'run-1', input_summary: 'go', origin: 'board', ...extra },
      ]),
    );
    return tree.items.find((i) => i.type === 'run');
  };
  test('the run item keeps the input_id of the event', () => {
    expect(runOf({ input_id: 'in-1' })).toMatchObject({ type: 'run', input_id: 'in-1' });
  });
  test('an older event has no input_id', () => {
    expect(runOf({})).not.toHaveProperty('input_id');
  });
});

describe('the tool use of a subagent (DL-191)', () => {
  const started = (node_id: string, provider_refs: Record<string, unknown>): Event => ({
    kind: 'tool.started',
    node_id,
    parent_node_id: null,
    tool_kind: 'subagent',
    title: 'Agent',
    input_summary: 'work',
    files: [],
    provider_refs: provider_refs as Extract<Event, { kind: 'tool.started' }>['provider_refs'],
  });

  test('tool_use_id comes from the provider refs, else it is null', () => {
    const tree = buildTimeline(
      envelopes('tu', [
        started('a', { tool_use_id: 'toolu_01' }),
        started('b', {}),
        started('c', { tool_use_id: 7 }),
      ]),
    );
    expect(subs(tree.items).map((s) => s.tool_use_id)).toEqual(['toolu_01', null, null]);
  });
});

describe('the whole prompt of a run (backlog B10)', () => {
  const runs = (items: readonly Item[]) => items.flatMap((i) => (i.type === 'run' ? [i] : []));

  test('a run carries text from input_text, and the summary stays the input', () => {
    const run = runs(buildTimeline(recorded('subagent')).items)[0];
    expect(run?.text).toStartWith('Use the Task tool to start a general-purpose subagent');
    expect(run?.input).toStartWith('Use the Task tool to start a general-purpose subagent');
  });

  test('a run from an event without input_text has no text', () => {
    const events = envelopes('old', [
      { kind: 'run.started', run_id: 'run-1', input_summary: 'go', origin: 'board' },
    ]);
    const run = runs(buildTimeline(events).items)[0];
    expect(run?.input).toBe('go');
    expect(run).not.toHaveProperty('text');
  });

  test('input_text and the summary can differ', () => {
    const events = envelopes('long', [
      {
        kind: 'run.started',
        run_id: 'run-1',
        input_summary: 'first line…',
        input_text: 'first line\nsecond line',
        origin: 'board',
      },
    ]);
    const run = runs(buildTimeline(events).items)[0];
    expect(run?.input).toBe('first line…');
    expect(run?.text).toBe('first line\nsecond line');
  });
});

describe('effort notices (B18)', () => {
  const effort = (value: string | null): Event => ({ kind: 'effort.changed', effort: value });
  const started: Event = {
    kind: 'session.started',
    provider: 'claude',
    provider_instance: 'claude',
    provider_version: null,
    model: null,
    cwd: '/w',
    mode: null,
    capabilities: claudeCaps(),
    resumed: null,
    provider_refs: {},
  };
  const texts = (events: readonly Event[]): string[] =>
    buildTimeline(envelopes('e', events)).items.map((i) => (i.type === 'notice' ? i.text : i.type));

  test('the first report adds no notice', () => {
    expect(texts([effort('high')])).toEqual([]);
  });

  test('a change adds a notice with the effort in the title', () => {
    const tree = buildTimeline(envelopes('e', [effort('high'), effort('low')]));
    expect(tree.items).toEqual([
      expect.objectContaining({
        type: 'notice',
        text: 'Effort changed to low',
        title: 'Reasoning effort: low',
        tone: 'info',
      }),
    ]);
  });

  test('the same value twice adds nothing', () => {
    expect(texts([effort('high'), effort('high')])).toEqual([]);
  });

  test('null adds nothing, and the last value stays', () => {
    expect(texts([effort(null)])).toEqual([]);
    expect(texts([effort('low'), effort(null), effort('low')])).toEqual([]);
    expect(texts([effort('high'), effort(null), effort('low')])).toEqual(['Effort changed to low']);
  });

  test('the first report of a new process adds no notice', () => {
    expect(texts([effort('high'), started, effort('low')])).toEqual([]);
    expect(texts([effort('high'), started, effort('high'), effort('low')])).toEqual([
      'Effort changed to low',
    ]);
  });

  test('a model change and an effort change give two notices, the model alone in its own', () => {
    expect(
      texts([effort('high'), { kind: 'model.changed', model: 'claude-sonnet-5-5' }, effort('low')]),
    ).toEqual(['Model changed to Sonnet 5.5', 'Effort changed to low']);
  });
});

describe('effort.set_result notices (B18 Phase 2)', () => {
  const result = (
    outcome: Extract<Event, { kind: 'effort.set_result' }>['outcome'],
    extra: { effort?: string | null; message?: string } = {},
  ): Event => ({
    kind: 'effort.set_result',
    control_id: 'fx-1',
    requested: 'max',
    effort: extra.effort ?? null,
    outcome,
    ...(extra.message === undefined ? {} : { message: extra.message }),
  });
  const items = (event: Event) => buildTimeline(envelopes('e', [event])).items;

  test('accepted, applied and superseded add no notice', () => {
    expect(items(result('accepted'))).toEqual([]);
    expect(items(result('applied', { effort: 'max' }))).toEqual([]);
    expect(items(result('superseded'))).toEqual([]);
  });

  test('adjusted names the level that runs', () => {
    expect(items(result('adjusted', { effort: 'high' }))).toEqual([
      expect.objectContaining({
        type: 'notice',
        text: 'Effort is high, not max',
        tone: 'info',
      }),
    ]);
  });

  test('adjusted with no level says that the model has no effort setting', () => {
    expect(items(result('adjusted'))).toEqual([
      expect.objectContaining({
        type: 'notice',
        text: 'This model does not use an effort setting',
        tone: 'info',
      }),
    ]);
  });

  test('unverified says that the provider does not report the level', () => {
    expect(items(result('unverified'))).toEqual([
      expect.objectContaining({
        type: 'notice',
        text: 'Effort set to max; the provider does not report it',
        tone: 'info',
      }),
    ]);
  });

  test('refused is an error, with the message when there is one', () => {
    expect(items(result('refused', { message: 'not offered' }))).toEqual([
      expect.objectContaining({
        type: 'notice',
        text: 'Effort not changed: not offered',
        tone: 'error',
      }),
    ]);
    expect(items(result('refused'))).toEqual([
      expect.objectContaining({ type: 'notice', text: 'Effort not changed.', tone: 'error' }),
    ]);
  });

  test('a result adds no effort.changed state: a later change still notices against the last report', () => {
    const changed = (effort: string): Event => ({ kind: 'effort.changed', effort });
    const tree = buildTimeline(
      envelopes('e', [
        changed('high'),
        result('accepted'),
        result('applied', { effort: 'low' }),
        changed('low'),
      ]),
    );
    expect(tree.items.map((i) => (i.type === 'notice' ? i.text : i.type))).toEqual([
      'Effort changed to low',
    ]);
  });
});
