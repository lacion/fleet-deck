// B17 §3.3 (design `wall-control.md`): the Remote page. The link (polls, one action at a time, texts),
// the calls of the deck, and the pieces of the page that render without a browser.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { displayActions } from '../displays/actions';
import type { Timers } from '../desktop/flow';
import type {
  DesktopInfo,
  DirectiveBody,
  DisplayView,
  WallControl,
  WallControlAck,
  WallRemoteView,
} from '../gen';
import { SettingsProvider, type SettingsEnv } from '../settings/context';
import { DeckError, type DeckIo } from '../settings/http';
import { station } from '../testing/fleet';
import {
  CANNOT_SHOW_TEXT,
  failureText,
  GONE_TEXT,
  NO_REMOTE_TEXT,
  OFFLINE_TEXT,
  OLDER_TEXT,
  OWNER_ONLY_TEXT,
  POLL_HIDDEN_MS,
  POLL_VISIBLE_MS,
  type RemoteCalls,
  RemoteLink,
  SENT_TEXT,
  STATUS_CLEAR_MS,
  TOO_MANY_TEXT,
} from './api';
import {
  mirrorData,
  mirrorRoute,
  mirrorScope,
  monitorSize,
  type PadButton,
  padAction,
  scrollAreas,
} from './mirror';
import { KEYS_TEXT, Pad, PAD_BUTTONS } from './Pad';
import {
  CAPTION_TEXT,
  Footer,
  LinkLine,
  loadSetup,
  MISSION_TEXT,
  RemotePage,
  type Setup,
  SetupNotice,
  screenshotMonitor,
  SHOT_TEXT,
} from './RemotePage';

const MISSION: DirectiveBody = { mode: 'mission', pin: false, filter: null };

const view = (extra: Partial<WallRemoteView> = {}): WallRemoteView => ({
  directive: { generation: 1, directive: MISSION },
  link: 'ready',
  incarnation: 'inc-1',
  seq: 0,
  state: { selected: null, frame_tenths: 0 },
  hold_left_ms: 0,
  complete: true,
  controls: [],
  ...extra,
});

/** Timers that run only when the test says so. */
function fakeTimers() {
  const queue: { id: number; fn: () => void; ms: number }[] = [];
  let next = 0;
  /** Runs the first timer that waits `ms`, and removes it (a timer runs once). */
  const fire = (ms: number) => {
    const at = queue.findIndex((t) => t.ms === ms);
    const [timer] = at < 0 ? [] : queue.splice(at, 1);
    timer?.fn();
  };
  const timers: Timers = {
    set: (fn, ms) => {
      next += 1;
      queue.push({ id: next, fn, ms });
      return next;
    },
    clear: (h) => {
      const at = queue.findIndex((t) => t.id === h);
      if (at >= 0) queue.splice(at, 1);
    },
  };
  return { timers, queue, fire };
}

type Log = string[];

/** Fake calls that log in order. `gate` holds a call until the test opens it. */
function fakeCalls(log: Log, over: Partial<RemoteCalls> = {}, views: WallRemoteView[] = [view()]) {
  const state = { polls: 0 };
  let ackSeq = 0;
  const ack = (): WallControlAck => {
    ackSeq += 1;
    return { incarnation: 'inc-1', seq: ackSeq, generation: 1 };
  };
  const calls: RemoteCalls = {
    remote: async (_id, after) => {
      log.push(`remote:${after}`);
      const v = views[Math.min(state.polls, views.length - 1)] as WallRemoteView;
      state.polls += 1;
      return v;
    },
    control: async (_id, control: WallControl) => {
      log.push(`control:${JSON.stringify(control)}`);
      return ack();
    },
    remoteNavigate: async (_id, directive) => {
      log.push(`navigate:${directive.mode}`);
      return ack();
    },
    directive: async (_id, body) => {
      log.push(`directive:${body.mode}`);
      return { generation: 2, directive: body };
    },
    ...over,
  };
  return calls;
}

const settle = async () => {
  for (let i = 0; i < 20; i++) await Promise.resolve();
};

const started = async (log: Log, over: Partial<RemoteCalls> = {}, views?: WallRemoteView[]) => {
  const clock = fakeTimers();
  const link = new RemoteLink({
    id: 'D1',
    calls: fakeCalls(log, over, views),
    now: () => 10_000,
    timers: clock.timers,
  });
  link.start();
  await settle();
  return { link, clock };
};

