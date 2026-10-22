import { afterEach, describe, expect, test } from 'bun:test';
import type { ReactElement, ReactNode } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { SessionSummary } from '../gen';
import type { ShellActions } from '../shell/shell';
import { releaseHold, returnToBoard, startHandover, waitFor } from './handover';
import { KeyBar, ctrlByte } from './KeyBar';
import { CopyAsk, copyPreview, PasteNote, Terminal, termTheme } from './Terminal';
import { TerminalDock } from './TerminalDock';
import { type DockState, type TermTab, createTerminalStore, tabId } from './store';

const tab = (shellId: string, kind: 'shell' | 'tui' = 'shell', title = shellId): TermTab => ({
  id: tabId('st', shellId),
  station: 'st',
  shellId,
  kind,
  title,
  session: kind === 'tui' ? { station_id: 'st', session_id: 's-1' } : null,
});

function memory(initial: Record<string, string> = {}): Storage {
  const data = new Map(Object.entries(initial));
  return {
    get length() {
      return data.size;
    },
    clear: () => data.clear(),
    getItem: (k) => data.get(k) ?? null,
    key: (i) => [...data.keys()][i] ?? null,
    removeItem: (k) => {
      data.delete(k);
    },
    setItem: (k, v) => {
      data.set(k, v);
    },
  };
}

describe('terminal store', () => {
  test('add makes the tab active, opens the dock and keeps a duplicate once', () => {
    const store = createTerminalStore(null);
    expect(store.getState()).toEqual({ tabs: [], active: null, open: false, height: 320 });
    store.add(tab('a'));
    store.add(tab('b'));
    store.add(tab('a'));
    const s = store.getState();
    expect(s.tabs.map((t) => t.id)).toEqual(['st/a', 'st/b']);
    expect(s.active).toBe('st/a');
    expect(s.open).toBe(true);
  });

  test('remove moves the active tab to the next, else the previous, else none', () => {
    const store = createTerminalStore(null);
    for (const id of ['a', 'b', 'c']) store.add(tab(id));
    store.activate('st/b');
    store.remove('st/b');
    expect(store.getState().active).toBe('st/c');
    store.remove('st/c');
    expect(store.getState().active).toBe('st/a');
    store.remove('st/zzz');
    expect(store.getState().tabs).toHaveLength(1);
    store.remove('st/a');
    expect(store.getState()).toMatchObject({ tabs: [], active: null, open: false });
  });

  test('removing a tab that is not active keeps the active tab', () => {
    const store = createTerminalStore(null);
    for (const id of ['a', 'b']) store.add(tab(id));
    store.remove('st/a');
    expect(store.getState().active).toBe('st/b');
  });

  test('subscribers hear changes until they unsubscribe', () => {
    const store = createTerminalStore(null);
    let n = 0;
    const off = store.subscribe(() => {
      n += 1;
    });
    store.add(tab('a'));
    store.setOpen(false);
    off();
    store.setOpen(true);
    expect(n).toBe(2);
  });

  test('state persists under fd-terminals and loads again', () => {
    const storage = memory();
    const store = createTerminalStore(storage);
    store.add(tab('a'));
    store.add(tab('b', 'tui', 'fix login'));
    store.setHeight(400);
    store.setOpen(false);
    expect(JSON.parse(storage.getItem('fd-terminals') ?? 'null')).toEqual(store.getState());
    const again = createTerminalStore(storage).getState();
    expect(again).toEqual(store.getState() as DockState);
    expect(again.tabs[1]?.session).toEqual({ station_id: 'st', session_id: 's-1' });
  });

  test('invalid stored JSON gives the defaults', () => {
    const defaults = { tabs: [], active: null, open: false, height: 320 };
    expect(createTerminalStore(memory({ 'fd-terminals': '{nope' })).getState()).toEqual(defaults);
    expect(
      createTerminalStore(memory({ 'fd-terminals': '{"tabs":[{"id":1}]}' })).getState(),
    ).toEqual(defaults);
    expect(createTerminalStore(memory({ 'fd-terminals': '[]' })).getState()).toEqual(defaults);
  });

  test('height is clamped to 160 and the given max', () => {
    const store = createTerminalStore(null);
    store.setHeight(10);
    expect(store.getState().height).toBe(160);
    store.setHeight(5000);
    expect(store.getState().height).toBe(640);
    store.setHeight(5000, 500);
    expect(store.getState().height).toBe(500);
    store.setHeight(300, 500);
    expect(store.getState().height).toBe(300);
  });
});

