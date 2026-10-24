// B17 §3.1.5: the pure parts of the wall controls.
import { describe, expect, test } from 'bun:test';
import type { WallControl, WallControlFrame, WallRemoteState } from '../gen';
import {
  applyFrame,
  type ControlState,
  initialControl,
  REMOTE_HOLD_MS,
  selectable,
  shownRemote,
  stepSelection,
} from './control';

const NOW = 1_000_000;
const ctx = (shown: number | null) => ({ shown, now: NOW });
const rest: WallRemoteState = { selected: null, frame_tenths: 0 };
const picked: WallRemoteState = {
  selected: { station_id: 'st', session_id: 'se' },
  frame_tenths: 20,
};

const frame = (
  seq: number,
  generation: number,
  event: WallControlFrame['event'],
  state: WallRemoteState = rest,
  incarnation = 'inc-1',
): WallControlFrame => ({ incarnation, seq, generation, event, state });
const scroll = (area: WallControl & { op: 'scroll' }): WallControlFrame['event'] => ({
  kind: 'control',
  control: area,
});
const down = { op: 'scroll', area: 'chat', step: 'page_down' } as const;

describe('applyFrame', () => {
  test('stale_generation_does_nothing', () => {
    const out = applyFrame(initialControl, frame(1, 2, scroll(down)), ctx(3));
    expect(out.state).toBe(initialControl);
    expect(out.effect).toEqual({ kind: 'none' });
    expect(out.reread).toBe(false);
  });

  test('a_newer_generation_asks_for_a_reread_and_keeps_the_state', () => {
    const out = applyFrame(initialControl, frame(1, 5, scroll(down), picked), ctx(4));
    expect(out.reread).toBe(true);
    expect(out.effect).toEqual({ kind: 'none' });
    expect(out.state.generation).toBe(5);
    expect(out.state.remote).toEqual(picked);
    expect(out.state.holdUntil).toBe(NOW + REMOTE_HOLD_MS);
    // The wall shows generation 4 still: it draws the default. At 5 it draws the kept state.
    expect(shownRemote(out.state, 4)).toEqual(rest);
    expect(shownRemote(out.state, 5)).toEqual(picked);
    // No directive is shown yet: the same.
    expect(applyFrame(initialControl, frame(1, 0, { kind: 'navigate' }), ctx(null)).reread).toBe(
      true,
    );
  });

  test('a_repeated_seq_does_nothing', () => {
    const first = applyFrame(initialControl, frame(3, 1, scroll(down)), ctx(1));
    expect(first.effect).toEqual({ kind: 'scroll', area: 'chat', step: 'page_down' });
    for (const seq of [3, 2, 1]) {
      const again = applyFrame(first.state, frame(seq, 1, scroll(down)), ctx(1));
      expect(again.state).toBe(first.state);
      expect(again.effect).toEqual({ kind: 'none' });
    }
    expect(applyFrame(first.state, frame(4, 1, scroll(down)), ctx(1)).state.seq).toBe(4);
  });

  test('a_new_incarnation_is_accepted', () => {
    const first = applyFrame(initialControl, frame(40, 1, scroll(down)), ctx(1));
    expect(first.state.seq).toBe(40);
    const other = applyFrame(first.state, frame(1, 1, scroll(down), rest, 'inc-2'), ctx(1));
    expect(other.state.incarnation).toBe('inc-2');
    expect(other.state.seq).toBe(1);
    expect(other.effect).toEqual({ kind: 'scroll', area: 'chat', step: 'page_down' });
  });

  test('sync_sets_the_state_without_a_hold', () => {
    const held: ControlState = { ...initialControl, holdUntil: 5 };
    const out = applyFrame(held, frame(7, 2, { kind: 'sync' }, picked), ctx(2));
    expect(out.state.remote).toEqual(picked);
    expect(out.state.holdUntil).toBe(5);
    expect(out.state.seq).toBe(7);
    expect(out.effect).toEqual({ kind: 'none' });
    expect(out.reread).toBe(false);
    // A sync of the same seq (an attach after a lag) is accepted and sets the state again.
    expect(applyFrame(out.state, frame(7, 2, { kind: 'sync' }, rest), ctx(2)).state.remote).toEqual(
      rest,
    );
    // A sync with a newer generation asks for a reread and sets no hold.
    const newer = applyFrame(initialControl, frame(7, 3, { kind: 'sync' }, picked), ctx(2));
    expect(newer.reread).toBe(true);
    expect(newer.state.holdUntil).toBe(0);
  });

  test('control_and_navigate_set_the_hold', () => {
    const c = applyFrame(initialControl, frame(1, 1, scroll(down)), ctx(1));
    expect(c.state.holdUntil).toBe(NOW + 120_000);
    const n = applyFrame(initialControl, frame(1, 1, { kind: 'navigate' }), ctx(1));
    expect(n.state.holdUntil).toBe(NOW + 120_000);
    expect(n.effect).toEqual({ kind: 'none' });
    const select = applyFrame(
      initialControl,
      frame(1, 1, { kind: 'control', control: { op: 'select', item: null } }, picked),
      ctx(1),
    );
    expect(select.state.holdUntil).toBe(NOW + 120_000);
    expect(select.state.remote).toEqual(picked);
    expect(select.effect).toEqual({ kind: 'none' });
  });

  test('only_relative_areas_give_a_scroll_effect', () => {
    for (const area of ['needs', 'stream', 'chat', 'changes', 'artifact'] as const) {
      const out = applyFrame(
        initialControl,
        frame(1, 1, scroll({ op: 'scroll', area, step: 'top' })),
        ctx(1),
      );
      expect(out.effect).toEqual({ kind: 'scroll', area, step: 'top' });
    }
    // A page uses `frame_tenths` from the state, never an effect.
    const page = applyFrame(
      initialControl,
      frame(1, 1, scroll({ op: 'scroll', area: 'page', step: 'page_down' }), picked),
      ctx(1),
    );
    expect(page.effect).toEqual({ kind: 'none' });
    expect(page.state.remote.frame_tenths).toBe(20);
  });

  test('shown_remote_is_the_default_for_another_generation', () => {
    const out = applyFrame(initialControl, frame(1, 2, scroll(down), picked), ctx(2));
    expect(shownRemote(out.state, 2)).toEqual(picked);
    expect(shownRemote(out.state, 3)).toEqual(rest);
    expect(shownRemote(out.state, null)).toEqual(rest);
    expect(shownRemote(initialControl, 0)).toEqual(rest);
  });
});

