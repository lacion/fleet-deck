// §4.5: the Properties panel, its header toggle, the changed files it lists, and the composer's
// model chip that opens the model picker of the session menu.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession } from '../gen';
import { station } from '../testing/fleet';
import { summary } from '../testing/summary';
import { claudeCaps } from '../testing/capabilities';
import { sessionActions } from './commands';
import { Composer } from './Composer';
import {
  ChangedList,
  compactCount,
  PROPS_FILES_MAX,
  scopeLabel,
  usePropsOpen,
} from './PropertiesPanel';
import { openPicker } from './SessionSettings';
import { SessionView } from './SessionView';

const key = { station_id: 'station-a', session_id: 's1' };

function session(): DeckSession {
  return {
    station_id: 'station-a',
    status: 'working',
    summary: summary('s1', 'working', 1, {
      title: 'Fix login',
      capabilities: claudeCaps(),
      models: [{ value: 'claude-sonnet-5-5', label: 'Sonnet 5.5', description: null }],
      model: 'claude-sonnet-5-5',
      current_run: 'run-1',
    }),
  };
}

const view = () =>
  renderToStaticMarkup(
    <SessionView
      session={session()}
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
    expect(compactCount(9400)).toBe('9.4 k');
    expect(compactCount(182000)).toBe('182 k');
    expect(compactCount(1250000)).toBe('1.3 M');
  });

  test('scopeLabel names the window with its provider', () => {
    expect(scopeLabel('five_hour', 'Claude')).toBe('Claude 5-hour window');
    expect(scopeLabel('weekly', null)).toBe('Weekly window');
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

  test('up to five files as buttons with the path, then "+N more"', () => {
    const paths = Array.from({ length: 7 }, (_, i) => `crates/shim/src/f${i}.rs`);
    const html = list(paths);
    expect(html.match(/<button /g)?.length).toBe(PROPS_FILES_MAX);
    expect(html).toContain('title="crates/shim/src/f0.rs"');
    expect(html).toContain('<bdi>crates/shim/src/f4.rs</bdi>');
    expect(html).not.toContain('f5.rs');
    expect(html).toContain('<li class="fd-props-more">+2 more</li>');
    expect(list(paths.slice(0, 5))).not.toContain('more');
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
