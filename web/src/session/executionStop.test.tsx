// BG-4: stop the processes of a lost run (actions, plan view, confirmation, polling, results).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ExecutionCheck, ExecutionStopOutcome, ExecutionStopPlan } from '../gen';
import { ApiError } from '../runtime/api';
import { sessionActions, STOP_TEXTS } from './commands';
import { markAndResume, RESUME_TEXTS } from './resumeRefusal';
import {
  type Clock,
  ExecutionStop,
  mayMarkAfterStop,
  PlanPanel,
  planIsStale,
  planNotice,
  sendStop,
  stillRuns,
} from './ExecutionStop';

const key = { station_id: 'station a', session_id: 's/1' };

const check = (extra: Partial<ExecutionCheck> = {}): ExecutionCheck => ({
  shim_id: 'shim-1',
  reason: 'lost: the link broke',
  same_boot: true,
  identity_alive: false,
  group_members: [{ pid: 4242, command: 'node' }],
  session_members: [],
  escaped: [],
  complete: true,
  error: null,
  ...extra,
});

const plan = (extra: Partial<ExecutionStopPlan> = {}): ExecutionStopPlan => ({
  plan_id: 'plan-1',
  expires_at: 1_792_400_030_000,
  eligible: true,
  code: null,
  targets: [
    { pid: 4242, command: 'node' },
    { pid: 4243, command: 'worker' },
  ],
  escaped: [{ pid: 77, command: 'sleep' }],
  check: check(),
  ...extra,
});

const lastStop = (extra: Partial<ExecutionStopOutcome> = {}): ExecutionStopOutcome => ({
  command_id: 'c1',
  outcome: 'done',
  code: null,
  term_sent: 3,
  kill_sent: 1,
  gone: 3,
  skipped: 0,
  ...extra,
});

const CODES = [
  [
    'identity_unproven',
    'Fleet Deck cannot safely prove that these processes belong to the previous run. No processes were stopped.',
  ],
  [
    'inspection_incomplete',
    'Fleet Deck could not complete the safety check. No processes were stopped.',
  ],
  ['plan_changed', 'The process list changed. Review it again before stopping processes.'],
  ['permission_denied', 'Fleet Deck could not stop one or more processes as the station user.'],
  ['processes_remain', 'Some processes remain. Review the updated list.'],
  [
    'outcome_unknown',
    'The stop result is unknown. Refresh the process check before taking another action.',
  ],
] as const;

describe('stop texts', () => {
  test('the six codes have the exact texts of the design', () => {
    expect(Object.keys(STOP_TEXTS).sort()).toEqual(CODES.map(([c]) => c).sort());
    for (const [code, text] of CODES) expect(STOP_TEXTS[code]).toBe(text);
  });

  test('a plan that is not eligible shows the text of its code and no confirm', () => {
    for (const [code, text] of CODES) {
      expect(planNotice(plan({ eligible: false, code, targets: [] }))).toBe(text);
    }
    expect(planNotice(plan({ eligible: false, code: null, targets: [] }))).toBe(
      'No process of this run is left to stop. Check again, then use Mark as ended.',
    );
    expect(planNotice(plan({ eligible: false, code: 'other', targets: [] }))).toBe(
      'The deck sent an invalid stop plan.',
    );
    expect(planNotice(plan())).toBeNull();
  });
});

