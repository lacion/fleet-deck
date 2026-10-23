// R13 P2c, P3c: the owner's side: the Displays page, "Send to wall" and the Desktop panel.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { formatHash } from '../app/routes';
import { DesktopPanel } from '../desktop/DesktopPanel';
import { isWebUrl } from '../desktop/DesktopForms';
import type { DesktopInfo, DisplayView, PendingPairing, StationView } from '../gen';
import { SettingsProvider, type SettingsEnv } from '../settings/context';
import type { SettingsActions } from '../settings/actions';
import { DeckError, type DeckIo } from '../settings/http';
import { station } from '../testing/fleet';
import { directiveFor, SendToWall, SendToWallDialog } from '../wall/SendToWall';
import {
  displayActions,
  desktopActions,
  noDisplays,
  NO_DISPLAYS_TEXT,
  type DisplayActions,
} from './actions';
import { AddDisplay, COST_TEXT } from './AddDisplay';
import { DisplayRow, changeOf, isDirty, QUIET_PROBLEM, seenText } from './DisplayRow';
import { ADD_TEXT, DisplaysTab, DisplaysView } from './DisplaysTab';
import { ELEVATION_LABELS, withElevation } from './elevate';
import { PendingPairings } from './PendingPairings';

type Call = { method: 'GET' | 'POST'; path: string; body?: unknown };

function fakeIo(answers: Record<string, unknown> = {}) {
  const calls: Call[] = [];
  const io: DeckIo = {
    get: async (path) => {
      calls.push({ method: 'GET', path });
      return answers[path] ?? null;
    },
    post: async (path, body) => {
      calls.push({ method: 'POST', path, body });
      return answers[path] ?? null;
    },
  };
  return { io, calls };
}

const display = (extra: Partial<DisplayView> = {}): DisplayView => ({
  display_id: 'D'.repeat(26),
  name: 'Wall',
  ceiling: ['station-a'],
  show_diffs: false,
  wake_on_needs_you: true,
  quiet_hours: null,
  binding: null,
  created_at: 1_000,
  last_seen: null,
  ...extra,
});

const pairing = (extra: Partial<PendingPairing> = {}): PendingPairing => ({
  pairing_id: 'P1',
  code: '482913',
  source: '10.0.0.7',
  user_agent: 'Chromium/140',
  created_at: 1_000,
  expires_at: 301_000,
  ...extra,
});

const stationsList: StationView[] = [station('station-a'), station('station-b')];

describe('the display calls', () => {
  test('the list is validated and the approve body carries the code and the elevation', async () => {
    const { io, calls } = fakeIo({
      '/api/displays': { displays: [display()], pending: [pairing()] },
      '/api/displays/pair/P%201/approve': display(),
    });
    const actions = displayActions(io);
    const list = await actions.list();
    expect(list.displays).toHaveLength(1);
    await actions.approve('P 1', '123456', { display_id: 'X' }, 'el');
    expect(calls[1]).toEqual({
      method: 'POST',
      path: '/api/displays/pair/P%201/approve',
      body: { code: '123456', display_id: 'X', elevation: 'el' },
    });
    await actions.approve('P 1', '123456', { name: 'N', ceiling: ['a'] }, null);
    expect(calls[2]?.body).toEqual({ code: '123456', name: 'N', ceiling: ['a'] });
  });

  test('a bad list answer is an error', async () => {
    await expect(displayActions(fakeIo({ '/api/displays': {} }).io).list()).rejects.toThrow(
      'invalid display list',
    );
  });

  test('link, change, revoke and directive use the deck paths', async () => {
    const { io, calls } = fakeIo({
      '/api/displays/link': { url: 'https://x/display#display=t', expires_at: 5 },
      '/api/displays/D1/settings': display(),
      '/api/displays/D1/directive': { generation: 2, directive: { mode: 'mission' } },
    });
    const actions = displayActions(io);
    expect((await actions.makeLink('Wall', ['a'], 'el')).url).toContain('#display=');
    await actions.change('D1', { show_diffs: true, binding: null }, null);
    await actions.revoke('D1');
    const view = await actions.directive('D1', { mode: 'mission', pin: false, filter: null });
    expect(view.generation).toBe(2);
    expect(calls.map((c) => c.path)).toEqual([
      '/api/displays/link',
      '/api/displays/D1/settings',
      '/api/displays/D1/revoke',
      '/api/displays/D1/directive',
    ]);
    expect(calls[0]?.body).toEqual({ name: 'Wall', ceiling: ['a'], elevation: 'el' });
    expect(calls[1]?.body).toEqual({ show_diffs: true, binding: null });
  });

  test('the desktop calls use the station name', async () => {
    const { io, calls } = fakeIo({
      '/api/stations/Alpha%20One/desktop': { monitors: [] },
      '/api/stations/Alpha%20One/desktop/act': { address: null, info: { monitors: [] } },
    });
    const actions = desktopActions(io);
    await actions.info('Alpha One');
    await actions.act('Alpha One', { action: 'screen_lock' });
    expect(calls[1]?.body).toEqual({ action: 'screen_lock' });
    await expect(desktopActions(fakeIo({}).io).info('x')).rejects.toThrow('invalid desktop');
  });

  test('a 404 means a deck without displays', () => {
    expect(noDisplays(new DeckError(404, 'x', 'not_found', {}))).toBe(true);
    expect(noDisplays(new DeckError(500, 'x', null, {}))).toBe(false);
    expect(noDisplays(new Error('x'))).toBe(false);
  });
});