describe('ctrlByte', () => {
  test('maps letters and the control punctuation, and leaves the rest', () => {
    expect(ctrlByte('a')).toBe('\x01');
    expect(ctrlByte('C')).toBe('\x03');
    expect(ctrlByte('z')).toBe('\x1a');
    expect(ctrlByte('@')).toBe('\x00');
    expect(ctrlByte('[')).toBe('\x1b');
    expect(ctrlByte('\\')).toBe('\x1c');
    expect(ctrlByte(']')).toBe('\x1d');
    expect(ctrlByte('^')).toBe('\x1e');
    expect(ctrlByte('_')).toBe('\x1f');
    expect(ctrlByte('?')).toBe('\x7f');
    expect(ctrlByte('1')).toBe('1');
    expect(ctrlByte(' ')).toBe(' ');
  });
});

describe('KeyBar', () => {
  test('renders the keys with labels and the Ctrl toggle state', () => {
    const html = renderToStaticMarkup(
      <KeyBar
        send={() => undefined}
        ctrl={true}
        setCtrl={() => undefined}
        onPaste={() => undefined}
      />,
    );
    for (const label of ['Esc', 'Tab', 'Ctrl']) expect(html).toContain(`>${label}<`);
    for (const aria of ['Up', 'Down', 'Left', 'Right']) {
      expect(html).toContain(`aria-label="${aria}"`);
    }
    expect(html).toContain('aria-pressed="true"');
  });

  test('has paging, Home, End, the hidden characters and Paste, in this order after the arrows', () => {
    const html = renderToStaticMarkup(
      <KeyBar
        send={() => undefined}
        ctrl={false}
        setCtrl={() => undefined}
        onPaste={() => undefined}
      />,
    );
    // A key's name: its aria-label (the arrows are icons), else its text.
    const labels = [...html.matchAll(/<button([^>]*)>(.*?)<\/button>/g)].map(
      (m) => /aria-label="([^"]*)"/.exec(m[1] ?? '')?.[1] ?? m[2],
    );
    expect(labels).toEqual([
      'Esc',
      'Tab',
      'Ctrl',
      'Up',
      'Down',
      'Left',
      'Right',
      'PgUp',
      'PgDn',
      'Home',
      'End',
      '|',
      '/',
      '~',
      '-',
      'Paste',
    ]);
  });
});

describe('waitFor', () => {
  test('resolves true at once when the predicate holds', async () => {
    let subscribed = 0;
    const ok = await waitFor(
      () => 1,
      () => {
        subscribed += 1;
        return () => undefined;
      },
      (v) => v === 1,
      1000,
    );
    expect(ok).toBe(true);
    expect(subscribed).toBe(0);
  });

  test('resolves false at the timeout and unsubscribes', async () => {
    let off = 0;
    const ok = await waitFor(
      () => 0,
      () => () => {
        off += 1;
      },
      (v) => v === 1,
      5,
    );
    expect(ok).toBe(false);
    expect(off).toBe(1);
  });

  test('resolves true when a change makes the predicate hold, and unsubscribes', async () => {
    let value = 0;
    let listener: () => void = () => undefined;
    let off = 0;
    const waiting = waitFor(
      () => value,
      (cb) => {
        listener = cb;
        return () => {
          off += 1;
        };
      },
      (v) => v === 2,
      1000,
    );
    value = 1;
    listener();
    value = 2;
    listener();
    expect(await waiting).toBe(true);
    expect(off).toBe(1);
  });
});