describe('the calls of the deck', () => {
  const fakeIo = (answers: Record<string, unknown>) => {
    const calls: { method: string; path: string; body?: unknown }[] = [];
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
  };
  const ack = { incarnation: 'i', seq: 3, generation: 1 };

  test('the_pad_posts_each_control_and_navigation', async () => {
    const { io, calls } = fakeIo({
      '/api/displays/D%201/remote?after=7': view({ seq: 7 }),
      '/api/displays/D%201/control': ack,
      '/api/displays/D%201/remote/navigate': ack,
    });
    const actions = displayActions(io);
    expect((await actions.remote('D 1', 7)).seq).toBe(7);
    await actions.control('D 1', { op: 'scroll', area: 'chat', step: 'page_down' });
    await actions.control('D 1', { op: 'select', item: null });
    await actions.remoteNavigate('D 1', MISSION);
    expect(calls).toEqual([
      { method: 'GET', path: '/api/displays/D%201/remote?after=7' },
      {
        method: 'POST',
        path: '/api/displays/D%201/control',
        body: { control: { op: 'scroll', area: 'chat', step: 'page_down' } },
      },
      {
        method: 'POST',
        path: '/api/displays/D%201/control',
        body: { control: { op: 'select', item: null } },
      },
      {
        method: 'POST',
        path: '/api/displays/D%201/remote/navigate',
        body: { directive: MISSION },
      },
    ]);
  });

  test('a bad answer is an error', async () => {
    const bad = displayActions(fakeIo({}).io);
    await expect(bad.remote('D', 0)).rejects.toThrow('invalid remote state');
    await expect(bad.control('D', { op: 'select', item: null })).rejects.toThrow('invalid answer');
    await expect(bad.remoteNavigate('D', MISSION)).rejects.toThrow('invalid answer');
  });
});

