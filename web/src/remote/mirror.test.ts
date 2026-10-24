// B17 §3.3 (design `wall-control.md`): the mirror state, the pad table and the keys. Pure functions only.
import { describe, expect, test } from 'bun:test';
import { directiveRoute } from '../display/api';
import type {
  DirectiveBody,
  DirectiveFilter,
  SessionRef,
  WallControl,
  WallControlFrame,
  WallEvent,
  WallRemoteState,
  WallRemoteView,
} from '../gen';
import { card } from '../testing/fleet';
import { wallDefaults } from '../app/routes';
import {
  activeArea,
  applyView,
  initialMirror,
  type MirrorState,
  type PadContext,
  type PadAction,
  type PadKey,
  padAction,
  padKey,
  REBASE_EFFECTS,
  routeDirective,
  scrollAreas,
  selectableRefs,
} from './mirror';

const NO_STATE: WallRemoteState = { selected: null, frame_tenths: 0 };
const MISSION: DirectiveBody = { mode: 'mission', pin: false, filter: null };

const view = (extra: Partial<WallRemoteView> = {}): WallRemoteView => ({
  directive: { generation: 1, directive: MISSION },
  link: 'ready',
  incarnation: 'inc-1',
  seq: 0,
  state: NO_STATE,
  hold_left_ms: 0,
  complete: true,
  controls: [],
  ...extra,
});

const frame = (
  seq: number,
  event: WallEvent,
  extra: Partial<WallControlFrame> = {},
): WallControlFrame => ({
  incarnation: 'inc-1',
  seq,
  generation: 1,
  event,
  state: NO_STATE,
  ...extra,
});

const scroll = (seq: number, step: 'line_down' | 'line_up' | 'page_down', area = 'chat' as const) =>
  frame(seq, { kind: 'control', control: { op: 'scroll', area, step } });

const select = (seq: number, item: SessionRef | null) =>
  frame(
    seq,
    { kind: 'control', control: { op: 'select', item } },
    { state: { selected: item, frame_tenths: 0 } },
  );

const at = (cursor: number, extra: Partial<MirrorState> = {}): MirrorState => ({
  incarnation: 'inc-1',
  cursor,
  holdUntil: 0,
  effects: [],
  ...extra,
});

