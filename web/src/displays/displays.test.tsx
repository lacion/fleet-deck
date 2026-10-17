// R13 P2c, P3c: the owner's side: the Displays page, "Send to wall" and the Desktop panel.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
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
import { DisplayRow, changeOf } from './DisplayRow';
import { DisplaysTab } from './DisplaysTab';
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

  test('an empty list says so', () => {
    expect(view([])).toContain('No display waits for a code.');
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
    expect(changeOf(base, draft({ quiet: '10pm' }))).toEqual({
      problem: 'Quiet hours look like 22:00-07:00.',
    });
    expect(changeOf(base, draft({ bindMonitor: 'bad name' }))).toEqual({
      problem: 'Give a monitor name.',
    });
  });

  test('the row shows its controls', () => {
    const html = renderToStaticMarkup(
      <ul>
        <DisplayRow
          display={base}
          stations={stationsList}
          actions={{} as DisplayActions}
          settings={{} as SettingsActions}
          ask={async () => null}
          onDone={() => {}}
          now={61_000}
        />
      </ul>,
    );
    for (const text of [
      'Show code changes (patches)',
      'Wake the screen when a session needs you',
      'Local time of the bound station. Empty: none.',
      'Clear binding',
      'Show Mission',
      'Revoke',
      'Save',
      'never used',
    ]) {
      expect(html).toContain(text);
    }
  });
});

describe('the Displays page', () => {
  const env = {
    settings: {} as SettingsActions,
    accounts: {} as never,
    tailnet: {} as never,
    displays: {} as DisplayActions,
    desktop: {} as never,
  } as SettingsEnv;

  test('the first render has the add form with the cost text and waits for the list', () => {
    const html = renderToStaticMarkup(<DisplaysTab env={env} stations={stationsList} live />);
    expect(html).toContain('Add a wall display');
    expect(html).toContain(COST_TEXT);
    expect(html).toContain('Loading…');
  });

  test('the add form needs a name and a station', () => {
    const html = renderToStaticMarkup(
      <AddDisplay
        actions={{} as DisplayActions}
        settings={{} as SettingsActions}
        ask={async () => null}
        stations={stationsList}
        onDone={() => {}}
      />,
    );
    expect(html).toContain('Stations that it may show');
    expect(html).toContain('Alpha');
    expect(html).toContain('Beta');
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
    allowed: { desktop: true, capture: false, input: false },
    tools: {
      wtype: true,
      pointer: false,
      recorder: false,
      grim: true,
      chromium: true,
      notify_send: true,
    },
  };
  const env = (acts: Partial<SettingsEnv> = {}) =>
    ({
      settings: {} as SettingsActions,
      accounts: {} as never,
      tailnet: {} as never,
      displays: {} as DisplayActions,
      desktop: {} as never,
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