describe('the elevation retry', () => {
  const settings = (held: string | null) => {
    let forgot = 0;
    const actions = {
      elevation: () => held,
      forgetElevation: () => {
        forgot += 1;
        held = null;
      },
    } as unknown as SettingsActions;
    return { actions, forgot: () => forgot };
  };

  test('a call with the held elevation runs once', async () => {
    const s = settings('held');
    const seen: (string | null)[] = [];
    const out = await withElevation(
      s.actions,
      async () => 'never',
      async (e) => {
        seen.push(e);
        return 'ok';
      },
    );
    expect(out).toBe('ok');
    expect(seen).toEqual(['held']);
  });

  test('elevation_required asks once and sends the call again with the new token', async () => {
    const s = settings(null);
    const seen: (string | null)[] = [];
    const asked: (readonly string[] | undefined)[] = [];
    const out = await withElevation(
      s.actions,
      async (labels) => {
        asked.push(labels);
        return 'fresh';
      },
      async (e) => {
        seen.push(e);
        if (e === null) throw new DeckError(400, 'x', 'elevation_required', { keys: ['displays'] });
        return 'done';
      },
    );
    expect(out).toBe('done');
    expect(seen).toEqual([null, 'fresh']);
    expect(asked).toEqual([ELEVATION_LABELS]);
    expect(s.forgot()).toBe(1);
  });

  test('a cancelled dialog gives null and another error passes through', async () => {
    const s = settings(null);
    const need = async () => {
      throw new DeckError(400, 'x', 'elevation_required', {});
    };
    expect(await withElevation(s.actions, async () => null, need)).toBeNull();
    await expect(
      withElevation(
        s.actions,
        async () => 'x',
        async () => {
          throw new DeckError(409, 'no', 'display_bound', {});
        },
      ),
    ).rejects.toThrow('no');
  });
});

describe('the pending pairings', () => {
  const noop = async () => null;
  const view = (pending: PendingPairing[], displays: DisplayView[] = []) =>
    renderToStaticMarkup(
      <PendingPairings
        pending={pending}
        displays={displays}
        stations={stationsList}
        actions={{} as DisplayActions}
        settings={{} as SettingsActions}
        ask={noop}
        onDone={() => {}}
        now={61_000}
      />,
    );

  test('with no pairing the section is absent', () => {
    expect(view([])).toBe('');
  });

  test('a row shows the source and the agent, never the code', () => {
    const html = view([pairing({ user_agent: 'U'.repeat(300) })]);
    expect(html).toContain('10.0.0.7');
    expect(html).toContain(`${'U'.repeat(120)}…`);
    expect(html).not.toContain('482913');
    expect(html).toContain('Code on the wall screen');
    expect(html).toContain('inputMode="numeric"');
  });

  test('a managed launch is locked to its display', () => {
    const d = display();
    const html = view(
      [pairing({ expected_display_id: d.display_id })],
      [d, display({ display_id: 'E'.repeat(26), name: 'Other' })],
    );
    expect(html).toContain('This launch belongs to');
    expect(html).toContain('<strong>Wall</strong>');
    expect(html).toContain('disabled=""');
    expect(html).not.toContain('New display');
  });

  test('without a launch the owner picks a new or an existing display', () => {
    const html = view([pairing()], [display()]);
    expect(html).toContain('New display');
    expect(html).toContain('Existing display');
  });
});