/** A session whose summary the test changes by hand. */
function fixture(initial: Partial<SessionSummary> | null) {
  let summary = initial === null ? null : ({ current_run: null, ...initial } as SessionSummary);
  const listeners = new Set<() => void>();
  return {
    watch: {
      get: () => summary,
      subscribe: (cb: () => void) => {
        listeners.add(cb);
        return () => {
          listeners.delete(cb);
        };
      },
    },
    set(next: Partial<SessionSummary>) {
      summary = { ...summary, ...next } as SessionSummary;
      for (const cb of [...listeners]) cb();
    },
    releaseHold() {
      const { terminal: _held, ...rest } = summary as SessionSummary;
      summary = rest as SessionSummary;
      for (const cb of [...listeners]) cb();
    },
    listeners,
  };
}

const hold = { shell_id: 'tui-1', pid: 7, since: 1, reason: null };

describe('startHandover', () => {
  test('a running session is stopped, awaited, then handed over', async () => {
    const f = fixture({ state: 'running', current_run: 'r-1' });
    const calls: string[] = [];
    const result = startHandover({
      watch: f.watch,
      stop: async () => {
        calls.push('stop');
        queueMicrotask(() => f.set({ state: 'stopping' }));
        setTimeout(() => f.set({ state: 'exited', current_run: null }), 5);
        return { ok: true, text: 'Stopping.' };
      },
      handover: async () => {
        calls.push('handover');
        return 'tui-9';
      },
    });
    expect(await result).toEqual({ ok: true, shellId: 'tui-9' });
    expect(calls).toEqual(['stop', 'handover']);
    expect(f.listeners.size).toBe(0);
  });

  test('an ended session is handed over without a stop', async () => {
    const f = fixture({ state: 'lost' });
    let stops = 0;
    const result = await startHandover({
      watch: f.watch,
      stop: async () => {
        stops += 1;
        return { ok: true, text: '' };
      },
      handover: async () => 'tui-2',
    });
    expect(result).toEqual({ ok: true, shellId: 'tui-2' });
    expect(stops).toBe(0);
  });

  test('a refused stop fails with its text and does not hand over', async () => {
    const f = fixture({ state: 'running' });
    let handovers = 0;
    const result = await startHandover({
      watch: f.watch,
      stop: async () => ({ ok: false, text: 'Stop refused.' }),
      handover: async () => {
        handovers += 1;
        return 'x';
      },
    });
    expect(result).toEqual({ ok: false, text: 'Stop refused.' });
    expect(handovers).toBe(0);
  });

  test('a session that does not end in time fails', async () => {
    const f = fixture({ state: 'running', current_run: 'r-1' });
    let handovers = 0;
    const result = await startHandover(
      {
        watch: f.watch,
        stop: async () => ({ ok: true, text: '' }),
        handover: async () => {
          handovers += 1;
          return 'x';
        },
      },
      0,
    );
    expect(result).toEqual({
      ok: false,
      text: 'The session is still stopping. Try again when it has stopped.',
    });
    expect(handovers).toBe(0);
  });

  test('an exited session is handed over even when a stale run id is set', async () => {
    const f = fixture({ state: 'exited', current_run: 'r-1' });
    const result = await startHandover({
      watch: f.watch,
      stop: async () => ({ ok: true, text: '' }),
      handover: async () => 'tui-3',
    });
    expect(result.ok).toBe(true);
  });

  test('no summary and a held session fail', async () => {
    const deps = {
      stop: async () => ({ ok: true, text: '' }),
      handover: async () => 'x',
    };
    expect(await startHandover({ ...deps, watch: fixture(null).watch })).toEqual({
      ok: false,
      text: 'The session is not on the board.',
    });
    expect(
      await startHandover({ ...deps, watch: fixture({ state: 'exited', terminal: hold }).watch }),
    ).toEqual({ ok: false, text: 'A terminal holds this session already.' });
  });

  test('a refused handover shows the error', async () => {
    const f = fixture({ state: 'exited' });
    const result = await startHandover({
      watch: f.watch,
      stop: async () => ({ ok: true, text: '' }),
      handover: async () => {
        throw new Error('feature_not_negotiated');
      },
    });
    expect(result).toEqual({ ok: false, text: 'Not opened: feature_not_negotiated' });
  });
});