describe('the link', () => {
  test('it polls with the cursor, every second while visible and every 10 s while hidden', async () => {
    const log: Log = [];
    const { link, clock } = await started(log, {}, [view({ seq: 4 }), view({ seq: 4 })]);
    expect(log).toEqual(['remote:0']);
    expect(link.getSnapshot().mirror.cursor).toBe(4);
    expect(clock.queue.map((t) => t.ms)).toEqual([POLL_VISIBLE_MS]);
    clock.fire(POLL_VISIBLE_MS);
    await settle();
    expect(log).toEqual(['remote:0', 'remote:4']);
    // Hidden: the next wait is 10 s. Visible again: one poll at once.
    link.setVisible(false);
    expect(clock.queue.map((t) => t.ms)).toEqual([POLL_HIDDEN_MS]);
    link.setVisible(true);
    await settle();
    expect(log).toHaveLength(3);
    link.stop();
    expect(clock.queue).toEqual([]);
  });

  test('the_mirror_applies_nothing_from_an_answer_and_polls_at_once', async () => {
    const log: Log = [];
    const { link } = await started(log, {}, [
      view({ seq: 1 }),
      // The next poll has the frame of the own control (seq 2) and another device's (seq 1 was seen).
      view({
        seq: 2,
        controls: [
          {
            incarnation: 'inc-1',
            seq: 2,
            generation: 1,
            event: { kind: 'control', control: { op: 'scroll', area: 'chat', step: 'page_down' } },
            state: { selected: null, frame_tenths: 0 },
          },
        ],
      }),
    ]);
    const before = link.getSnapshot().mirror;
    expect(before.cursor).toBe(1);
    const seen: number[] = [];
    link.subscribe(() => seen.push(link.getSnapshot().mirror.cursor));
    await link.press({
      kind: 'control',
      control: { op: 'scroll', area: 'chat', step: 'page_down' },
    });
    // The answer (seq 1 of the fake) changed nothing; the poll after it carried the frame.
    expect(log).toEqual([
      'remote:0',
      `control:${JSON.stringify({ op: 'scroll', area: 'chat', step: 'page_down' })}`,
      'remote:1',
    ]);
    expect(seen.filter((c) => c === 1).length).toBeGreaterThan(0);
    expect(link.getSnapshot().mirror.cursor).toBe(2);
    expect(link.getSnapshot().mirror.effects).toEqual([
      { kind: 'scroll', area: 'chat', step: 'page_down' },
    ]);
  });

  test('too_many_controls_shows_the_text: an error answer polls at once and never repeats the POST', async () => {
    const log: Log = [];
    const { link } = await started(log, {
      control: async () => {
        log.push('control');
        throw new DeckError(429, 'x', 'too_many_controls', {});
      },
    });
    await link.press({ kind: 'control', control: { op: 'select', item: null } });
    expect(log).toEqual(['remote:0', 'control', 'remote:0']);
    expect(link.getSnapshot().status).toEqual({
      text: 'Too many controls. Wait a moment.',
      tone: 'error',
    });
    expect(TOO_MANY_TEXT).toBe('Too many controls. Wait a moment.');
  });

  test('one_action_at_a_time', async () => {
    const log: Log = [];
    let open: () => void = () => {};
    const { link } = await started(log, {
      control: async (_id, control) => {
        log.push(`control:${JSON.stringify(control)}`);
        await new Promise<void>((resolve) => {
          open = resolve;
        });
        return { incarnation: 'inc-1', seq: 1, generation: 1 };
      },
    });
    const first = link.press({ kind: 'control', control: { op: 'select', item: null } });
    expect(link.getSnapshot().busy).toBe(true);
    // A press while the POST is in flight does nothing (the control, the navigation, the switch).
    await link.press({ kind: 'control', control: { op: 'scroll', area: 'needs', step: 'top' } });
    await link.press({ kind: 'navigate', directive: MISSION });
    await link.press({ kind: 'switch', directive: MISSION });
    expect(log.filter((l) => l.startsWith('control'))).toHaveLength(1);
    expect(log.filter((l) => l.startsWith('navigate') || l.startsWith('directive'))).toEqual([]);
    open();
    await first;
    expect(link.getSnapshot().busy).toBe(false);
    // The next press goes through.
    const second = link.press({ kind: 'navigate', directive: MISSION });
    await settle();
    await second;
    expect(log).toContain('navigate:mission');
  });

  test('a success shows "Sent." and the text clears after 4 seconds', async () => {
    const log: Log = [];
    const { link, clock } = await started(log);
    await link.press({ kind: 'switch', directive: MISSION });
    expect(log).toContain('directive:mission');
    expect(link.getSnapshot().status).toEqual({ text: SENT_TEXT, tone: 'ok' });
    expect(clock.queue.some((t) => t.ms === STATUS_CLEAR_MS)).toBe(true);
    clock.fire(STATUS_CLEAR_MS);
    expect(link.getSnapshot().status).toBeNull();
  });

  test('a failed poll shows its text and keeps the last view', async () => {
    const log: Log = [];
    let fail = false;
    const { link, clock } = await started(log, {
      remote: async () => {
        log.push('remote');
        if (fail) throw new DeckError(404, 'x', 'display_unknown', {});
        return view({ seq: 3 });
      },
    });
    fail = true;
    clock.fire(POLL_VISIBLE_MS);
    await settle();
    expect(link.getSnapshot().problem).toBe(GONE_TEXT);
    expect(link.getSnapshot().view?.seq).toBe(3);
  });

  test('the status texts follow the answer of the deck', () => {
    const e = (status: number, code: string) => new DeckError(status, 'raw', code, {});
    expect(failureText(e(409, 'wall_offline'))).toBe(OFFLINE_TEXT);
    expect(failureText(e(409, 'wall_controls_unsupported'))).toBe(OLDER_TEXT);
    expect(failureText(e(429, 'too_many_controls'))).toBe(TOO_MANY_TEXT);
    expect(failureText(e(404, 'display_unknown'))).toBe(GONE_TEXT);
    expect(failureText(e(403, 'outside_ceiling'))).toBe(CANNOT_SHOW_TEXT);
    expect(failureText(e(400, 'invalid_control'))).toBe(CANNOT_SHOW_TEXT);
    expect(failureText(e(500, 'x'))).toBe('raw');
    expect(failureText(new Error('plain'))).toBe('plain');
  });
});

const html = (node: React.ReactElement) => renderToStaticMarkup(node);