describe('stop actions', () => {
  test('stopPlan reads the encoded path and checks the shape and the code', async () => {
    const paths: string[] = [];
    const get = async (path: string) => {
      paths.push(path);
      return plan();
    };
    const actions = sessionActions(
      async () => ({}),
      key,
      ['admin'],
      () => 'c1',
      get,
    );
    expect(await actions.stopPlan('shim/1')).toEqual(plan());
    expect(paths).toEqual(['/api/sessions/station%20a/s%2F1/execution/stop-plan?shim_id=shim%2F1']);

    for (const bad of [{ x: 1 }, plan({ code: 'nope' }), { ...plan(), targets: [{ pid: 'x' }] }]) {
      const reader = sessionActions(
        async () => ({}),
        key,
        ['admin'],
        () => 'c1',
        async () => bad,
      );
      await expect(reader.stopPlan('s')).rejects.toThrow('The deck sent an invalid stop plan.');
    }
    const none = sessionActions(async () => ({}), key, ['admin']);
    await expect(none.stopPlan('s')).rejects.toThrow('No reader.');
  });

  test('a station that cannot stop shows the message of the deck as is', async () => {
    const text = "This station's build cannot stop the processes of a lost run from the board.";
    const actions = sessionActions(
      async () => ({}),
      key,
      ['admin'],
      () => 'c1',
      async () => {
        throw new ApiError(501, text, 'unsupported');
      },
    );
    await expect(actions.stopPlan('shim-1')).rejects.toThrow(text);
  });

  test('stopExecution posts the command with the elevation and the id of the caller', async () => {
    const posts: [string, unknown][] = [];
    const actions = sessionActions(
      async (path, body) => {
        posts.push([path, body]);
        return { state: 'done', receipt: { state: 'accepted' } };
      },
      key,
      ['admin'],
      () => 'other',
    );
    expect(await actions.stopExecution('shim-1', 'plan-1', 'elev-1', 'mine')).toEqual({
      ok: true,
      text: 'Done.',
    });
    expect(posts).toEqual([
      [
        '/api/commands/session.stop_execution',
        {
          station: 'station a',
          command_id: 'mine',
          command: {
            op: 'stop_execution',
            session_id: 's/1',
            shim_id: 'shim-1',
            plan_id: 'plan-1',
          },
          elevation: 'elev-1',
        },
      ],
    ]);
  });

  test('the other commands post no elevation', async () => {
    const posts: unknown[] = [];
    const actions = sessionActions(
      async (_path, body) => {
        posts.push(body);
        return { state: 'done', receipt: { state: 'accepted' } };
      },
      key,
      ['admin'],
      () => 'c1',
    );
    await actions.reconcileExecution('shim-1');
    expect(Object.keys(posts[0] as object)).toEqual(['station', 'command_id', 'command']);
  });

  test('a rejected receipt shows the station message', async () => {
    const actions = sessionActions(
      async () => ({
        state: 'done',
        receipt: { state: 'rejected', reject: { message: 'busy with a resume' } },
      }),
      key,
      ['admin'],
    );
    expect(await actions.stopExecution('s', 'p', 'e', 'i')).toEqual({
      ok: false,
      text: 'Refused: busy with a resume',
    });
  });
});

/** A clock that moves only when the code sleeps. */
const fakeClock = (): Clock & { slept: number[] } => {
  let t = 0;
  const slept: number[] = [];
  return {
    slept,
    now: () => t,
    sleep: async (ms) => {
      slept.push(ms);
      t += ms;
    },
  };
};