describe('the mirror state (§3.3.3)', () => {
  test('the_first_view_rebases', () => {
    const out = applyView(
      initialMirror,
      view({ seq: 5, controls: [scroll(4, 'line_down'), scroll(5, 'page_down')] }),
      1_000,
    );
    expect(out.incarnation).toBe('inc-1');
    expect(out.cursor).toBe(5);
    // The relative areas go back to their default position; no frame gives an effect.
    expect(out.effects).toBe(REBASE_EFFECTS);
    expect(REBASE_EFFECTS.at(-1)).toEqual({ kind: 'scroll', area: 'chat', step: 'bottom' });
    expect(REBASE_EFFECTS.slice(0, -1).every((e) => e.kind === 'scroll' && e.step === 'top')).toBe(
      true,
    );
  });

  test('a_new_incarnation_rebases', () => {
    const out = applyView(
      at(7),
      view({
        incarnation: 'inc-2',
        seq: 2,
        controls: [scroll(2, 'line_down')].map((f) => ({ ...f, incarnation: 'inc-2' })),
      }),
      1_000,
    );
    expect(out).toMatchObject({ incarnation: 'inc-2', cursor: 2 });
    expect(out.effects).toBe(REBASE_EFFECTS);
  });

  test('an_incomplete_view_rebases_and_gives_no_effect', () => {
    const out = applyView(
      at(2),
      view({
        seq: 40,
        complete: false,
        controls: [scroll(39, 'line_down'), scroll(40, 'line_up')],
      }),
      1_000,
    );
    expect(out.cursor).toBe(40);
    expect(out.effects).toBe(REBASE_EFFECTS);
  });

  test('frames_apply_in_seq_order_above_the_cursor', () => {
    // The frame at the cursor is a repeat; the others come in `seq` order, whatever the list order.
    const out = applyView(
      at(2),
      view({
        seq: 4,
        controls: [scroll(4, 'page_down'), scroll(2, 'line_up'), scroll(3, 'line_down')],
      }),
      1_000,
    );
    expect(out.cursor).toBe(4);
    expect(out.effects).toEqual([
      { kind: 'scroll', area: 'chat', step: 'line_down' },
      { kind: 'scroll', area: 'chat', step: 'page_down' },
    ]);
    // A view with nothing new keeps the cursor and gives no effect.
    const quiet = applyView(out, view({ seq: 4 }), 2_000);
    expect(quiet.cursor).toBe(4);
    expect(quiet.effects).toEqual([]);
  });

  test('frames of another generation and frames that are not a scroll give no effect', () => {
    const out = applyView(
      at(0),
      view({
        seq: 3,
        directive: { generation: 5, directive: MISSION },
        controls: [
          scroll(1, 'line_down'),
          select(2, { station_id: 'a', session_id: 's' }),
          frame(3, { kind: 'navigate' }, { generation: 5 }),
        ],
      }),
      1_000,
    );
    expect(out.cursor).toBe(3);
    expect(out.effects).toEqual([]);
  });

  test('another_devices_earlier_control_is_applied_first', () => {
    // This device's own POST answered `seq` 2. The next view has the frames 1 (another device) and 2
    // (own): the effects come in the order 1, 2 and the selection is Y, the deck's checkpoint.
    const x: SessionRef = { station_id: 'a', session_id: 'X' };
    const y: SessionRef = { station_id: 'a', session_id: 'Y' };
    const frames = [
      select(1, x),
      frame(1, { kind: 'control', control: { op: 'scroll', area: 'chat', step: 'line_down' } }),
      select(2, y),
      frame(2, { kind: 'control', control: { op: 'scroll', area: 'chat', step: 'line_up' } }),
    ];
    // Each `seq` is one frame in the deck: keep the scrolls as the frames 1 and 2 here.
    const ordered = [frames[1] as WallControlFrame, frames[3] as WallControlFrame];
    const out = applyView(
      at(0),
      view({ seq: 2, state: { selected: y, frame_tenths: 0 }, controls: ordered }),
      1_000,
    );
    expect(out.effects).toEqual([
      { kind: 'scroll', area: 'chat', step: 'line_down' },
      { kind: 'scroll', area: 'chat', step: 'line_up' },
    ]);
  });

  test('state_always_comes_from_the_view', () => {
    // The mirror state holds the cursor, the hold and the effects, and never a selection or a frame
    // position: the page draws the `state` of the view itself.
    const out = applyView(at(0), view({ seq: 1, controls: [select(1, null)] }), 1_000);
    expect(Object.keys(out).sort()).toEqual(['cursor', 'effects', 'holdUntil', 'incarnation']);
  });

  test('the_hold_follows_hold_left_ms', () => {
    expect(applyView(at(0), view({ hold_left_ms: 5_000 }), 1_000).holdUntil).toBe(6_000);
    expect(applyView(at(0), view({ hold_left_ms: 0 }), 1_000).holdUntil).toBe(0);
    // The same for a rebase.
    expect(applyView(initialMirror, view({ hold_left_ms: 250 }), 1_000).holdUntil).toBe(1_250);
    expect(applyView(initialMirror, view({ hold_left_ms: 0 }), 1_000).holdUntil).toBe(0);
  });
});

const filter: DirectiveFilter = { stations: ['a', 'b'], repo: 'app' };
const A: SessionRef = { station_id: 'a', session_id: 's1' };

describe('route_directive_is_the_inverse_of_directive_route', () => {
  const bodies: DirectiveBody[] = [
    { mode: 'mission', pin: true, filter },
    { mode: 'ambient', pin: false, filter: null },
    { mode: 'focus', session: A, pin: true, filter },
    { mode: 'review', session: A, pin: false, filter: null },
    { mode: 'race', batch_id: 'B1', pin: true, filter },
    {
      mode: 'artifact',
      artifact: { station_id: 'a', artifact_id: 'art', version: 3 },
      pin: false,
      filter: null,
    },
    { mode: 'page', page: { page_id: 'P'.repeat(26) }, pin: true, filter },
  ];
  test('every mode comes back as it was', () => {
    for (const body of bodies) {
      expect(routeDirective(directiveRoute(body), body.pin, body.filter)).toEqual(body);
    }
  });

  test('a route without the target of its mode has no directive', () => {
    expect(routeDirective({ ...wallDefaults, mode: 'focus' }, false, null)).toBeNull();
    expect(routeDirective({ ...wallDefaults, mode: 'review' }, false, null)).toBeNull();
    expect(routeDirective({ ...wallDefaults, mode: 'race' }, false, null)).toBeNull();
    expect(routeDirective({ ...wallDefaults, mode: 'page' }, false, null)).toBeNull();
    expect(
      routeDirective(
        { ...wallDefaults, mode: 'artifact', artifact: { station: 'a', id: 'x' } },
        false,
        null,
      ),
    ).toBeNull();
  });
});

