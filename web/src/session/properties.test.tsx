// §4.5: the Properties panel, its header toggle, the changed files it lists, and the composer's
// model chip that opens the model picker of the session menu.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession } from '../gen';
import { station } from '../testing/fleet';
import { summary } from '../testing/summary';
import { claudeCaps } from '../testing/capabilities';
import { compactCount } from '../ui/format';
import { sessionActions } from './commands';
import { Composer } from './Composer';
import {
  ChangedList,
  changeTotal,
  PROPS_FILES_MAX,
  PropertiesPanel,
  scopeLabel,
  sinceText,
  usePropsOpen,
} from './PropertiesPanel';
import type { FinalizedTurn } from './workspace';
import { openPicker } from './SessionSettings';
import { SessionView } from './SessionView';

const key = { station_id: 'station-a', session_id: 's1' };

function session(held = false): DeckSession {
  return {
    station_id: 'station-a',
    status: 'working',
    summary: summary('s1', 'working', 1, {
      title: 'Fix login',
      capabilities: claudeCaps(),
      models: [{ value: 'claude-sonnet-5-5', label: 'Sonnet 5.5', description: null }],
      model: 'claude-sonnet-5-5',
      current_run: 'run-1',
      ...(held ? { terminal: { shell_id: 'tui-7', pid: 42, since: 1, reason: null } } : {}),
    }),
  };
}

const view = (held = false) =>
  renderToStaticMarkup(
    <SessionView
      session={session(held)}
      stations={[station()]}
      freshness="live"
      timeline={{
        incarnation: 'inc',
        cursor: 0,
        events: [],
        freshness: 'live',
        error: null,
        ready: true,
      }}
      actions={sessionActions(async () => ({}), key, ['read', 'drive', 'approve'])}
      layout="desktop"
      tab="chat"
    />,
  );

/** The toggle of the properties panel, read through its hook (the header button calls it). */
function propsToggle(): () => void {
  let toggle: () => void = () => {};
  function Probe() {
    toggle = usePropsOpen()[1];
    return null;
  }
  renderToStaticMarkup(<Probe />);
  return toggle;
}

describe('properties panel numbers (§4.5)', () => {
  test('compactCount shortens token counts', () => {
    expect(compactCount(950)).toBe('950');
    expect(compactCount(9400)).toBe('9.4K');
    expect(compactCount(182000)).toBe('182K');
    expect(compactCount(1250000)).toBe('1.3M');
  });

  test('scopeLabel names the window with its provider', () => {
    expect(scopeLabel('five_hour', 'Claude')).toBe('Claude 5-hour window');
    expect(scopeLabel('weekly', null)).toBe('Weekly window');
  });
});