describe('the link line', () => {
  test('older_wall_shows_the_reload_text_and_button', () => {
    const out = html(<LinkLine link="older" onReload={() => {}} />);
    expect(out).toContain(OLDER_TEXT);
    expect(out).toContain('</svg> Reload the wall</button>');
    // Without the wall in the Desktop of the station there is no button.
    const none = html(<LinkLine link="older" onReload={null} />);
    expect(none).toContain(OLDER_TEXT);
    expect(none).not.toContain('<button');
  });

  test('offline_wall_shows_the_text', () => {
    const out = html(<LinkLine link="offline" onReload={() => {}} />);
    expect(out).toContain('The wall is not connected.');
    expect(out).not.toContain('<button');
    expect(out).not.toContain('Connected</p>');
  });

  test('a ready wall is connected', () => {
    const out = html(<LinkLine link="ready" onReload={null} />);
    expect(out).toContain('data-status="online"');
    expect(out).toContain('Connected');
  });
});

describe('the setup', () => {
  test('deck_without_the_feature_shows_no_pad', async () => {
    const env = {
      capture: { features: async () => ['artifacts_video.v1'] },
      displays: { list: async () => ({ displays: [], pending: [] }) },
      desktop: {},
    } as unknown as SettingsEnv;
    const setup = await loadSetup(env, 'D1', []);
    expect(setup.kind).toBe('no_feature');
    const out = html(<SetupNotice setup={setup} />);
    expect(out).toContain(NO_REMOTE_TEXT);
    expect(out).not.toContain('Remote pad');
    // An old deck (no feature list at all) has no remote either.
    const old = { ...env, capture: { features: async () => null } } as unknown as SettingsEnv;
    expect((await loadSetup(old, 'D1', [])).kind).toBe('no_feature');
  });

  test('a display that is not in the list does not exist any more', async () => {
    const env = {
      capture: { features: async () => ['wall_control.v1'] },
      displays: { list: async () => ({ displays: [], pending: [] }) },
      desktop: {},
    } as unknown as SettingsEnv;
    const setup = await loadSetup(env, 'D1', []);
    expect(setup.kind).toBe('missing');
    expect(html(<SetupNotice setup={setup} />)).toContain(GONE_TEXT);
  });

  test('a ready setup reads the desktop of the bound station', async () => {
    const d = display({ binding: { station_id: 'station-a', monitor: 'DP-1' } });
    const seen: string[] = [];
    const env = {
      capture: { features: async () => ['wall_control.v1'] },
      displays: { list: async () => ({ displays: [d], pending: [] }) },
      desktop: {
        info: async (name: string) => {
          seen.push(name);
          return info;
        },
      },
    } as unknown as SettingsEnv;
    const setup = await loadSetup(env, d.display_id, [station('station-a')]);
    expect(seen).toEqual(['Alpha']);
    expect(setup).toMatchObject({ kind: 'ready', info });
    // An unbound display reads no desktop.
    const free = {
      ...env,
      displays: { list: async () => ({ displays: [display()], pending: [] }) },
    };
    const none = await loadSetup(free as unknown as SettingsEnv, display().display_id, []);
    expect(none).toMatchObject({ kind: 'ready', info: undefined });
  });

  test('loading and error states have no pad', () => {
    const states: Setup[] = [
      { kind: 'loading' },
      { kind: 'error', text: 'The deck did not answer.' },
    ];
    for (const setup of states) {
      expect(html(<SetupNotice setup={setup} />)).not.toContain('Remote pad');
    }
    expect(html(<SetupNotice setup={{ kind: 'error', text: 'Boom.' }} />)).toContain('Boom.');
  });

  test('another login sees one line and no pad', () => {
    // No settings context: not the owner's login.
    const out = html(
      <RemotePage id="D1" freshness="live" board={{ sessions: [], stations: [] }} />,
    );
    expect(out).toContain(OWNER_ONLY_TEXT);
    expect(out).not.toContain('Remote pad');
    // The owner's login starts with the loading line.
    const env = {} as SettingsEnv;
    const owner = html(
      <SettingsProvider env={env}>
        <RemotePage id="D1" freshness="live" board={{ sessions: [], stations: [] }} />
      </SettingsProvider>,
    );
    expect(owner).toContain('Loading the remote');
    expect(owner).toContain('Remote');
  });
});

const display = (extra: Partial<DisplayView> = {}): DisplayView => ({
  display_id: 'D'.repeat(26),
  name: 'Wall',
  ceiling: ['station-a', 'station-b'],
  show_diffs: false,
  wake_on_needs_you: true,
  allow_apps: false,
  quiet_hours: null,
  binding: null,
  created_at: 1_000,
  last_seen: null,
  ...extra,
});

