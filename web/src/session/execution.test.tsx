// DL-182: the check of the previous run (dialog view, session actions, header menu).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, ExecutionCheck } from '../gen';
import { ownerEnv, PeopleProvider } from '../sharing/context';
import { claudeCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { type Outcome, sessionActions } from './commands';
import { type Controller, sessionFlags } from './controller';
import {
  ExecutionBody,
  ExecutionDialog,
  executionView,
  finishResume,
  MARK_UNCONFIRMED,
  markEnded,
} from './ExecutionDialog';
import { markAndResume, RESUME_TEXTS, withResumeCheck } from './resumeRefusal';
import { SessionHeader } from './SessionHeader';

const key = { station_id: 'station a', session_id: 's/1' };

const check = (extra: Partial<ExecutionCheck> = {}): ExecutionCheck => ({
  shim_id: 'shim-1',
  reason: 'lost: the link broke',
  same_boot: true,
  identity_alive: false,
  group_members: [],
  session_members: [],
  escaped: [],
  complete: true,
  error: null,
  ...extra,
});

describe('execution view (DL-182)', () => {
  test('nothing unresolved: the note and no mark', () => {
    const view = executionView(check({ shim_id: null, reason: 'nothing unresolved' }));
    expect(view).toMatchObject({ nothing: true, canMark: false });
    expect(view.lines).toEqual([
      'Nothing to reconcile: no earlier run of this session is unresolved.',
    ]);
  });

  test('a live group member blocks the mark and is listed', () => {
    const view = executionView(check({ group_members: [{ pid: 4242, command: 'node' }] }));
    expect(view.canMark).toBe(false);
    expect(view.lines).toEqual([
      'Why it is unresolved: lost: the link broke',
      'Same boot as the run: yes',
      'The provider process is alive: no',
    ]);
    expect(view.lists).toEqual([
      { title: 'Processes in its process group', rows: ['4242 node'] },
      { title: 'Processes in its session', rows: ['none'] },
    ]);
  });

  test('a live session member or a live provider blocks the mark', () => {
    expect(executionView(check({ session_members: [{ pid: 1, command: 'sh' }] })).canMark).toBe(
      false,
    );
    expect(executionView(check({ identity_alive: true })).canMark).toBe(false);
  });

  test('an incomplete check cannot be marked and says why', () => {
    const view = executionView(check({ complete: false, error: 'cannot read /proc/9' }));
    expect(view.canMark).toBe(false);
    expect(view.incomplete).toBe(
      'The check is incomplete: cannot read /proc/9. Fleet Deck cannot prove that the run ended.',
    );
    expect(executionView(check({ complete: false })).incomplete).toContain(
      'The check is incomplete: unknown reason.',
    );
  });

  test('a complete check with nothing alive can be marked', () => {
    const view = executionView(check({ same_boot: false }));
    expect(view).toMatchObject({ nothing: false, canMark: true, incomplete: null });
    expect(view.lines[1]).toBe('Same boot as the run: no');
  });

  test('escaped processes are listed and do not block', () => {
    const view = executionView(check({ escaped: [{ pid: 77, command: 'sleep' }] }));
    expect(view.canMark).toBe(true);
    expect(view.lists.at(-1)).toEqual({
      title: 'Processes that left its group (they do not block)',
      rows: ['77 sleep'],
    });
    expect(executionView(check()).lists).toHaveLength(2);
  });
});

describe('execution dialog markup', () => {
  test('the first render is the dialog that checks the processes', () => {
    const actions = sessionActions(async () => ({}), key, ['admin']);
    const html = renderToStaticMarkup(<ExecutionDialog actions={actions} onClose={() => {}} />);
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-modal="true"');
    expect(html).toContain('Previous run of this session');
    expect(html).toContain('Checking the processes…');
    expect(html).toContain('Check again');
    expect(html).toContain('Close');
  });

  test('the dialog has the head close button and no text Close button', () => {
    const actions = sessionActions(async () => ({}), key, ['admin']);
    const html = renderToStaticMarkup(<ExecutionDialog actions={actions} onClose={() => {}} />);
    expect(html).toContain('fd-dialog-head');
    expect(html).toContain('class="fd-iconbtn"');
    expect(html).toContain('aria-label="Close"');
    expect(html).not.toMatch(/>\s*Close\s*</);
    expect(html).toContain('Check again');
  });

  test('the resume mode has its own head', () => {
    const actions = sessionActions(async () => ({}), key, ['admin']);
    const html = renderToStaticMarkup(
      <ExecutionDialog
        actions={actions}
        purpose="resume"
        onResumed={() => {}}
        onClose={() => {}}
      />,
    );
    expect(html).toContain('Resume needs the previous run to end');
    expect(html).toContain(RESUME_TEXTS.description);
    expect(html).toContain('aria-label="Previous run of this session"');
  });

  test('the menu mode has no description', () => {
    const actions = sessionActions(async () => ({}), key, ['admin']);
    const html = renderToStaticMarkup(<ExecutionDialog actions={actions} onClose={() => {}} />);
    expect(html).not.toContain('Resume needs the previous run to end');
  });
});

describe('execution dialog body', () => {
  const actions = sessionActions(async () => ({}), key, ['admin']);
  const body = (purpose: 'menu' | 'resume', c: ExecutionCheck, markError: string | null = null) =>
    renderToStaticMarkup(
      <ExecutionBody
        check={c}
        purpose={purpose}
        actions={actions}
        busy={false}
        marked={false}
        markError={markError}
        onAct={() => {}}
        onCheck={() => {}}
      />,
    );

  test('resume mode, nothing lives: the note and Mark as ended and resume', () => {
    const html = body('resume', check());
    expect(html).toContain(RESUME_TEXTS.nothingLives);
    expect(html).toContain('Mark as ended and resume');
    expect(html).toContain('fd-danger');
    expect(html).not.toContain('Stop them and resume');
  });

  test('resume mode with nothing unresolved offers Resume', () => {
    const html = body('resume', check({ shim_id: null, reason: 'nothing unresolved' }));
    expect(html).toContain('Nothing to reconcile: no earlier run of this session is unresolved.');
    expect(html).toContain('<button type="button" class="fd-primary">Resume</button>');
    expect(html).not.toContain('Mark as ended');
  });

  test('the menu mode with nothing unresolved has no Resume', () => {
    const html = body('menu', check({ shim_id: null, reason: 'nothing unresolved' }));
    expect(html).not.toContain('fd-primary');
  });

  test('resume mode, live processes: Stop them and resume, and no mark', () => {
    const html = body('resume', check({ group_members: [{ pid: 4242, command: 'node' }] }));
    expect(html).toContain('Stop them and resume');
    expect(html).not.toContain('Stop these processes');
    expect(html).not.toContain('Mark as ended and resume');
  });

  test('resume mode, incomplete check: its text and no mark button', () => {
    const html = body('resume', check({ complete: false, error: 'cannot read /proc/9' }));
    expect(html).toContain('The check is incomplete: cannot read /proc/9.');
    expect(html).not.toContain('Mark as ended');
  });

  test('a refusal of the mark or the resume shows its text as an alert', () => {
    const html = body('resume', check(), 'Refused: a process lives');
    expect(html).toContain('role="alert">Refused: a process lives');
  });

  test('the menu mode keeps its texts', () => {
    const html = body('menu', check());
    expect(html).toContain('Mark as ended');
    expect(html).not.toContain('Mark as ended and resume');
    expect(html).toContain('Mark it as ended only when you know that the previous run stopped.');
  });
});

describe('a refused resume (DL-433)', () => {
  const refusal = 'Refused: not_resumable: the previous execution may still run (shim x)';
  const wrapped = (scopes: string[]) => {
    const base = sessionActions(async () => ({}), key, scopes);
    const outcome: Outcome = { ok: false, text: refusal };
    let opened = 0;
    const actions = withResumeCheck({ ...base, resume: async () => outcome }, () => {
      opened += 1;
    });
    return { actions, opened: () => opened };
  };

  test('a refused resume opens the dialog in resume mode (owner)', async () => {
    const t = wrapped(['admin']);
    expect((await t.actions.resume()).text).toBe(refusal);
    expect(t.opened()).toBe(1);
  });

  test('a member sees only the refusal', async () => {
    const t = wrapped(['read', 'drive']);
    expect((await t.actions.resume()).text).toBe(refusal);
    expect(t.opened()).toBe(0);
  });
});

describe('execution session actions', () => {
  test('checkExecution reads the encoded path and checks the shape', async () => {
    const paths: string[] = [];
    const get = async (path: string) => {
      paths.push(path);
      return check();
    };
    const actions = sessionActions(
      async () => ({}),
      key,
      ['admin'],
      () => 'c1',
      get,
    );
    expect(actions.canAdmin).toBe(true);
    expect(await actions.checkExecution()).toEqual(check());
    expect(paths).toEqual(['/api/sessions/station%20a/s%2F1/execution']);

    const bad = sessionActions(
      async () => ({}),
      key,
      ['admin'],
      () => 'c1',
      async () => ({ x: 1 }),
    );
    await expect(bad.checkExecution()).rejects.toThrow('The deck sent an invalid execution check.');
    const none = sessionActions(async () => ({}), key, ['admin']);
    await expect(none.checkExecution()).rejects.toThrow('No reader.');
  });

  test('reconcileExecution posts the command', async () => {
    const posts: [string, unknown][] = [];
    const actions = sessionActions(
      async (path, body) => {
        posts.push([path, body]);
        return { state: 'done', receipt: { state: 'accepted' } };
      },
      key,
      ['read', 'drive'],
      () => 'c1',
    );
    expect(actions.canAdmin).toBe(false);
    expect(await actions.reconcileExecution('shim-1')).toEqual({ ok: true, text: 'Done.' });
    expect(posts).toEqual([
      [
        '/api/commands/session.reconcile_execution',
        {
          station: 'station a',
          command_id: 'c1',
          command: { op: 'reconcile_execution', session_id: 's/1', shim_id: 'shim-1' },
        },
      ],
    ]);
  });
});

describe('session menu entry', () => {
  const s: DeckSession = card('s1', 'working', 'station-a', {
    title: 'Fix login',
    capabilities: claudeCaps(),
    current_run: 'run-1',
  });
  const actions = sessionActions(async () => ({}), { station_id: 'station-a', session_id: 's1' }, [
    'read',
    'drive',
    'admin',
  ]);
  const idle: Controller = {
    flags: sessionFlags(s, 'live', actions),
    busy: null,
    outcome: null,
    refusals: {},
    act: () => {},
    dismiss: () => {},
  };
  const render = (onCheckExecution: (() => void) | undefined, member = false) =>
    renderToStaticMarkup(
      <PeopleProvider
        value={
          member
            ? { ...ownerEnv, member: { role: 'collaborator', name: 'Ann', scope: null } }
            : ownerEnv
        }
      >
        <SessionHeader
          session={s}
          stations={[station()]}
          controller={idle}
          actions={actions}
          terminal={null}
          onCheckExecution={onCheckExecution}
        />
      </PeopleProvider>,
    );

  test('the entry shows only with the prop and not for a guest', () => {
    expect(render(() => {})).toContain('Check the previous run');
    expect(render(undefined)).not.toContain('Check the previous run');
    expect(render(() => {}, true)).not.toContain('Check the previous run');
  });
});

describe('the resume steps of the dialog', () => {
  const spy = (marked: Outcome) => {
    const calls: string[] = [];
    const actions = {
      reconcileExecution: async (id: string) => {
        calls.push(`reconcile ${id}`);
        return marked;
      },
      resume: async () => {
        calls.push('resume');
        return { ok: true, text: 'Done.' };
      },
    };
    return { calls, actions };
  };

  test('mark as ended and resume calls reconcile then resume and closes', async () => {
    const t = spy({ ok: true, text: 'Done.' });
    let closed = 0;
    const text = await finishResume(
      () => markAndResume(t.actions, 'shim-1'),
      () => {
        closed += 1;
      },
    );
    expect(text).toBeNull();
    expect(t.calls).toEqual(['reconcile shim-1', 'resume']);
    expect(closed).toBe(1);
  });

  test('a refused reconcile shows its text and does not resume', async () => {
    const t = spy({ ok: false, text: 'Refused: a process of the run lives' });
    let closed = 0;
    const text = await finishResume(
      () => markAndResume(t.actions, 'shim-1'),
      () => {
        closed += 1;
      },
    );
    expect(text).toBe('Refused: a process of the run lives');
    expect(t.calls).toEqual(['reconcile shim-1']);
    expect(closed).toBe(0);
  });

  test('a refused resume keeps the dialog open', async () => {
    let closed = 0;
    const text = await finishResume(
      async () => ({ ok: false, text: 'Refused: busy' }),
      () => {
        closed += 1;
      },
    );
    expect(text).toBe('Refused: busy');
    expect(closed).toBe(0);
  });
});

describe('mark as ended follows a pending command (G4)', () => {
  const PENDING_TEXT = 'Sent. The station has not confirmed it yet.';
  const signal = new AbortController().signal;
  const actionsOf = (sent: Outcome) => ({ reconcileExecution: async () => sent });

  test('a pending mark is followed until the station refuses it', async () => {
    const texts: string[] = [];
    const result = await markEnded(
      actionsOf({
        ok: true,
        text: PENDING_TEXT,
        later: async () => ({ ok: false, text: 'Refused: a process of the run still lives' }),
      }),
      'shim-1',
      signal,
      (t) => texts.push(t),
    );
    expect(result).toEqual({ marked: false, text: 'Refused: a process of the run still lives' });
    expect(texts).toEqual([PENDING_TEXT]);
  });

  test('only a confirmed mark marks', async () => {
    const texts: string[] = [];
    const onPending = (t: string) => texts.push(t);
    expect(
      await markEnded(actionsOf({ ok: true, text: 'Done.' }), 'shim-1', signal, onPending),
    ).toEqual({ marked: true });
    expect(texts).toEqual([]);
    expect(
      await markEnded(
        actionsOf({
          ok: true,
          text: PENDING_TEXT,
          later: async () => ({ ok: true, text: 'Done.' }),
        }),
        'shim-1',
        signal,
        onPending,
      ),
    ).toEqual({ marked: true });
    expect(
      await markEnded(
        actionsOf({ ok: true, text: PENDING_TEXT, later: async () => null }),
        'shim-1',
        signal,
        onPending,
      ),
    ).toEqual({ marked: false, text: MARK_UNCONFIRMED });
  });
});
