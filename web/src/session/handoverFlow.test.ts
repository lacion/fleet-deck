import { describe, expect, test } from 'bun:test';
import type { SessionSummary, ShellInfo } from '../gen';
import type { ShellActions } from '../shell/shell';
import type { Watch } from '../terminal/handover';
import { createTerminalStore } from '../terminal/store';
import { summary } from '../testing/summary';
import type { Outcome, SessionActions } from './commands';
import { handoverFlow } from './handoverFlow';

const key = { station_id: 'st', session_id: 's-1' };
const done: Outcome = { ok: true, text: 'Done.' };
const hold = { shell_id: 'tui-1', pid: 7, since: 1, reason: null };

/** A change of the summary; `release` drops the terminal hold. */
type Change = Partial<SessionSummary> & { readonly release?: true };

/** A summary the test can change; `set` tells the subscribers. */
function fixture(extra: Partial<SessionSummary>) {
  let current: SessionSummary | null = summary('s-1', 'idle', 1, extra);
  const listeners = new Set<() => void>();
  const watch: Watch = {
    get: () => current,
    subscribe: (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
  };
  const set = (next: Change) => {
    const { release, ...rest } = next;
    const updated = summary('s-1', 'idle', 2, { ...extra, ...rest });
    if (release) delete updated.terminal;
    current = updated;
    for (const cb of [...listeners]) cb();
  };
  return { watch, set };
}

function setup(
  extra: Partial<SessionSummary>,
  options: {
    onStop?: (set: (next: Change) => void) => void;
    onClose?: (set: (next: Change) => void) => void;
    onResume?: () => Outcome;
    live?: ShellInfo[];
    handover?: () => Promise<string>;
    release?: () => Promise<void>;
  } = {},
) {
  const f = fixture(extra);
  const calls: string[] = [];
  const actions = {
    stop: async (how: string) => {
      calls.push(`stop:${how}`);
      options.onStop?.(f.set);
      return done;
    },
    resume: async () => {
      calls.push('resume');
      return options.onResume?.() ?? done;
    },
  } as unknown as SessionActions;
  const shells = {
    list: async (station: string) => {
      calls.push(`list:${station}`);
      return options.live ?? [];
    },
    close: async (station: string, shellId: string) => {
      calls.push(`close:${station}/${shellId}`);
      options.onClose?.(f.set);
    },
    handover: async (station: string, sessionId: string, cols: number, rows: number) => {
      calls.push(`handover:${station}/${sessionId}/${cols}x${rows}`);
      return (await options.handover?.()) ?? 'tui-9';
    },
    release: async (station: string, shellId: string) => {
      calls.push(`release:${station}/${shellId}`);
      await options.release?.();
    },
  } as unknown as ShellActions;
  const terminals = createTerminalStore(null);
  const flow = handoverFlow({
    key,
    title: 'Fix login',
    watch: f.watch,
    actions,
    shells,
    terminals,
  });
  return { flow, calls, terminals, set: f.set };
}

const shell = (shellId: string, state: ShellInfo['state']): ShellInfo => ({
  shell_id: shellId,
  cwd: '/repo',
  state,
  cols: 120,
  rows: 32,
  created_at: 1,
  exit_code: null,
});

describe('handover flow (DL-140)', () => {
  test('open stops the session, runs the TUI and adds a tui tab', async () => {
    const t = setup(
      { state: 'running' },
      { onStop: (set) => set({ state: 'exited', current_run: null }) },
    );
    expect(await t.flow.open()).toEqual({ ok: true, text: 'The terminal is open.' });
    expect(t.calls).toEqual(['stop:graceful', 'handover:st/s-1/120x32']);
    expect(t.terminals.getState().tabs).toEqual([
      {
        id: 'st/tui-9',
        station: 'st',
        shellId: 'tui-9',
        kind: 'tui',
        title: 'Fix login',
        session: key,
      },
    ]);
    expect(t.terminals.getState().active).toBe('st/tui-9');
  });

  test('open does not stop a session that has ended already', async () => {
    const t = setup({ state: 'exited' });
    expect((await t.flow.open()).ok).toBe(true);
    expect(t.calls).toEqual(['handover:st/s-1/120x32']);
  });

  test('a failed open adds no tab and returns the text', async () => {
    const t = setup(
      { state: 'exited' },
      {
        handover: async () => {
          throw new Error('The station refused.');
        },
      },
    );
    expect(await t.flow.open()).toEqual({ ok: false, text: 'Not opened: The station refused.' });
    expect(t.terminals.getState().tabs).toEqual([]);
    const held = setup({ state: 'exited', terminal: hold });
    expect(await held.flow.open()).toEqual({
      ok: false,
      text: 'A terminal holds this session already.',
    });
    expect(held.terminals.getState().tabs).toEqual([]);
  });

  test('back with no hold only resumes', async () => {
    const t = setup({ state: 'exited' });
    expect(await t.flow.back()).toEqual(done);
    expect(t.calls).toEqual(['resume']);
  });

  test('back with a live shell closes it, resumes and removes the tab', async () => {
    const t = setup(
      { state: 'exited', terminal: hold },
      {
        live: [shell('other', 'running'), shell('tui-1', 'running')],
        onClose: (set) => set({ release: true }),
      },
    );
    t.terminals.add({
      id: 'st/tui-1',
      station: 'st',
      shellId: 'tui-1',
      kind: 'tui',
      title: 'Fix login',
      session: key,
    });
    expect(await t.flow.back()).toEqual({ ok: true, text: 'Back under board control.' });
    expect(t.calls).toEqual(['list:st', 'close:st/tui-1', 'resume']);
    expect(t.terminals.getState().tabs).toEqual([]);
  });

  test('back with an exited shell does not close it and still removes the tab', async () => {
    const t = setup({ state: 'exited', terminal: hold }, { live: [shell('tui-1', 'exited')] });
    t.terminals.add({
      id: 'st/tui-1',
      station: 'st',
      shellId: 'tui-1',
      kind: 'tui',
      title: 'Fix login',
      session: key,
    });
    // The hold is still set, so returnToBoard waits; the station clears it.
    const back = t.flow.back();
    await Promise.resolve();
    t.set({ release: true });
    expect((await back).ok).toBe(true);
    expect(t.calls).toEqual(['list:st', 'resume']);
    expect(t.terminals.getState().tabs).toEqual([]);
  });

  test('a failed resume keeps the text and still removes the tab', async () => {
    const t = setup(
      { state: 'exited', terminal: hold },
      {
        live: [shell('tui-1', 'running')],
        onClose: (set) => set({ release: true }),
        onResume: () => ({ ok: false, text: 'Refused: busy.' }),
      },
    );
    t.terminals.add({
      id: 'st/tui-1',
      station: 'st',
      shellId: 'tui-1',
      kind: 'tui',
      title: 'Fix login',
      session: key,
    });
    expect(await t.flow.back()).toEqual({
      ok: false,
      text: 'Returned, but the resume failed: Refused: busy.',
    });
    expect(t.terminals.getState().tabs).toEqual([]);
  });

  test('release returns the texts of the hold release', async () => {
    const ok = setup({ state: 'exited', terminal: hold });
    expect(await ok.flow.release('tui-1')).toEqual({
      ok: true,
      text: 'The terminal hold is released.',
    });
    expect(ok.calls).toEqual(['release:st/tui-1']);
    const failing = setup(
      { state: 'exited', terminal: hold },
      {
        release: async () => {
          throw new Error('Not an owner.');
        },
      },
    );
    expect(await failing.flow.release('tui-1')).toEqual({
      ok: false,
      text: 'Not released: Not an owner.',
    });
  });
});