describe('a display row', () => {
  const base = display({
    binding: { station_id: 'station-a', monitor: 'DP-1' },
    quiet_hours: '22:00-07:00',
  });
  const draft = (extra: Partial<Parameters<typeof changeOf>[1]> = {}) => ({
    name: 'Wall',
    ceiling: ['station-a'],
    showDiffs: false,
    wake: true,
    quiet: '22:00-07:00',
    bindStation: 'station-a',
    bindMonitor: 'DP-1',
    ...extra,
  });

  test('an unchanged draft sends nothing', () => {
    expect(changeOf(base, draft())).toEqual({ change: {} });
  });

  test('only the changed keys are sent', () => {
    expect(
      changeOf(
        base,
        draft({ name: ' New ', showDiffs: true, quiet: '', ceiling: ['station-a', 'station-b'] }),
      ),
    ).toEqual({
      change: {
        name: 'New',
        show_diffs: true,
        quiet_hours: null,
        ceiling: ['station-a', 'station-b'],
      },
    });
  });

  test('the binding is set, changed or cleared', () => {
    expect(changeOf(base, draft({ bindStation: '', bindMonitor: '' }))).toEqual({
      change: { binding: null },
    });
    expect(changeOf(base, draft({ bindMonitor: 'HDMI-A-1' }))).toEqual({
      change: { binding: { station_id: 'station-a', monitor: 'HDMI-A-1' } },
    });
    expect(changeOf(display(), draft({ bindStation: '', bindMonitor: '', quiet: '' }))).toEqual({
      change: {},
    });
  });

  test('a bad draft is a problem and not a call', () => {
    expect(changeOf(base, draft({ name: ' ' }))).toEqual({ problem: 'Give a name.' });
    expect(changeOf(base, draft({ ceiling: [] }))).toEqual({
      problem: 'Choose at least one station.',
    });
    // A time field that the owner cleared leaves one side of the quiet hours empty.
    expect(changeOf(base, draft({ quiet: '-07:00' }))).toEqual({ problem: QUIET_PROBLEM });
    expect(changeOf(base, draft({ quiet: '10pm' }))).toEqual({ problem: QUIET_PROBLEM });
    expect(changeOf(base, draft({ bindMonitor: 'bad name' }))).toEqual({
      problem: 'Give a monitor name.',
    });
  });

  test('a draft is dirty when it has a change or a problem', () => {
    expect(isDirty(base, draft())).toBe(false);
    expect(isDirty(base, draft({ name: 'Wall ' }))).toBe(false);
    expect(isDirty(base, draft({ ceiling: ['station-a'] }))).toBe(false);
    expect(isDirty(base, draft({ wake: false }))).toBe(true);
    expect(isDirty(base, draft({ name: '' }))).toBe(true);
    expect(isDirty(base, draft({ bindMonitor: '' }))).toBe(true);
  });

  const row = (d: DisplayView, stations: StationView[] = stationsList) =>
    renderToStaticMarkup(
      <ul>
        <DisplayRow
          display={d}
          stations={stations}
          actions={{} as DisplayActions}
          settings={{} as SettingsActions}
          ask={async () => null}
          onDone={() => {}}
          now={3 * 86_400_000 + 1_000}
        />
      </ul>,
    );

  test('the row shows its controls, the actions in its menu and no save bar at rest', () => {
    const html = row(base);
    for (const text of [
      'Show code changes',
      'The wall shows the changed lines of a session, not only its changed files.',
      'Wake the screen',
      'Turn on the bound monitor when a session needs you, except in quiet hours.',
      'The screen stays dark in these hours, in the local time of the bound station.',
      'Bound monitor',
      'aria-label="Display actions"',
      'Switch to Mission view',
      'Revoke…',
      'Never connected',
      'title="Added 3 d ago"',
      'Stations it shows',
    ]) {
      expect(html).toContain(text);
    }
    // Every field is a settings field row (§4.7), the station names are wide type (§2.3).
    expect(html.match(/class="fd-fieldwrap" data-layout="row"/g)?.length).toBe(7);
    expect(html).toContain('<span class="fd-wide">Alpha</span>');
    expect(html).not.toContain('<fieldset');
    // One control clears the binding: the "None" station.
    expect(html).not.toContain('Clear binding');
    expect(html).not.toContain('placeholder="Off"');
    expect(html).not.toContain('(patches)');
    expect(html).not.toContain('Discard');
    expect(html).not.toContain('>Save<');
    expect(html).not.toContain('Revoke this display?');
  });

  test('the_row_menu_opens_the_remote', () => {
    const html = row(base);
    // "Remote control" is the first item of the menu of the row, before "Switch to Mission view".
    const menu = html.slice(html.indexOf('aria-label="Display actions"'));
    expect(menu).toContain('Remote control');
    expect(menu.indexOf('Remote control')).toBeLessThan(menu.indexOf('Switch to Mission view'));
    expect(menu.indexOf('Remote control')).toBeLessThan(menu.indexOf('Revoke…'));
    // It opens the route of the display.
    expect(formatHash({ name: 'remote', id: base.display_id })).toBe(`#/remote/${base.display_id}`);
  });

  test('quiet hours are a switch and two times that read the stored text', () => {
    const html = row(base);
    expect(html).toContain('role="group" aria-label="Quiet hours"');
    expect(html).toContain('From<input type="time" step="60" value="22:00"/>');
    expect(html).toContain('to<input type="time" step="60" value="07:00"/>');
    const off = row(display({ binding: base.binding }));
    expect(off).toContain('Quiet hours');
    expect(off).not.toContain('type="time"');
  });

  test('the link state is a glyph and words: online, the last time, or never', () => {
    const now = 3 * 86_400_000 + 1_000;
    expect(seenText(null, now)).toBe('Never connected');
    expect(seenText(now - 60_000, now)).toBe('Online');
    expect(seenText(now - 5 * 60_000, now)).toBe('Last seen 5 min ago');
    expect(seenText(2 * 86_400_000, now)).toBe('Last seen 1 d ago');
    expect(row(display({ last_seen: 3 * 86_400_000 }))).toContain(
      '<span class="fd-dot" data-status="online" aria-hidden="true"></span>Online',
    );
    expect(row(display({ last_seen: 2 * 86_400_000 }))).toContain(
      '<span class="fd-dot" data-status="idle" aria-hidden="true"></span>Last seen 1 d ago',
    );
  });

  test('an unbound display has no monitor row, and waking and quiet hours wait for a binding', () => {
    const html = row(display({ quiet_hours: '22:00-07:00' }));
    expect(html).toContain('Bound station');
    expect(html).not.toContain('Bound monitor');
    // A disabled switch says its stored value in words, and what to do first.
    expect(html.match(/On after you bind a station and a monitor\./g)).toHaveLength(2);
    expect(html).not.toContain('Turn on the bound monitor');
    expect(html).not.toContain('The screen stays dark in these hours');
    const off = row(display({ wake_on_needs_you: false }));
    expect(off.match(/Bind a station and a monitor first\./g)).toHaveLength(2);
    // The stored values stay: the switches keep their state, disabled.
    expect(html).toMatch(/aria-checked="true" disabled="" checked=""/);
    expect(html).toContain('<input type="time" step="60" disabled="" value="22:00"/>');
  });

  test('a station that reports monitors suggests them; a name not in the list says so', () => {
    const monitor = (name: string) => ({
      name,
      description: name === 'DP-1' ? 'Main' : '',
      width: 1920,
      height: 1080,
      scale_pct: 100,
      refresh_hz: 60,
      focused: false,
      dpms_on: true,
    });
    const withMonitors = (names: string[]): StationView =>
      station('station-a', {
        info: { desktop: { monitors: names.map(monitor) } } as unknown as StationView['info'],
      });
    const listed = row(base, [withMonitors(['DP-1', 'HDMI-A-1'])]);
    // A name to type, so that a monitor that is off now can be bound; the reported monitors are
    // suggestions: the words first, the connector last; the title keeps the raw text.
    expect(listed).toMatch(/<input list="[^"]+"[^>]*value="DP-1"\/>/);
    expect(listed).toContain('<option value="DP-1" label="Main · DP-1" title="DP-1 — Main">');
    expect(listed).toContain('<option value="HDMI-A-1" label="HDMI-A-1" title="HDMI-A-1">');
    // The one select is the Bound station.
    expect(listed.match(/<select/g)).toHaveLength(1);
    expect(listed).not.toContain('is not connected now.');
    const gone = row(base, [withMonitors(['HDMI-A-1'])]);
    expect(gone).toContain('DP-1 is not connected now.');
    // A station without monitors: a plain name field.
    expect(row(base)).not.toContain('<datalist');
  });
});

