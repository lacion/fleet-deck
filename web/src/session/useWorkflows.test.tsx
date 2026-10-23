// B12: the pages of the workflow projection (design `workflow-agents.md` §4.6 Loading).
import { afterEach, beforeEach, describe, expect, spyOn, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { AgentsMenu } from '../chat/AgentsMenu';
import type {
  Event,
  WorkflowAgent,
  WorkflowRun,
  WorkflowsCursor,
  WorkflowsPage,
  WorkflowUpdate,
} from '../gen';
import { ApiError } from '../runtime/api';
import { envelopes } from '../testing/timeline';
import { sessionActions } from './commands';
import { windowPassed, WorkflowsLoader } from './useWorkflows';
import { type WorkflowsSnapshot, workflowStates } from './workflows';

const state = (more: Partial<WorkflowUpdate> = {}): WorkflowUpdate => ({
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
const run = (run_order: number, more: Partial<WorkflowUpdate> = {}): WorkflowRun => ({
  run_order,
  state: state(more),
});
const cursor = (run_order: number, agent_index: number): WorkflowsCursor => ({
  run_order,
  agent_index,
});
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** A page; `next` is left out when absent (the type has no `undefined`). */
const page = (as_of: number, workflows: WorkflowRun[], next?: WorkflowsCursor): WorkflowsPage =>
  next === undefined ? { as_of, workflows } : { as_of, workflows, next };

/** A fake `load`: it records the cursor of each call and answers from a queue. */
function fake(...answers: (WorkflowsPage | Error)[]) {
  const calls: (WorkflowsCursor | null)[] = [];
  const load = async (after: WorkflowsCursor | null): Promise<WorkflowsPage> => {
    calls.push(after);
    const next = answers.shift();
    if (next === undefined) throw new Error('no answer');
    if (next instanceof Error) throw next;
    return next;
  };
  return { calls, load };
}

const menu = (
  workflows: ReturnType<typeof workflowStates>,
  snapshot: WorkflowsSnapshot | null,
  loading: boolean,
) =>
  renderToStaticMarkup(
    <AgentsMenu
      subagents={[]}
      tasks={new Map()}
      toolTitles={new Map()}
      agent={null}
      onChoose={() => {}}
      workflows={workflows}
      snapshot={snapshot}
      loading={loading}
    />,
  );

const taskOnly = (): ReturnType<typeof workflowStates> =>
  workflowStates(
    null,
    envelopes('s', [
      { kind: 'task.updated', task_id: 'w1', task_kind: 'local_workflow', status: 'running' },
    ]),
  );

let warn: ReturnType<typeof spyOn<Console, 'warn'>>;
beforeEach(() => {
  warn = spyOn(console, 'warn').mockImplementation(() => {});
});
afterEach(() => warn.mockRestore());

describe('WorkflowsLoader (B12)', () => {
  test('useWorkflows reads every page and merges them (a run cut over two pages)', async () => {
    const { calls, load } = fake(
      page(
        10,
        [
          run(5, {
            phases: [{ index: 1, title: 'Alpha' }],
            agents: [agent(0), agent(1)],
            agents_total: 4,
          }),
        ],
        cursor(5, 1),
      ),
      page(12, [
        run(5, { agents: [agent(2), agent(3)], agents_total: 4 }),
        run(7, { task_id: 't2' }),
      ]),
    );
    const loader = new WorkflowsLoader(load, 'a/s');
    expect(loader.getSnapshot()).toEqual({ snapshot: null, loading: true });
    loader.start();
    loader.start();
    await settle();
    expect(calls).toEqual([null, cursor(5, 1)]);
    const { snapshot, loading } = loader.getSnapshot();
    expect(loading).toBe(false);
    expect(snapshot?.incomplete).toBe(false);
    // The smallest `as_of` of the pages: the later events fill the rest.
    expect(snapshot?.as_of).toBe(10);
    expect(snapshot?.runs.map((r) => r.run_order)).toEqual([5, 7]);
    const first = snapshot?.runs[0]?.state;
    expect(first?.agents.map((a) => a.index)).toEqual([0, 1, 2, 3]);
    expect(first?.phases).toEqual([{ index: 1, title: 'Alpha' }]);
    const views = workflowStates(snapshot ?? null, []);
    expect([...views.keys()]).toEqual(['t1', 't2']);
    expect(views.get('t1')?.agents.size).toBe(4);
  });

  test('a cold load of 70 pages reads until next is absent', async () => {
    const calls: (WorkflowsCursor | null)[] = [];
    const load = async (after: WorkflowsCursor | null): Promise<WorkflowsPage> => {
      calls.push(after);
      const at = calls.length;
      const runs = [run(at, { task_id: `t${at}`, agents: [agent(at)], agents_total: 1 })];
      return at < 70 ? page(100, runs, cursor(at, at)) : page(100, runs);
    };
    const loader = new WorkflowsLoader(load, 'a/s');
    loader.start();
    await settle();
    expect(calls).toHaveLength(70);
    const { snapshot } = loader.getSnapshot();
    expect(snapshot?.incomplete).toBe(false);
    expect(snapshot?.runs).toHaveLength(70);
    expect(workflowStates(snapshot ?? null, []).size).toBe(70);
    expect(warn).not.toHaveBeenCalled();
  });

  test('a next that does not advance stops the load as incomplete', async () => {
    const { calls, load } = fake(
      page(10, [run(1)], cursor(1, 0)),
      page(10, [run(2, { task_id: 't2' })], cursor(2, 0)),
      // The third page repeats the second `next`.
      page(10, [run(3, { task_id: 't3' })], cursor(2, 0)),
      page(10, [run(4, { task_id: 't4' })]),
    );
    const loader = new WorkflowsLoader(load, 'a/s');
    loader.start();
    await settle();
    expect(calls).toHaveLength(3);
    const { snapshot, loading } = loader.getSnapshot();
    expect(loading).toBe(false);
    expect(snapshot?.incomplete).toBe(true);
    expect(snapshot?.runs).toHaveLength(3);
    expect(warn).toHaveBeenCalledTimes(1);
    const html = menu(workflowStates(snapshot ?? null, []), snapshot ?? null, false);
    expect(html).toContain(
      'Fleet Deck could not read every workflow of this session. Open the session again to retry.',
    );
  });

  test('a page with next and no workflows stops the load as incomplete', async () => {
    const { calls, load } = fake(page(10, [], cursor(1, 0)), page(10, [run(2)]));
    const loader = new WorkflowsLoader(load, 'a/s');
    loader.start();
    await settle();
    expect(calls).toHaveLength(1);
    expect(loader.getSnapshot().snapshot?.incomplete).toBe(true);
    expect(warn).toHaveBeenCalledTimes(1);
  });

  test('no half-merged snapshot is shown while pages load', async () => {
    let finish: (p: WorkflowsPage) => void = () => {};
    const second = new Promise<WorkflowsPage>((resolve) => {
      finish = resolve;
    });
    const answers: Promise<WorkflowsPage>[] = [
      Promise.resolve(page(10, [run(1)], cursor(1, 0))),
      second,
    ];
    const loader = new WorkflowsLoader(
      () => answers.shift() ?? Promise.reject(new Error('x')),
      'a/s',
    );
    loader.start();
    await settle();
    expect(loader.getSnapshot()).toEqual({ snapshot: null, loading: true });
    expect(menu(taskOnly(), null, true)).toContain('Loading the agents of this workflow…');
    finish(page(10, [run(2, { task_id: 't2' })]));
    await settle();
    expect(loader.getSnapshot().loading).toBe(false);
    expect(loader.getSnapshot().snapshot?.runs).toHaveLength(2);
  });

  test('an error after some pages gives the timeline-only view', async () => {
    const { load } = fake(page(10, [run(1)], cursor(1, 0)), new Error('boom'));
    const loader = new WorkflowsLoader(load, 'a/s');
    loader.start();
    await settle();
    expect(loader.getSnapshot()).toEqual({ snapshot: null, loading: false });
    const html = menu(taskOnly(), null, false);
    expect(html).toContain('This station does not report the agents of a workflow.');
    expect(html).not.toContain('Loading the agents');
  });

  test('a 409 gives the timeline-only view', async () => {
    const { load } = fake(new ApiError(409, 'older station', 'workflows_unsupported'));
    const loader = new WorkflowsLoader(load, 'a/s');
    loader.start();
    await settle();
    expect(loader.getSnapshot()).toEqual({ snapshot: null, loading: false });
  });

  test('a cold load beyond the timeline shows the snapshot', () => {
    const agents = Array.from({ length: 1000 }, (_, index) => agent(index, { state: 'done' }));
    const snapshot: WorkflowsSnapshot = {
      as_of: 5000,
      incomplete: false,
      runs: [run(1, { agents, agents_total: 1000, status: 'completed' })],
    };
    // The window holds only the last events: none of them is an event of those agents.
    const window: Event[] = [
      { kind: 'run.started', run_id: 'r', input_summary: 'x', origin: 'board' },
    ];
    const events = envelopes('s', window).map((e) => ({ ...e, session_seq: 5001 }));
    const w = workflowStates(snapshot, events).get('t1');
    expect(w?.agents.size).toBe(1000);
    expect(w?.agents.get(999)?.state).toBe('done');
    expect(windowPassed(snapshot, events)).toBe(false);
  });

  test('continued viewing past the window reads the pages again once', async () => {
    const { calls, load } = fake(page(10, [run(1)]), page(1500, [run(1)]));
    const loader = new WorkflowsLoader(load, 'a/s');
    loader.start();
    await settle();
    const first = loader.getSnapshot().snapshot;
    // The oldest retained event is far after the snapshot: the window dropped events.
    const events = envelopes('s', [
      { kind: 'run.started', run_id: 'r', input_summary: 'x', origin: 'board' },
    ]).map((e) => ({ ...e, session_seq: 500 }));
    expect(windowPassed(first ?? null, events)).toBe(true);
    // The old snapshot stays while the pages are read again, and a second call waits.
    loader.reload();
    loader.reload();
    expect(loader.getSnapshot()).toEqual({ snapshot: first ?? null, loading: true });
    await settle();
    expect(calls).toHaveLength(2);
    const second = loader.getSnapshot().snapshot;
    expect(second?.as_of).toBe(1500);
    expect(windowPassed(second ?? null, events)).toBe(false);
  });

  test('a disposed loader drops the answer', async () => {
    const { load } = fake(page(10, [run(1)]));
    const loader = new WorkflowsLoader(load, 'a/s');
    loader.start();
    loader.dispose();
    await settle();
    expect(loader.getSnapshot()).toEqual({ snapshot: null, loading: true });
  });
});

describe('the workflow actions (B12)', () => {
  const key = { station_id: 'station-a', session_id: 's1' };

  test('the workflows action sends the cursor as after_order and after_index', async () => {
    const paths: string[] = [];
    const get = async (path: string): Promise<unknown> => {
      paths.push(path);
      return { as_of: 1, workflows: [] };
    };
    const actions = sessionActions(async () => ({}), key, ['read'], undefined, get);
    await actions.workflows?.(null);
    await actions.workflows?.(cursor(5, 2));
    expect(paths).toEqual([
      '/api/sessions/station-a/s1/workflows',
      '/api/sessions/station-a/s1/workflows?after_order=5&after_index=2',
    ]);
  });

  test('the workflow agent action reads the log of one agent', async () => {
    const paths: string[] = [];
    const get = async (path: string): Promise<unknown> => {
      paths.push(path);
      return path.endsWith('bad') ? { nope: true } : { items: [], state: { kind: 'empty' } };
    };
    const actions = sessionActions(async () => ({}), key, ['read'], undefined, get);
    expect((await actions.workflowAgent?.('a1b2'))?.state.kind).toBe('empty');
    expect(paths).toEqual(['/api/sessions/station-a/s1/workflow-agents/a1b2']);
    await expect(actions.workflowAgent?.('bad')).rejects.toThrow('invalid answer');
  });

  test('without a reader there are no workflow actions', () => {
    const actions = sessionActions(async () => ({}), key, ['read']);
    expect(actions.workflows).toBeNull();
    expect(actions.workflowAgent).toBeNull();
  });

  test('a page that is not a page is refused', async () => {
    const actions = sessionActions(
      async () => ({}),
      key,
      ['read'],
      undefined,
      async () => ({
        workflows: 1,
      }),
    );
    await expect(actions.workflows?.(null)).rejects.toThrow('invalid answer');
  });
});