const info: DesktopInfo = {
  kind: 'omarchy',
  omarchy_version: '3.1',
  monitors: [
    {
      name: 'DP-1',
      description: 'Main',
      width: 3840,
      height: 2160,
      scale_pct: 200,
      refresh_hz: 60,
      focused: true,
      dpms_on: true,
    },
  ],
  locked: false,
  dnd: false,
  idle_inhibited: null,
  walls: [],
  allowed: { desktop: true, capture: true, capture_monitors: [], input: false },
  tools: {
    wtype: true,
    pointer: false,
    recorder: false,
    grim: true,
    chromium: true,
    notify_send: true,
  },
  features: ['desktop_capture.v1'],
};

describe('the mirror data', () => {
  test('the_mirror_uses_the_display_ceiling_and_the_directive_filter', () => {
    const ceiling = ['station-a', 'station-b'];
    // No filter: the whole ceiling.
    expect(mirrorScope(ceiling, MISSION)).toEqual({ stations: ceiling, repo: '' });
    // A filter narrows the ceiling and cannot widen it.
    const narrow: DirectiveBody = {
      mode: 'mission',
      pin: false,
      filter: { stations: ['station-b', 'station-z'], repo: 'app' },
    };
    expect(mirrorScope(ceiling, narrow)).toEqual({ stations: ['station-b'], repo: 'app' });
    const route = mirrorRoute(ceiling, narrow);
    expect(route).toMatchObject({
      name: 'wall',
      mode: 'mission',
      stations: ['station-b'],
      repo: 'app',
    });
    // The data holds only the sessions and stations of the scope; an empty scope shows nothing.
    const sessions = [
      { station_id: 'station-a', status: 'idle', summary: { session_id: 'a1' } },
      { station_id: 'station-b', status: 'idle', summary: { session_id: 'b1' } },
      { station_id: 'station-c', status: 'idle', summary: { session_id: 'c1' } },
    ] as never;
    const data = mirrorData(sessions, [station('station-a'), station('station-b')], {
      stations: ['station-b'],
      repo: '',
    });
    expect(data.sessions.map((c) => c.summary.session_id)).toEqual(['b1']);
    expect(data.stations.map((s) => s.station_id)).toEqual(['station-b']);
    const empty = mirrorData(sessions, [station('station-a')], { stations: [], repo: '' });
    expect(empty.sessions).toEqual([]);
    expect(empty.stations).toEqual([]);
    const emptyScope = mirrorScope(ceiling, {
      ...narrow,
      filter: { stations: ['station-z'], repo: null },
    });
    expect(emptyScope.stations).toEqual([]);
  });

  test('the monitor size is the logical size, or 1920 by 1080', () => {
    expect(monitorSize(info, 'DP-1')).toEqual({ w: 1920, h: 1080 });
    const small = {
      monitors: [{ ...info.monitors[0], width: 2880, height: 1800, scale_pct: 150 }],
    };
    expect(monitorSize(small as never, 'DP-1')).toEqual({ w: 1920, h: 1200 });
    expect(monitorSize(info, 'HDMI-9')).toEqual({ w: 1920, h: 1080 });
    expect(monitorSize(info, null)).toEqual({ w: 1920, h: 1080 });
    expect(monitorSize(undefined, 'DP-1')).toEqual({ w: 1920, h: 1080 });
    const odd = { monitors: [{ ...info.monitors[0], scale_pct: 0 }] };
    expect(monitorSize(odd as never, 'DP-1')).toEqual({ w: 1920, h: 1080 });
  });
});