describe('returnToBoard', () => {
  test('a live shell is closed, the hold awaited, then the session resumed', async () => {
    const f = fixture({ state: 'exited', terminal: hold });
    const calls: string[] = [];
    const result = await returnToBoard({
      watch: f.watch,
      shellLive: async () => true,
      close: async () => {
        calls.push('close');
        setTimeout(() => f.releaseHold(), 5);
      },
      resume: async () => {
        calls.push('resume');
        return { ok: true, text: 'Resuming.' };
      },
    });
    expect(result).toEqual({ ok: true, text: 'Back under board control.' });
    expect(calls).toEqual(['close', 'resume']);
    expect(f.listeners.size).toBe(0);
  });

  test('a shell that is gone is not closed again', async () => {
    const f = fixture({ state: 'exited' });
    let closes = 0;
    const result = await returnToBoard({
      watch: f.watch,
      shellLive: async () => false,
      close: async () => {
        closes += 1;
      },
      resume: async () => ({ ok: true, text: '' }),
    });
    expect(result.ok).toBe(true);
    expect(closes).toBe(0);
  });

  test('a refused resume says so', async () => {
    const f = fixture({ state: 'exited' });
    const result = await returnToBoard({
      watch: f.watch,
      shellLive: async () => false,
      close: async () => undefined,
      resume: async () => ({ ok: false, text: 'No capacity.' }),
    });
    expect(result).toEqual({
      ok: false,
      text: 'Returned, but the resume failed: No capacity.',
    });
  });

  test('a hold that stays fails after the wait and does not resume', async () => {
    const f = fixture({ state: 'exited', terminal: hold });
    let resumes = 0;
    const result = await returnToBoard(
      {
        watch: f.watch,
        shellLive: async () => true,
        close: async () => undefined,
        resume: async () => {
          resumes += 1;
          return { ok: true, text: '' };
        },
      },
      0,
    );
    expect(result).toEqual({
      ok: false,
      text: 'The terminal process has not ended yet. Try again, or end it in the terminal.',
    });
    expect(resumes).toBe(0);
  });

  test('a failed close shows the error', async () => {
    const f = fixture({ state: 'exited', terminal: hold });
    const result = await returnToBoard({
      watch: f.watch,
      shellLive: async () => true,
      close: async () => {
        throw new Error('link down');
      },
      resume: async () => ({ ok: true, text: '' }),
    });
    expect(result).toEqual({ ok: false, text: 'Not closed: link down' });
  });
});

describe('releaseHold', () => {
  test('reports a release and a refusal', async () => {
    expect(await releaseHold(async () => undefined)).toEqual({
      ok: true,
      text: 'The terminal hold is released.',
    });
    expect(
      await releaseHold(async () => {
        throw new Error('no admin');
      }),
    ).toEqual({ ok: false, text: 'Not released: no admin' });
  });
});

describe('TerminalDock', () => {
  const actions = {} as ShellActions;
  const render = (store: ReturnType<typeof createTerminalStore>, onNewShell?: () => void) =>
    renderToStaticMarkup(
      <TerminalDock
        store={store}
        actions={actions}
        stationName={(id) => `Station ${id}`}
        layout="dock"
        touch={false}
        onReturn={() => undefined}
        {...(onNewShell === undefined ? {} : { onNewShell })}
      />,
    );

  test('renders no bar without tabs, also when a new-shell action is given', () => {
    expect(render(createTerminalStore(null))).toBe('');
    expect(render(createTerminalStore(null), () => undefined)).toBe('');
  });

  test('the new-shell icon button shows with tabs only when an action is given', () => {
    const store = createTerminalStore(null);
    store.add(tab('a'));
    expect(render(store)).not.toContain('Open shell in workspace');
    expect(render(store, () => undefined)).toContain('aria-label="Open shell in workspace"');
  });

  test('a tui tab offers Return to board control and a Detach button', () => {
    const store = createTerminalStore(null);
    store.add(tab('a'));
    store.add(tab('b', 'tui', 'fix login'));
    const html = render(store);
    expect(html).toContain('aria-label="Terminals"');
    expect(html).toContain('role="tablist"');
    expect(html).toContain('aria-label="Detach a"');
    expect(html).toContain('aria-label="Detach fix login"');
    expect(html).toContain('Return to board control');
    expect(html).not.toContain('Close shell');
    expect(html).toContain('Station st');
    expect(html).toContain('Hide terminals');
    expect(html).toContain('aria-label="Resize terminals"');
  });

  test('a shell tab offers Close shell and no confirm until asked', () => {
    const store = createTerminalStore(null);
    store.add(tab('a'));
    const html = render(store);
    // Close shell is an item of the terminal menu
    expect(html).toContain('aria-label="Terminal menu"');
    expect(html).toContain('>Close shell</button>');
    expect(html).not.toContain('Return to board control');
    expect(html).not.toContain('Close the shell? Its process ends.');
    expect(html).toContain('lucide-square-terminal');
    expect(html).not.toContain('&gt;_');
  });

  test('a hidden dock keeps the terminals mounted and offers Show terminals', () => {
    const store = createTerminalStore(null);
    store.add(tab('a'));
    store.setOpen(false);
    const html = render(store);
    expect(html).toContain('Show terminals');
    expect(html).toContain('data-terminal="a"');
    expect(html).not.toContain('Resize terminals');
  });
});