describe('the confirmed stop', () => {
  const setup = (reads: ExecutionCheck[], elevation: string | null = 'elev-1') => {
    const posts: unknown[] = [];
    let n = 0;
    const base = sessionActions(
      async (_path, body) => {
        posts.push(body);
        return { state: 'done', receipt: { state: 'accepted' } };
      },
      key,
      ['admin'],
      () => 'unused',
    );
    const actions = {
      ...base,
      // "Mark as ended" is a separate action: the stop never presses it.
      reconcileExecution: async () => {
        throw new Error('Mark as ended was pressed');
      },
      checkExecution: async () => reads[Math.min(n++, reads.length - 1)] as ExecutionCheck,
    };
    const clock = fakeClock();
    const asked: (readonly string[])[] = [];
    const deps = {
      actions,
      ask: async (labels: readonly string[]) => {
        asked.push(labels);
        return elevation;
      },
      clock,
      newId: () => 'c1',
    };
    return { deps, posts, clock, asked, reads: () => n };
  };

  test('it asks the elevation, sends the body, polls until the matching last_stop', async () => {
    const t = setup([
      check(),
      check({ last_stop: lastStop({ command_id: 'older' }) }),
      check({ group_members: [], last_stop: lastStop() }),
    ]);
    const result = await sendStop(t.deps, 'shim-1', plan());
    expect(t.asked).toEqual([['Stop previous run processes']]);
    expect(t.posts).toEqual([
      {
        station: 'station a',
        command_id: 'c1',
        command: { op: 'stop_execution', session_id: 's/1', shim_id: 'shim-1', plan_id: 'plan-1' },
        elevation: 'elev-1',
      },
    ]);
    expect(t.clock.slept).toEqual([1000, 1000, 1000]);
    expect(result).toMatchObject({
      kind: 'done',
      text: 'Fleet Deck stopped 3 processes. 1 needed a force stop.',
      alert: false,
    });
    // Only the stop command was posted: "Mark as ended" is never pressed.
    expect(t.posts).toHaveLength(1);
  });

  test('a code in the result shows the text of the code', async () => {
    const t = setup([
      check({ last_stop: lastStop({ outcome: 'failed', code: 'processes_remain' }) }),
    ]);
    expect(await sendStop(t.deps, 'shim-1', plan())).toMatchObject({
      kind: 'done',
      text: 'Some processes remain. Review the updated list.',
      alert: true,
    });
  });

  test('a code outside the six shows the unknown text', async () => {
    const t = setup([check({ last_stop: lastStop({ outcome: 'failed', code: 'weird' }) })]);
    expect(await sendStop(t.deps, 'shim-1', plan())).toMatchObject({
      kind: 'done',
      text: STOP_TEXTS['outcome_unknown'],
      alert: true,
    });
  });

  test('no matching last_stop in 20 seconds gives the unknown text', async () => {
    const t = setup([check({ last_stop: lastStop({ command_id: 'older' }) })]);
    const result = await sendStop(t.deps, 'shim-1', plan());
    expect(result).toMatchObject({
      kind: 'done',
      text: 'The stop result is unknown. Refresh the process check before taking another action.',
      alert: true,
    });
    expect(t.clock.slept).toHaveLength(20);
    expect(t.reads()).toBe(20);
  });

  test('a cancelled elevation sends nothing', async () => {
    const t = setup([check()], null);
    expect(await sendStop(t.deps, 'shim-1', plan())).toEqual({ kind: 'cancelled' });
    expect(t.posts).toEqual([]);
    expect(t.reads()).toBe(0);
  });

  test('a refused command shows its message and does not poll', async () => {
    const t = setup([check()]);
    const failing = {
      ...t.deps.actions,
      stopExecution: async () => ({ ok: false, text: 'Refused: plan expired' }),
    };
    expect(await sendStop({ ...t.deps, actions: failing }, 'shim-1', plan())).toEqual({
      kind: 'failed',
      text: 'Refused: plan expired',
    });
    expect(t.reads()).toBe(0);
  });
});

