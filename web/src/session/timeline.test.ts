import { describe, expect, test } from 'bun:test';
import { summary } from '../testing/summary';
import { nestedSubagents, recorded } from '../testing/timeline';
import { buildTimeline, type Item, runningSubagents, type Subagent } from './timeline';

const subs = (items: readonly Item[]): Subagent[] =>
  items.filter((i): i is Subagent => i.type === 'subagent');

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