describe('image paste note (DL-147)', () => {
  test('no note by default, a status while it works and an alert for an error', () => {
    expect(renderToStaticMarkup(<PasteNote note={null} />)).toBe('');
    const busy = renderToStaticMarkup(
      <PasteNote note={{ tone: 'busy', text: 'Pasting image…' }} />,
    );
    expect(busy).toContain('class="fd-term-note"');
    expect(busy).toContain('role="status"');
    expect(busy).toContain('Pasting image…');
    const error = renderToStaticMarkup(
      <PasteNote
        note={{ tone: 'error', text: 'Image not pasted: The image is too large to paste.' }}
      />,
    );
    expect(error).toContain('role="alert"');
    expect(error).toContain('data-tone="error"');
    expect(error).toContain('Image not pasted: The image is too large to paste.');
  });

  test('a terminal starts without a paste note', () => {
    const html = renderToStaticMarkup(
      <Terminal station="s" shellId="sh-1" actions={{} as ShellActions} active touch={false} />,
    );
    expect(html).toContain('fd-term-host');
    expect(html).not.toContain('fd-term-note');
  });
});

/** The button elements of a rendered tree, found by their text. */
function buttons(
  node: ReactNode,
  found: Record<string, () => void> = {},
): Record<string, () => void> {
  if (node === null || typeof node !== 'object' || !('props' in node)) return found;
  const el = node as ReactElement<{ children?: ReactNode; onClick?: () => void }>;
  if (el.type === 'button' && el.props.onClick) {
    found[String(el.props.children)] = el.props.onClick;
  }
  for (const child of [el.props.children].flat()) buttons(child, found);
  return found;
}

describe('terminal copy ask (NV-06)', () => {
  const nav = globalThis as { navigator?: unknown };
  const savedNavigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
  afterEach(() => {
    if (savedNavigator) Object.defineProperty(globalThis, 'navigator', savedNavigator);
    else nav.navigator = undefined;
  });
  function stubClipboard(): string[] {
    const calls: string[] = [];
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: {
        clipboard: {
          writeText: (text: string) => {
            calls.push(text);
            return Promise.resolve();
          },
        },
      },
    });
    return calls;
  }

  test('no bar without a request', () => {
    expect(renderToStaticMarkup(<CopyAsk text={null} onDone={() => undefined} />)).toBe('');
  });

  test('the bar shows the count and the preview and does not write the clipboard', () => {
    const calls = stubClipboard();
    const html = renderToStaticMarkup(<CopyAsk text={'rm -rf /\nx'} onDone={() => undefined} />);
    expect(html).toContain('role="status"');
    expect(html).toContain('aria-live="polite"');
    expect(html).toContain('asks to copy 10');
    expect(html).toContain('characters to your clipboard.');
    expect(html).toContain('rm -rf / x');
    expect(html).toContain('>Copy<');
    expect(html).toContain('>Ignore<');
    expect(calls).toEqual([]);
  });

  test('the count uses the locale format', () => {
    const text = 'a'.repeat(12_345);
    const html = renderToStaticMarkup(<CopyAsk text={text} onDone={() => undefined} />);
    expect(html).toContain(`asks to copy ${(12_345).toLocaleString()}`);
  });

  test('the preview cuts at 120 characters and replaces control characters', () => {
    expect(copyPreview('a\tb\u0000c\u007fd')).toBe('a b c d');
    expect(copyPreview('x'.repeat(120))).toBe('x'.repeat(120));
    expect(copyPreview('x'.repeat(121))).toBe(`${'x'.repeat(120)}…`);
  });

  test('Copy writes the clipboard once with the text and hides the bar', () => {
    const calls = stubClipboard();
    let done = 0;
    const tree = CopyAsk({ text: 'hello', onDone: () => (done += 1) });
    buttons(tree)['Copy']?.();
    expect(calls).toEqual(['hello']);
    expect(done).toBe(1);
  });

  test('Ignore hides the bar and does not write the clipboard', () => {
    const calls = stubClipboard();
    let done = 0;
    const tree = CopyAsk({ text: 'hello', onDone: () => (done += 1) });
    buttons(tree)['Ignore']?.();
    expect(calls).toEqual([]);
    expect(done).toBe(1);
  });

  test('a terminal starts without a copy bar', () => {
    const html = renderToStaticMarkup(
      <Terminal station="s" shellId="sh-1" actions={{} as ShellActions} active touch={false} />,
    );
    expect(html).not.toContain('fd-term-copy');
  });
});