describe('markup', () => {
  const actions = sessionActions(async () => ({}), key, ['admin']);
  const render = (runs: boolean) =>
    renderToStaticMarkup(
      <ExecutionStop actions={actions} shimId="shim-1" stillRuns={runs} onCheck={() => {}} />,
    );

  test('the button shows only when the check shows a live process of the run', () => {
    expect(render(true)).toContain('Stop these processes…');
    expect(render(false)).not.toContain('Stop these processes…');
    expect(stillRuns(check())).toBe(true);
    expect(stillRuns(check({ group_members: [], session_members: [] }))).toBe(false);
    expect(stillRuns(check({ group_members: [], identity_alive: true }))).toBe(true);
    expect(
      stillRuns(check({ group_members: [], session_members: [{ pid: 1, command: 'sh' }] })),
    ).toBe(true);
  });

  test('a plan older than 25 seconds is read again', () => {
    expect(planIsStale(1_000, 26_000)).toBe(false);
    expect(planIsStale(1_000, 26_001)).toBe(true);
  });

  test('the plan review lists the targets, the escaped note and the confirmation', () => {
    const html = renderToStaticMarkup(
      <PlanPanel plan={plan()} busy={false} onConfirm={() => {}} onCancel={() => {}} />,
    );
    expect(html).toContain('Processes to stop');
    expect(html).toContain('4242 node');
    expect(html).toContain('4243 worker');
    expect(html).toContain('77 sleep');
    expect(html).toContain(
      'Processes outside the run&#x27;s group and session are listed for information and will not be stopped.',
    );
    expect(html).toContain(
      'Stop the listed processes from this previous run? Fleet Deck will ask them to exit, then force-stop surviving targets after five seconds. Unsaved work may be lost.',
    );
    expect(html).toContain('Stop processes');
    expect(html).toContain('Cancel');
  });

  test('a plan without escaped processes has no escaped note', () => {
    const html = renderToStaticMarkup(
      <PlanPanel
        plan={plan({ escaped: [] })}
        busy={false}
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(html).not.toContain('listed for information');
  });
});

describe('stop them and resume (DL-433)', () => {
  const done = (check: ExecutionCheck | null, extra: Partial<ExecutionStopOutcome> = {}) =>
    ({
      kind: 'done',
      text: 'Fleet Deck stopped 3 processes. 1 needed a force stop.',
      alert: typeof extra.code === 'string',
      check,
    }) as const;
  const clean = check({ group_members: [], session_members: [] });

  test('stop them and resume marks and resumes after a done outcome and a clean check', async () => {
    expect(mayMarkAfterStop(done(clean))).toBe(true);
    const calls: string[] = [];
    const result = await markAndResume(
      {
        reconcileExecution: async (id) => {
          calls.push(`reconcile ${id}`);
          return { ok: true, text: 'Done.' };
        },
        resume: async () => {
          calls.push('resume');
          return { ok: true, text: 'Done.' };
        },
      },
      'shim-1',
    );
    expect(result.ok).toBe(true);
    expect(calls).toEqual(['reconcile shim-1', 'resume']);
  });

  test('a stop with processes_remain neither marks nor resumes', () => {
    expect(mayMarkAfterStop(done(clean, { code: 'processes_remain' }))).toBe(false);
    // The same for a stop that ended clean while a process of the run still lives or the check
    // is incomplete or missing, and for a cancelled or failed stop.
    expect(mayMarkAfterStop(done(check()))).toBe(false);
    expect(mayMarkAfterStop(done(check({ group_members: [], complete: false })))).toBe(false);
    expect(mayMarkAfterStop(done(null))).toBe(false);
    expect(mayMarkAfterStop({ kind: 'cancelled' })).toBe(false);
    expect(mayMarkAfterStop({ kind: 'failed', text: 'Refused: plan expired' })).toBe(false);
  });

  test('the confirmation names the mark and the resume', () => {
    const html = (then: 'resume' | null) =>
      renderToStaticMarkup(
        <PlanPanel
          plan={plan()}
          busy={false}
          onConfirm={() => {}}
          onCancel={() => {}}
          then={then}
        />,
      );
    expect(html('resume')).toContain(RESUME_TEXTS.stopConfirm);
    expect(html('resume')).toContain(
      'Unsaved work may be lost. Then Fleet Deck marks the run as ended',
    );
    expect(html(null)).not.toContain('Then Fleet Deck marks');
  });

  test('the action is named Stop them and resume only in resume mode', () => {
    const actions = sessionActions(async () => ({}), key, ['admin']);
    const render = (then: 'resume' | null) =>
      renderToStaticMarkup(
        <ExecutionStop
          actions={actions}
          shimId="shim-1"
          stillRuns={true}
          onCheck={() => {}}
          then={then}
        />,
      );
    expect(render('resume')).toContain('Stop them and resume');
    expect(render('resume')).not.toContain('Stop these processes…');
    expect(render(null)).toContain('Stop these processes…');
  });
});
