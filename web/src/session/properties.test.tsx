// §4.5: the Properties panel, its header toggle, the changed files it lists, and the composer's
// model chip with its own popover of models.
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
  NEW_INPUT_TITLE,
  PROPS_FILES_MAX,
  PropertiesPanel,
  scopeLabel,
  sinceText,
  tokensText,
  usePropsOpen,
} from './PropertiesPanel';
import type { FinalizedTurn } from './workspace';
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

const view = (held = false, layout: 'desktop' | 'phone' = 'desktop') =>
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
      layout={layout}
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

  test('the Agents row counts as the Agents bar: subagents, commands and how many run', () => {
    expect(panel('live')).not.toContain('<dt>Agents</dt>');
    const html = panel('live', true, { agents: 2, runningAgents: 1 });
    expect(html).toContain(
      '<dt>Agents</dt><dd><span>2</span><span class="fd-props-aside">1 running</span>',
    );
    expect(html).not.toContain('Subagents');
    // The commands follow the subagents in quiet words, as in the summary of the bar.
    expect(panel('live', true, { agents: 3, commands: 33, runningAgents: 5 })).toContain(
      '<dt>Agents</dt><dd><span>3 <span class="fd-props-aside">· 33 commands</span></span><span class="fd-props-aside">5 running</span></dd>',
    );
    // Commands only: the row names them ("Commands 34" in the bar).
    expect(panel('live', true, { agents: 0, commands: 34, runningAgents: 0 })).toContain(
      '<dt>Commands</dt><dd><span>34</span></dd>',
    );
    expect(panel('live', true, { agents: 0, commands: 1, runningAgents: 1 })).toContain(
      '<dt>Commands</dt><dd><span>1</span><span class="fd-props-aside">1 running</span></dd>',
    );
  });

  test('the status says since when on a line of its own (the CSS gives the aside a full line)', () => {
    expect(panel('live')).toMatch(
      /<dd><span class="fd-sh-status" data-status="working">[\s\S]*?<\/span><span class="fd-props-aside">/,
    );
  });

  test('the context window is a --faint aside of the Model row, not a row of its own', () => {
    const opus = { ...session(), summary: { ...session().summary, model: 'claude-opus-5-5[1m]' } };
    const html = panel('live', true, { session: opus });
    expect(html).toContain(
      '<dt>Model</dt><dd><span title="claude-opus-5-5[1m]">Opus 5.5</span><span class="fd-props-aside">1M context</span></dd>',
    );
    expect(html).not.toContain('<dt>Context</dt>');
    expect(panel('live')).not.toContain('context</span>');
  });

  test('the tokens say "new input": the station counts the input outside the prompt cache only', () => {
    expect(tokensText(682, 309_000)).toBe('682 new input · 309K output');
    expect(tokensText(null, 309_000)).toBe('309K output');
    expect(tokensText(1200, null)).toBe('1.2K new input');
    // A Codex session counts the cached input too (the ops glance says "input").
    expect(tokensText(1200, 5, 'input')).toBe('1.2K input · 5 output');
    const usage = { input_tokens: 682, output_tokens: 309_000, cost_usd: 1, cost_inferred: false };
    const html = panel('live', true, {
      session: { ...session(), summary: { ...session().summary, usage } },
    });
    expect(html).toContain(
      `<dt>Tokens</dt><dd><span class="fd-props-fuel" title="${NEW_INPUT_TITLE}">682 new input · 309K output</span>`,
    );
  });

  test('the session id (with Copy) and the event count close the panel', () => {
    const html = panel('live', true, {
      session: { ...session(), summary: { ...session().summary, session_seq: 7405 } },
    });
    expect(html).toContain(
      '<dt>Session</dt><dd><code class="fd-props-mono fd-props-sid" title="s1">s1</code><button type="button" class="fd-iconbtn fd-sm" aria-label="Copy the session id"',
    );
    expect(html).toContain('<dt>Events</dt><dd>7,405</dd>');
    expect(html).not.toContain('<dt>Summary</dt>');
    const digest = panel('live', true, {
      session: { ...session(), summary: { ...session().summary, digest: '- One\n- Two' } },
    });
    expect(digest).toContain('<dt>Summary</dt><dd><span class="fd-generated" role="img"');
    expect(digest).toContain(
      '<dd class="fd-props-filelist fd-props-digest"><ul class="fd-sh-digest"><li>One</li><li>Two</li></ul></dd>',
    );
  });

  test('the Mode row: a plain fact, or a ghost menu with the mode ladder when it can be set', () => {
    const planned = { ...session(), summary: { ...session().summary, mode: 'bypassPermissions' } };
    const plain = panel('live', true, { session: planned });
    expect(plain).toContain(
      '<span class="fd-props-mode" data-tone="hazard" title="Permission mode: bypassPermissions">',
    );
    expect(plain).not.toContain('fd-props-modemenu');
    const menu = panel('live', true, {
      session: planned,
      mode: { modes: ['default', 'plan', 'bypassPermissions'], enabled: true, onSet: () => {} },
    });
    expect(menu).toMatch(
      /<span class="fd-props-mode fd-props-modemenu" data-tone="hazard"><details class="fd-menu"><summary class="fd-ghost fd-sm" data-variant="toggle" aria-label="Mode: Bypass permissions" title="Change the permission mode">/,
    );
    expect(menu).toContain('<legend>Mode</legend>');
    expect(menu).toMatch(/title="bypassPermissions" aria-pressed="true" disabled=""/);
  });

  test('a folder never cuts its last name (the CSS wraps it); the earlier ones stay whole', () => {
    const cwd = '/home/me/projects/obsurvive/fleet-deck--fd-raven-b6df';
    const html = panel('live', true, {
      session: { ...session(), summary: { ...session().summary, cwd } },
    });
    expect(html).toContain('<span class="fd-props-seg">fleet-deck--fd-raven-b6df</span></code>');
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
  test('a failed read of the session diff falls back to the turns (pending only while it loads)', () => {
    // SessionView passes pending = has a base and files && the read is 'loading'; a failed read
    // is not pending, so the sum of the kept turns shows.
    const turns: FinalizedTurn[] = [
      {
        turnId: 't1',
        start: null,
        end: null,
        diff: { files_changed: 2, insertions: 5, deletions: 1 },
        reverted: false,
      },
    ];
    expect(changeTotal(undefined, turns, false)).toEqual({
      files_changed: 2,
      insertions: 5,
      deletions: 1,
    });
  });

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
  test('the chip is the summary of its own popover of models', () => {
    const html = renderToStaticMarkup(
      <Composer
        enabled
        working={false}
        steer={false}
        nativeQueue={false}
        onSend={() => {}}
        model="Sonnet 5.5"
        modelChoice={{
          current: 'claude-sonnet-5-5',
          models: [{ value: 'claude-sonnet-5-5', label: 'Sonnet 5.5', description: null }],
          onSet: () => {},
        }}
      />,
    );
    expect(html).toMatch(
      /<details class="fd-menu"><summary class="fd-sm fd-composer-model" aria-label="Model: Sonnet 5.5" title="Change the model">Sonnet 5.5<svg/,
    );
    // The popover lists the models; the current one has its check.
    expect(html).toContain('<legend>Model</legend>');
    expect(html).toMatch(/title="claude-sonnet-5-5" aria-pressed="true"/);
  });

  test('the chip opens its popover while the model can be changed', () => {
    const html = view();
    expect(html).toMatch(
      /<summary class="fd-sm fd-composer-model" aria-label="Model: Sonnet 5\.5"/,
    );
  });

  test('a held session (a terminal holds it) shows the model as plain text, not a menu', () => {
    const html = view(true);
    expect(html).toContain('<span class="fd-composer-model">Sonnet 5.5</span>');
    expect(html).not.toContain('aria-label="Model: ');
  });

  test('one model picker on a screen: the composer on the desktop, the session menu on the phone', () => {
    const desktop = view();
    expect(desktop.match(/<legend>Model<\/legend>/g)?.length).toBe(1);
    expect(desktop).not.toContain('fd-model-select');
    const phone = view(false, 'phone');
    expect(phone.match(/<legend>Model<\/legend>/g)?.length).toBe(1);
    expect(phone).not.toContain('aria-label="Model: ');
  });
});