describe('pad_action_table', () => {
  const ctx = (extra: Partial<PadContext> = {}): PadContext => ({
    current: { mode: 'mission', pin: true, filter },
    selected: null,
    items: ['a/s1', 'a/s2', 'b/s3'],
    first: { station: 'a', id: 's1' },
    area: 'needs',
    ...extra,
  });
  const nav = (directive: DirectiveBody): PadAction => ({ kind: 'navigate', directive });
  const ctl = (control: WallControl): PadAction => ({ kind: 'control', control });

  test('the views step the mode and keep the pin and the filter', () => {
    expect(padAction('next_view', ctx())).toEqual(
      nav({ mode: 'focus', session: A, pin: true, filter }),
    );
    expect(padAction('previous_view', ctx())).toEqual(nav({ mode: 'ambient', pin: true, filter }));
    // The selected item is the target of Focus before the first tile.
    expect(
      padAction('next_view', ctx({ selected: { station_id: 'b', session_id: 's3' } })),
    ).toEqual(
      nav({ mode: 'focus', session: { station_id: 'b', session_id: 's3' }, pin: true, filter }),
    );
    // With no target for Focus and Review the next view is Ambient.
    expect(padAction('next_view', ctx({ first: null }))).toEqual(
      nav({ mode: 'ambient', pin: true, filter }),
    );
  });

  test('Open focuses the selected item, only in Mission and Race, with a selection', () => {
    const selected = { station_id: 'a', session_id: 's2' };
    expect(padAction('open', ctx({ selected }))).toEqual(
      nav({ mode: 'focus', session: selected, pin: true, filter }),
    );
    expect(padAction('open', ctx())).toBeNull();
    const race: DirectiveBody = { mode: 'race', batch_id: 'B1', pin: true, filter };
    expect(padAction('open', ctx({ current: race, selected }))).toEqual(
      nav({ mode: 'focus', session: selected, pin: true, filter }),
    );
    const focus: DirectiveBody = { mode: 'focus', session: A, pin: true, filter };
    expect(padAction('open', ctx({ current: focus, selected }))).toBeNull();
    const ambient: DirectiveBody = { mode: 'ambient', pin: true, filter };
    expect(padAction('open', ctx({ current: ambient, selected }))).toBeNull();
  });

  test('Back clears the selection in Mission and goes to Mission in every other mode', () => {
    expect(padAction('back', ctx())).toBeNull();
    expect(padAction('back', ctx({ selected: A }))).toEqual(ctl({ op: 'select', item: null }));
    for (const current of [
      { mode: 'focus', session: A, pin: true, filter },
      { mode: 'review', session: A, pin: true, filter },
      { mode: 'race', batch_id: 'B1', pin: true, filter },
      { mode: 'ambient', pin: true, filter },
    ] as DirectiveBody[]) {
      expect(padAction('back', ctx({ current, selected: null }))).toEqual(
        nav({ mode: 'mission', pin: true, filter }),
      );
    }
  });

  test('the item buttons select the absolute item, with wrap, and are off without items', () => {
    expect(padAction('next_item', ctx())).toEqual(
      ctl({ op: 'select', item: { station_id: 'a', session_id: 's1' } }),
    );
    expect(padAction('previous_item', ctx())).toEqual(
      ctl({ op: 'select', item: { station_id: 'b', session_id: 's3' } }),
    );
    expect(
      padAction('next_item', ctx({ selected: { station_id: 'b', session_id: 's3' } })),
    ).toEqual(ctl({ op: 'select', item: { station_id: 'a', session_id: 's1' } }));
    expect(padAction('previous_item', ctx({ selected: A }))).toEqual(
      ctl({ op: 'select', item: { station_id: 'b', session_id: 's3' } }),
    );
    expect(padAction('next_item', ctx({ items: [] }))).toBeNull();
    expect(padAction('previous_item', ctx({ items: [] }))).toBeNull();
  });

  test('the scroll buttons send the area and the step, and are off without an area', () => {
    const steps = [
      ['scroll_up', 'page_up'],
      ['scroll_down', 'page_down'],
      ['top', 'top'],
      ['bottom', 'bottom'],
      ['line_up', 'line_up'],
      ['line_down', 'line_down'],
    ] as const;
    for (const [button, step] of steps) {
      expect(padAction(button, ctx({ area: 'chat' }))).toEqual(
        ctl({ op: 'scroll', area: 'chat', step }),
      );
      expect(padAction(button, ctx({ area: null }))).toBeNull();
    }
  });
});