describe('properties panel facts (§4.5)', () => {
  const panel = (
    freshness: 'live' | 'stale',
    reachable = true,
    extra: Partial<Parameters<typeof PropertiesPanel>[0]> = {},
  ) =>
    renderToStaticMarkup(
      <PropertiesPanel
        session={session()}
        stations={[station('station-a', { reachable })]}
        turns={[]}
        total={null}
        agents={0}
        runningAgents={0}
        onOpenChange={() => {}}
        freshness={freshness}
        {...extra}
      />,
    );

  test('the status says since when: "since 2 min", or "just now"', () => {
    expect(sinceText('2 min')).toBe('since 2 min');
    expect(sinceText('3 h')).toBe('since 3 h');
    expect(sinceText('now')).toBe('just now');
    expect(panel('live')).toMatch(
      /<span class="fd-props-aside">(since \d+ (min|h|d)|just now)<\/span>/,
    );
  });

  test('a live board says that a reachable station is online', () => {
    const html = panel('live');
    expect(html).toContain('data-up="true"');
    expect(html).toContain('data-status="online"');
    expect(html).toContain('online</span>');
  });

  test('a stale board does not say that the station is online (the same as the fleet lane)', () => {
    const html = panel('stale');
    expect(html).toContain('data-up="false"');
    expect(html).toContain('data-status="unreachable"');
    expect(html).toContain('offline</span>');
    expect(html).not.toContain('online</span>');
    expect(panel('live', false)).toContain('offline</span>');
  });

  test('the Changes row: absent while unknown, "No file changes" only for an empty diff', () => {
    expect(panel('live')).not.toContain('<dt>Changes</dt>');
    const none = panel('live', true, { total: { files_changed: 0, insertions: 0, deletions: 0 } });
    // The value starts at the left of the column, as every other value.
    expect(none).toContain('<dt>Changes</dt><dd><span class="fd-props-aside">No file changes');
    const some = panel('live', true, { total: { files_changed: 3, insertions: 9, deletions: 0 } });
    expect(some).toContain('<span class="fd-add">+9</span><span class="fd-props-aside">3 files');
    expect(some).not.toContain('−0');
  });

  test('the Agents row counts the Agents bar: its agents and how many run', () => {
    expect(panel('live')).not.toContain('<dt>Agents</dt>');
    const html = panel('live', true, { agents: 2, runningAgents: 1 });
    expect(html).toContain('<dt>Agents</dt><dd>2<span class="fd-props-aside">1 running</span>');
    expect(html).not.toContain('Subagents');
    // Only a background command runs: the bar's number, with no count of subagents.
    expect(panel('live', true, { agents: 0, runningAgents: 1 })).toContain(
      '<dt>Agents</dt><dd><span class="fd-props-aside">1 running</span></dd>',
    );
  });

  test('the fleet-control fact is a row of the panel when given', () => {
    expect(panel('live')).not.toContain('Fleet control');
    const html = panel('live', true, { fleet: <button type="button">0 answers</button> });
    expect(html).toContain('<dt>Fleet control</dt><dd><button type="button">0 answers</button>');
  });

  test('the folder breaks only after a slash, never inside a name', () => {
    const html = panel('live', true, {
      session: { ...session(), summary: { ...session().summary, cwd: '/srv/fleet-deck/v2' } },
    });
    expect(html).toContain(
      '<span class="fd-props-seg">/</span><wbr/><span class="fd-props-seg">srv/</span><wbr/>' +
        '<span class="fd-props-seg">fleet-deck/</span><wbr/><span class="fd-props-seg">v2</span>',
    );
  });
});

describe('the size of the session changes (§4.5)', () => {
  const turn = (files: number): FinalizedTurn => ({
    turnId: `t${files}`,
    start: null,
    end: null,
    diff: { files_changed: files, insertions: files, deletions: 0 },
    reverted: false,
  });
  const loaded = { files_changed: 3, insertions: 1286, deletions: 400 };

  test('the loaded session diff wins; while it loads the size is not known', () => {
    expect(changeTotal(loaded, [turn(0)], true)).toEqual(loaded);
    expect(changeTotal(loaded, [turn(0)], false)).toEqual(loaded);
    // A shared folder: every turn diff is empty, the session diff is not. No "No file changes".
    expect(changeTotal(undefined, [turn(0)], true)).toBeNull();
  });

  test('without a session diff, the sum of the kept turns', () => {
    expect(changeTotal(undefined, [turn(1), turn(2)], false)).toEqual({
      files_changed: 3,
      insertions: 3,
      deletions: 0,
    });
    expect(changeTotal(undefined, [], false)).toBeNull();
  });
});

describe('properties panel toggle (§4.5)', () => {
  test('a wide desktop shows the pressed toggle and the panel; the toggle hides the panel', () => {
    const toggle = /<button[^>]*aria-label="Properties"[^>]*aria-pressed="(true|false)"/;
    const panel = '<aside class="fd-props" aria-label="Properties">';
    const open = view();
    expect(toggle.exec(open)?.[1]).toBe('true');
    expect(open).toContain(panel);
    const press = propsToggle();
    press();
    try {
      const closed = view();
      expect(toggle.exec(closed)?.[1]).toBe('false');
      expect(closed).not.toContain(panel);
    } finally {
      press();
    }
    expect(view()).toContain(panel);
  });
});