describe('terminal palette', () => {
  const tokens: Record<string, string> = {
    '--code': '#0a0d13',
    '--text': '#e7ecf5',
    '--act': '#f0a63c',
    '--bg': '#0b0e14',
    '--hazard': '#ff6b54',
    '--ok': '#57b98a',
    '--focus': '#8fbcf7',
    '--fuel': '#c4b0ff',
    '--term-cyan': '#6fd6cd',
    '--faint': '#5c6880',
  };
  const stub = (values: Record<string, string>) => ({
    getPropertyValue: (name: string) => (values[name] === undefined ? '' : ` ${values[name]} `),
  });

  test('gives the 16 ANSI colors, the chrome and the fixed bright values', () => {
    const t = termTheme(stub(tokens));
    const ansi = [
      'black',
      'red',
      'green',
      'yellow',
      'blue',
      'magenta',
      'cyan',
      'white',
      'brightBlack',
      'brightRed',
      'brightGreen',
      'brightYellow',
      'brightBlue',
      'brightMagenta',
      'brightCyan',
      'brightWhite',
    ];
    for (const key of ansi) expect(t[key as keyof typeof t]).toMatch(/^#[0-9a-fA-F]{6}$/);
    expect(t.background).toBe('#0a0d13');
    expect(t.cursorAccent).toBe('#0a0d13');
    expect(t.foreground).toBe('#e7ecf5');
    expect(t.cursor).toBe('#e7ecf5');
    expect(t.yellow).toBe('#f0a63c');
    expect(t.red).toBe('#ff6b54');
    expect(t.blue).toBe('#8fbcf7');
    expect(t.magenta).toBe('#c4b0ff');
    expect(t.cyan).toBe('#6fd6cd');
    expect(t.brightBlack).toBe('#5c6880');
    expect(t.brightRed).toBe('#FF8A76');
    expect(t.brightGreen).toBe('#7BD3A6');
    expect(t.brightYellow).toBe('#F6BE6A');
    expect(t.brightBlue).toBe('#AECFFA');
    expect(t.brightMagenta).toBe('#D8CBFF');
    expect(t.brightCyan).toBe('#9AE4DD');
    expect(t.brightWhite).toBe('#FFFFFF');
  });

  test('the selection is --term-selection, else --focus with a 44 alpha', () => {
    expect(termTheme(stub(tokens)).selectionBackground).toBe('#8fbcf744');
    const set = termTheme(stub({ ...tokens, '--term-selection': 'rgba(240, 166, 60, 0.3)' }));
    expect(set.selectionBackground).toBe('rgba(240, 166, 60, 0.3)');
  });

  test('a missing token leaves the key out', () => {
    expect(termTheme(stub({})).background).toBeUndefined();
  });
});