describe('the scroll areas and the keys', () => {
  test('the areas follow the mode', () => {
    expect(scrollAreas('mission').map((a) => a.label)).toEqual(['Needs you', 'Activity']);
    expect(scrollAreas('focus').map((a) => a.label)).toEqual(['Chat', 'Changes']);
    expect(scrollAreas('review').map((a) => a.label)).toEqual(['Changes']);
    expect(scrollAreas('artifact').map((a) => a.label)).toEqual(['Artifact']);
    expect(scrollAreas('page').map((a) => a.label)).toEqual(['Page']);
    expect(scrollAreas('race')).toEqual([]);
    expect(scrollAreas('ambient')).toEqual([]);
    // The chosen area counts only when the mode has it; else the first of the mode; else none.
    expect(activeArea('focus', 'changes')).toBe('changes');
    expect(activeArea('focus', 'needs')).toBe('chat');
    expect(activeArea('mission', null)).toBe('needs');
    expect(activeArea('race', 'needs')).toBeNull();
  });

  const key = (k: string, extra: Partial<Parameters<typeof padKey>[0]> = {}) =>
    padKey({ key: k, ctrlKey: false, metaKey: false, altKey: false, tag: 'DIV', ...extra });

  test('each key of the list is one pad key', () => {
    const table: Record<string, PadKey> = {
      ArrowLeft: 'previous_view',
      ArrowRight: 'next_view',
      j: 'previous_item',
      J: 'previous_item',
      k: 'next_item',
      Enter: 'open',
      Escape: 'back',
      ArrowUp: 'line_up',
      ArrowDown: 'line_down',
      PageUp: 'scroll_up',
      PageDown: 'scroll_down',
      Home: 'top',
      End: 'bottom',
      '[': 'previous_area',
      ']': 'next_area',
    };
    for (const [k, expected] of Object.entries(table)) expect(key(k)).toBe(expected);
    expect(key('x')).toBeNull();
  });

  test('a modifier, a field, and Enter on a button do nothing', () => {
    expect(key('ArrowLeft', { ctrlKey: true })).toBeNull();
    expect(key('ArrowLeft', { metaKey: true })).toBeNull();
    expect(key('ArrowLeft', { altKey: true })).toBeNull();
    for (const tag of ['INPUT', 'SELECT', 'TEXTAREA']) expect(key('ArrowDown', { tag })).toBeNull();
    for (const tag of ['BUTTON', 'A', 'SUMMARY']) expect(key('Enter', { tag })).toBeNull();
    // A button keeps the other keys of the pad.
    expect(key('PageDown', { tag: 'BUTTON' })).toBe('scroll_down');
  });
});

describe('the selectable refs of the mirror', () => {
  test('Mission lists the needs items, then the tiles, with no repeat; Focus has none', () => {
    const sessions = [card('s1', 'working'), card('s2', 'idle')];
    const refs = selectableRefs({ ...wallDefaults, mode: 'mission' }, sessions);
    expect(refs).toEqual([...new Set(refs)]);
    expect([...refs].sort()).toEqual(['station-a/s1', 'station-a/s2']);
    expect(selectableRefs({ ...wallDefaults, mode: 'ambient' }, sessions)).toEqual([]);
  });
});