describe('the selection', () => {
  const items = ['a/1', 'a/2', 'b/3'];

  test('step_selection_wraps_both_ways', () => {
    expect(stepSelection(items, 'a/1', 'next')).toBe('a/2');
    expect(stepSelection(items, 'b/3', 'next')).toBe('a/1');
    expect(stepSelection(items, 'a/1', 'previous')).toBe('b/3');
    expect(stepSelection(items, 'b/3', 'previous')).toBe('a/2');
    // No selection, or one that is not in the list: the first (next) or the last (previous).
    expect(stepSelection(items, null, 'next')).toBe('a/1');
    expect(stepSelection(items, null, 'previous')).toBe('b/3');
    expect(stepSelection(items, 'gone/9', 'next')).toBe('a/1');
    expect(stepSelection(items, 'gone/9', 'previous')).toBe('b/3');
  });

  test('step_selection_without_items_is_null', () => {
    expect(stepSelection([], null, 'next')).toBeNull();
    expect(stepSelection([], 'a/1', 'previous')).toBeNull();
  });

  test('selectable_lists_needs_then_tiles_without_repeats', () => {
    const needs = ['b/2', 'a/1'];
    const tiles = ['a/1', 'c/3', 'b/2', 'd/4'];
    const columns = ['x/1', 'x/2'];
    expect(selectable({ mode: 'mission' }, needs, tiles, columns)).toEqual([
      'b/2',
      'a/1',
      'c/3',
      'd/4',
    ]);
    expect(selectable({ mode: 'race' }, needs, tiles, columns)).toEqual(['x/1', 'x/2']);
    for (const mode of ['focus', 'review', 'artifact', 'ambient'] as const) {
      expect(selectable({ mode }, needs, tiles, columns)).toEqual([]);
    }
  });
});