describe('properties panel changed files (§4.5)', () => {
  const list = (paths: readonly string[] | undefined) =>
    renderToStaticMarkup(<ChangedList paths={paths} onOpen={() => {}} />);

  test('no loaded paths: no list', () => {
    expect(list(undefined)).toBe('');
    expect(list([])).toBe('');
  });

  test('up to five files as buttons with the path of a diff header, then "+N more files"', () => {
    const paths = Array.from({ length: 7 }, (_, i) => `crates/shim/src/f${i}.rs`);
    const html = list(paths);
    expect(html.match(/<button /g)?.length).toBe(PROPS_FILES_MAX);
    expect(html).toContain('title="crates/shim/src/f0.rs"');
    // The folders and the name are drawn apart; a screen reader reads the path as one text.
    expect(html).toContain(
      '<code class="fd-props-file fd-dl-path"><span class="fd-dl-dir" aria-hidden="true"><bdi>' +
        'crates/shim/src/</bdi></span><span class="fd-dl-base" aria-hidden="true">f4.rs</span>' +
        '<span class="fd-vh">crates/shim/src/f4.rs</span></code>',
    );
    expect(html).not.toContain('f5.rs');
    expect(html).toContain('<li class="fd-props-more">+2 more files</li>');
    expect(list(paths.slice(0, 5))).not.toContain('more');
  });

  test('"+N more files" counts the whole diff and opens the Changes tab', () => {
    const paths = Array.from({ length: 17 }, (_, i) => `src/f${i}.ts`);
    const html = renderToStaticMarkup(
      <ChangedList paths={paths} files={121} onOpen={() => {}} onOpenAll={() => {}} />,
    );
    expect(html).toContain(
      '<li class="fd-props-more"><button type="button" class="fd-link fd-props-link">' +
        '+116 more files</button></li>',
    );
    // A file at the root has no folder part.
    const root = renderToStaticMarkup(<ChangedList paths={['README.md']} onOpen={() => {}} />);
    expect(root).toContain(
      '<code class="fd-props-file fd-dl-path"><span class="fd-dl-base" aria-hidden="true">',
    );
    expect(root).not.toContain('more');
  });

  test('a file that no diff opens is plain text, not a button', () => {
    const html = renderToStaticMarkup(
      <ChangedList
        paths={['src/open.ts', 'src/plain.ts']}
        onOpen={() => {}}
        opens={(p) => p === 'src/open.ts'}
      />,
    );
    expect(html.match(/<button /g)?.length).toBe(1);
    expect(html).toContain(
      '<code class="fd-props-file fd-dl-path" title="src/plain.ts"><span class="fd-dl-dir" aria-hidden="true">',
    );
    expect(html).toContain('title="src/open.ts"');
  });
});

describe('composer model chip (§4.5)', () => {
  test('the chip is a button named after the model', () => {
    const html = renderToStaticMarkup(
      <Composer
        enabled
        working={false}
        steer={false}
        nativeQueue={false}
        onSend={() => {}}
        model="Sonnet 5.5"
        onModel={() => {}}
      />,
    );
    expect(html).toContain('aria-label="Model: Sonnet 5.5"');
  });

  test('the chip is a button while the model can be changed', () => {
    const html = view();
    expect(html).toMatch(/<button[^>]*aria-label="Model: Sonnet 5\.5"/);
  });

  test('a held session (a terminal holds it) shows the model as plain text, not a button', () => {
    const html = view(true);
    expect(html).toContain('<span class="fd-composer-model">Sonnet 5.5</span>');
    expect(html).not.toContain('aria-label="Model: ');
  });

  test('the session menu holds the model select that the chip opens', () => {
    const html = view();
    expect(html).toMatch(/<summary[^>]*aria-label="Session menu"/);
    expect(html).toContain('class="fd-sm fd-model-select"');
  });

  test('openPicker opens the menu of the control and focuses the control', () => {
    const menu = { open: false };
    let focused = false;
    let frames = 0;
    const control = {
      closest: (selector: string) => (selector === 'details' ? menu : null),
      focus: () => {
        focused = true;
      },
    } as unknown as HTMLElement;
    const opened = openPicker(control, (run) => {
      frames += 1;
      run();
    });
    expect(opened).toBe(true);
    expect(menu.open).toBe(true);
    expect(frames).toBe(1);
    expect(focused).toBe(true);
  });

  test('openPicker does nothing without a control or outside a menu', () => {
    expect(openPicker(null, () => {})).toBe(false);
    const loose = { closest: () => null, focus: () => {} } as unknown as HTMLElement;
    expect(openPicker(loose, () => {})).toBe(false);
  });
});