describe('the pad', () => {
  const noop = () => {};
  const pad = (mode: Parameters<typeof Pad>[0]['mode'], off: PadButton[] = []) =>
    html(
      <Pad
        mode={mode}
        off={new Set(off)}
        area={scrollAreas(mode)[0]?.area ?? null}
        onPress={noop}
        onArea={noop}
        onKeyDown={noop}
      />,
    );

  test('every control is a named button, with its icon hidden from assistive technology', () => {
    const out = pad('mission');
    for (const name of [
      'Previous view',
      'Next view',
      'Previous item',
      'Next item',
      'Open',
      'Back',
      'Scroll up',
      'Scroll down',
      'To the top',
      'To the end',
    ]) {
      expect(out).toContain(name);
    }
    expect(out).toContain('Mission');
    expect(out).toContain('aria-hidden="true"');
    expect(out.match(/<button/g)?.length).toBe(PAD_BUTTONS.length + 2);
  });

  test('the_scroll_areas_follow_the_mode', () => {
    const labels = (mode: Parameters<typeof pad>[0]) =>
      [...pad(mode).matchAll(/data-area="(\w+)"/g)].map((m) => m[1]);
    expect(labels('mission')).toEqual(['needs', 'stream']);
    expect(labels('focus')).toEqual(['chat', 'changes']);
    expect(labels('review')).toEqual(['changes']);
    expect(labels('artifact')).toEqual(['artifact']);
    expect(labels('race')).toEqual([]);
    expect(labels('ambient')).toEqual([]);
    expect(pad('race')).not.toContain('Scroll area');
  });

  test('a disabled button is off in the markup', () => {
    const out = pad('mission', ['open', 'back']);
    expect(out).toMatch(/data-pad="open" disabled/);
    expect(out).toMatch(/data-pad="back" disabled/);
    expect(out).not.toMatch(/data-pad="next_view" disabled/);
  });

  test('keys_act_only_inside_the_pad', () => {
    // The pad root owns the one key handler; the page adds none to the document.
    let got = 0;
    const element = Pad({
      mode: 'mission',
      off: new Set(),
      area: 'needs',
      onPress: noop,
      onArea: noop,
      onKeyDown: () => {
        got += 1;
      },
    });
    const props = element.props as { onKeyDown: () => void; role: string };
    expect(props.role).toBe('group');
    props.onKeyDown();
    expect(got).toBe(1);
  });
});

describe('the footer', () => {
  test('it has Switch to Mission view, the keys, and the screenshot only when given', () => {
    const bare = html(<Footer onMission={() => {}} shot={null} />);
    expect(bare).toContain(MISSION_TEXT);
    expect(bare).toContain('<summary>Keys</summary>');
    expect(bare).toContain(KEYS_TEXT);
    expect(bare).not.toContain(SHOT_TEXT);
    const withShot = html(
      <Footer onMission={() => {}} shot={<button type="button">{SHOT_TEXT}</button>} />,
    );
    expect(withShot).toContain(SHOT_TEXT);
  });

  test('the caption says that the mirror is not a picture of the screen', () => {
    expect(CAPTION_TEXT).toBe(
      'A copy of what the wall shows, made on this device. It is not a picture of the screen.',
    );
  });

  test('the_screenshot_button_shows_only_for_a_bound_capture_monitor', () => {
    const deck = ['desktop_capture.v1', 'wall_control.v1'];
    const bound = display({ binding: { station_id: 'station-a', monitor: 'DP-1' } });
    expect(screenshotMonitor(bound, info, deck)).toBe('DP-1');
    // Not bound, or the station reports no desktop.
    expect(screenshotMonitor(display(), info, deck)).toBeNull();
    expect(screenshotMonitor(bound, undefined, deck)).toBeNull();
    // The deck, the station or the monitor does not allow the capture.
    expect(screenshotMonitor(bound, info, ['wall_control.v1'])).toBeNull();
    expect(screenshotMonitor(bound, info, null)).toBeNull();
    expect(screenshotMonitor(bound, { ...info, features: [] }, deck)).toBeNull();
    expect(
      screenshotMonitor(bound, { ...info, tools: { ...info.tools, grim: false } }, deck),
    ).toBeNull();
    expect(
      screenshotMonitor(bound, { ...info, allowed: { ...info.allowed, capture: false } }, deck),
    ).toBeNull();
    expect(
      screenshotMonitor(
        bound,
        { ...info, allowed: { ...info.allowed, capture_monitors: ['HDMI-1'] } },
        deck,
      ),
    ).toBeNull();
  });
});

describe('the pad table and the link', () => {
  test('a pad action goes through the link as one call', async () => {
    const log: Log = [];
    const { link } = await started(log);
    const action = padAction('next_item', {
      current: MISSION,
      selected: null,
      items: ['station-a/s1'],
      first: null,
      area: 'needs',
    });
    expect(action).not.toBeNull();
    if (action !== null) await link.press(action);
    expect(log[1]).toBe(
      `control:${JSON.stringify({ op: 'select', item: { station_id: 'station-a', session_id: 's1' } })}`,
    );
  });
});