describe('the Displays page', () => {
  const env = {
    settings: {} as SettingsActions,
    accounts: {} as never,
    tailnet: {} as never,
    displays: {} as DisplayActions,
    desktop: {} as never,
    capture: {} as never,
    deletion: {} as never,
  } as SettingsEnv;

  const page = (displays: DisplayView[], pending: PendingPairing[] = []) =>
    renderToStaticMarkup(
      <DisplaysView
        state={{ kind: 'ready', list: { displays, pending } }}
        refresh={() => {}}
        env={env}
        stations={stationsList}
        ask={async () => null}
        now={61_000}
      />,
    );

  test('the first render has the add button, not the form, and waits for the list', () => {
    const html = renderToStaticMarkup(<DisplaysTab env={env} stations={stationsList} live />);
    expect(html).toContain(`${ADD_TEXT}</button>`);
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain(COST_TEXT);
    expect(html).toContain('<p class="fd-note fd-set-loading" role="status">Loading displays…</p>');
  });

  test('the displays come first, then the pairings that wait', () => {
    const html = page([display()], [pairing()]);
    expect(html.indexOf('Wall displays')).toBeLessThan(html.indexOf('Waiting for a code'));
    expect(html).toContain(`${ADD_TEXT}</button>`);
    expect(page([display()])).not.toContain('Waiting for a code');
  });

  test('with no display the empty state has the one add button', () => {
    const html = page([]);
    expect(html).toContain('No wall display yet');
    expect(html).toContain('Add one to show the fleet on a screen.');
    expect(html.split(`${ADD_TEXT}</button>`)).toHaveLength(2);
  });

  test('the add panel has the cost text, Cancel and the pairing link button', () => {
    const html = renderToStaticMarkup(
      <AddDisplay
        actions={{} as DisplayActions}
        settings={{} as SettingsActions}
        ask={async () => null}
        stations={stationsList}
        onDone={() => {}}
        onClose={() => {}}
      />,
    );
    expect(html).toContain(COST_TEXT);
    expect(html).toContain('Stations it shows');
    expect(html).toContain('class="fd-primary"');
    expect(html).toContain('Alpha');
    expect(html).toContain('Beta');
    expect(html).toContain('Cancel');
    expect(html).toContain('Make a pairing link');
    expect(html).not.toMatch(/checked=""/);
  });

  test('the text of a deck without displays', () => {
    expect(NO_DISPLAYS_TEXT).toBe('This deck has no wall displays');
  });
});

