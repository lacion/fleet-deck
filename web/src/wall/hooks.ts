// R13 P1: the hooks of the wall: the timeline subscriptions (the same code as Watch), the size of
// an element, and the session diff.
import { type RefObject, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { DeckSession, SessionDiff, SessionKey } from '../gen';
import type { Timeline } from '../runtime/state';
import type { FileActions } from '../session/workspace';
import { refOf } from '../watch/tiles';

/** The wait between two reads of the session diff of an open pane. */
export const DIFF_REFRESH_MS = 20_000;

export const keyOf = (c: DeckSession): SessionKey => ({
  station_id: c.station_id,
  session_id: c.summary.session_id,
});

export const timelineOf = (
  timelines: Readonly<Record<string, Timeline>> | undefined,
  key: SessionKey,
): Timeline | undefined => timelines?.[JSON.stringify([key.station_id, key.session_id])];

/**
 * Opens the live timeline of each session (the subscription code of Watch). A session whose
 * subscription throws (more timelines than the socket allows) is in the returned set.
 */
export function useTimelines(
  cards: readonly DeckSession[],
  watch: ((key: SessionKey) => () => void) | undefined,
): ReadonlySet<string> {
  const refs = cards.map(refOf).join('\n');
  const keys = useMemo(
    () => cards.map(keyOf),
    // `refs` stands for the content of `cards`.
    [refs],
  );
  const [failed, setFailed] = useState<ReadonlySet<string>>(() => new Set());
  useEffect(() => {
    if (watch === undefined) return undefined;
    const cancels: (() => void)[] = [];
    const bad = new Set<string>();
    for (const key of keys) {
      try {
        cancels.push(watch(key));
      } catch {
        bad.add(`${key.station_id}/${key.session_id}`);
      }
    }
    setFailed((now) => (now.size === bad.size && [...bad].every((r) => now.has(r)) ? now : bad));
    return () => {
      for (const cancel of cancels) cancel();
    };
  }, [watch, keys]);
  return failed;
}

/** The grid gap of the first `selector` element inside `el`, in px; 0 when none or `normal`. */
function gapOf(el: HTMLElement, selector: string | undefined): number {
  const grid = selector === undefined ? null : el.querySelector(selector);
  if (grid === null) return 0;
  // The tiles grid has one `gap`: the two values are the same.
  const gap = Number.parseFloat(getComputedStyle(grid).columnGap);
  return Number.isFinite(gap) ? gap : 0;
}

/**
 * The size of an element, measured at once and on every resize; `null` before the first read.
 * `gap` is the grid gap of the `gridSelector` element inside it (0 without a selector).
 */
export function useSize(ref: RefObject<HTMLElement | null>, on = true, gridSelector?: string) {
  const [size, setSize] = useState<{
    readonly w: number;
    readonly h: number;
    readonly gap: number;
  } | null>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (el === null || !on) return undefined;
    const read = () =>
      setSize((now) => {
        const gap = gapOf(el, gridSelector);
        return now !== null &&
          now.w === el.clientWidth &&
          now.h === el.clientHeight &&
          now.gap === gap
          ? now
          : { w: el.clientWidth, h: el.clientHeight, gap };
      });
    read();
    if (typeof ResizeObserver === 'undefined') return undefined;
    const observer = new ResizeObserver(read);
    observer.observe(el);
    return () => observer.disconnect();
  }, [ref, on, gridSelector]);
  return size;
}

export type DiffState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'ready'; readonly diff: SessionDiff };

/** The "Since the session started" diff of a session; it reads again every `DIFF_REFRESH_MS`. */
export function useSessionDiff(files: FileActions | null, ref: string): DiffState {
  const [state, setState] = useState<DiffState>({ kind: 'loading' });
  const latest = useRef(files);
  latest.current = files;
  const has = files !== null;
  useEffect(() => {
    if (!has) return undefined;
    let live = true;
    setState({ kind: 'loading' });
    const load = () => {
      latest.current?.sessionDiff().then(
        (diff) => live && setState({ kind: 'ready', diff }),
        (e: unknown) =>
          live &&
          setState((now) =>
            now.kind === 'ready'
              ? now
              : {
                  kind: 'error',
                  text: e instanceof Error ? e.message : 'The diff request failed.',
                },
          ),
      );
    };
    load();
    const timer = setInterval(load, DIFF_REFRESH_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
    // `ref` stands for the session.
  }, [has, ref]);
  return state;
}
