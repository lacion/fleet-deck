// DL-182: the check of the previous run (dialog view, session actions, header menu).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, ExecutionCheck } from '../gen';
import { ownerEnv, PeopleProvider } from '../sharing/context';
import { claudeCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { sessionActions } from './commands';
import { type Controller, sessionFlags } from './controller';
import { ExecutionDialog, executionView } from './ExecutionDialog';
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