describe('send to wall', () => {
  test('a session becomes a focus or review directive, pinned or not', () => {
    const target = { kind: 'session' as const, station_id: 'st', session_id: 'se' };
    expect(directiveFor(target, 'focus', true)).toEqual({
      mode: 'focus',
      session: { station_id: 'st', session_id: 'se' },
      pin: true,
      filter: null,
    });
    expect(directiveFor(target, 'review', false).mode).toBe('review');
  });

  test('an artifact becomes an artifact directive with its version', () => {
    expect(
      directiveFor(
        { kind: 'artifact', station_id: 'st', artifact_id: 'ar', version: 4 },
        'focus',
        true,
      ),
    ).toEqual({
      mode: 'artifact',
      artifact: { station_id: 'st', artifact_id: 'ar', version: 4 },
      pin: true,
      filter: null,
    });
  });

  test('the dialog offers the modes for a session and the version for an artifact', () => {
    const actions = {} as DisplayActions;
    const session = renderToStaticMarkup(
      <SendToWallDialog
        target={{ kind: 'session', station_id: 'st', session_id: 'se' }}
        actions={actions}
        onClose={() => {}}
      />,
    );
    expect(session).toContain('Focus');
    expect(session).toContain('Review');
    expect(session).toContain('Keep');
    expect(session).toContain('checked=""');
    const artifact = renderToStaticMarkup(
      <SendToWallDialog
        target={{ kind: 'artifact', station_id: 'st', artifact_id: 'ar', version: 4 }}
        actions={actions}
        onClose={() => {}}
      />,
    );
    expect(artifact).toContain('The wall shows version 4 of this artifact.');
    expect(artifact).not.toContain('>Review<');
  });

  test('the control is absent without the owner settings and present with them', () => {
    const target = { kind: 'session' as const, station_id: 'st', session_id: 'se' };
    expect(renderToStaticMarkup(<SendToWall target={target} />)).toBe('');
    const env = {
      settings: {} as SettingsActions,
      accounts: {} as never,
      tailnet: {} as never,
      displays: {} as DisplayActions,
      desktop: {} as never,
      capture: {} as never,
      deletion: {} as never,
    } as SettingsEnv;
    expect(
      renderToStaticMarkup(
        <SettingsProvider env={env}>
          <SendToWall target={target} />
        </SettingsProvider>,
      ),
    ).toContain('Send to wall…');
  });
});

describe('the Desktop panel', () => {
  const info: DesktopInfo = {
    kind: 'omarchy',
    omarchy_version: '3.1',
    monitors: [
      {
        name: 'DP-1',
        description: 'Main',
        width: 3440,
        height: 1440,
        scale_pct: 100,
        refresh_hz: 100,
        focused: true,
        dpms_on: false,
      },
    ],
    locked: false,
    dnd: true,
    idle_inhibited: null,
    walls: [{ display_id: 'D1', monitor: 'DP-1', address: '0xabc' }],
    allowed: { desktop: true, capture: false, capture_monitors: [], input: false },
    tools: {
      wtype: true,
      pointer: false,
      recorder: false,
      grim: true,
      chromium: true,
      notify_send: true,
    },
    features: [],
  };
  const env = (acts: Partial<SettingsEnv> = {}) =>
    ({
      settings: {} as SettingsActions,
      accounts: {} as never,
      tailnet: {} as never,
      displays: {} as DisplayActions,
      desktop: {} as never,
      capture: {} as never,
      deletion: {} as never,
      ...acts,
    }) as SettingsEnv;
  const withInfo = (desktop: DesktopInfo | undefined): StationView =>
    station('station-a', {
      info: {
        version: null,
        providers: [],
        desktop,
      } as unknown as StationView['info'],
    });
  const render = (view: StationView, e: SettingsEnv | null = env()) => {
    const panel = <DesktopPanel station={view} />;
    return renderToStaticMarkup(
      e === null ? panel : <SettingsProvider env={e}>{panel}</SettingsProvider>,
    );
  };

  test('it shows what the station reports', () => {
    const html = render(withInfo(info));
    expect(html).toContain('Omarchy 3.1');
    expect(html).toContain('DP-1');
    expect(html).toContain('3440×1440');
    expect(html).toContain('Screen off');
    expect(html).toContain('allowed on the station');
    expect(html).toContain('Wall D1 on DP-1');
    expect(html).toContain('Screen on');
    expect(html).toContain('Lock the screen');
    expect(html).toContain('Do not disturb');
    expect(html).toContain('Open the wall');
  });

  test('each open wall has a Remote button before Reload', () => {
    const html = render(withInfo(info));
    const wall = html.slice(html.indexOf('Wall D1 on DP-1'));
    expect(wall).toContain('>Remote</button>');
    expect(wall.indexOf('>Remote</button>')).toBeLessThan(wall.indexOf('>Reload</button>'));
  });

  test('it is absent without the owner settings', () => {
    expect(render(withInfo(info), null)).toBe('');
  });

  test('a station with info and no desktop says so; one without info says nothing', () => {
    expect(render(withInfo(undefined))).toContain('This station reports no desktop.');
    expect(render(station('station-a'))).toBe('');
  });

  test('a link must be http or https and at most 2048 bytes', () => {
    expect(isWebUrl('https://example.com/x')).toBe(true);
    expect(isWebUrl('http://127.0.0.1:3000')).toBe(true);
    expect(isWebUrl('file:///etc/passwd')).toBe(false);
    expect(isWebUrl('javascript:alert(1)')).toBe(false);
    expect(isWebUrl('not a url')).toBe(false);
    expect(isWebUrl(`https://e.com/${'a'.repeat(2100)}`)).toBe(false);
  });
});
