// B17 §3.1.5: the pure parts of the wall controls. A frame of the deck relay (`wall_control`) goes
// through `applyFrame`: it gives the new control state, the scroll effect and whether the wall must
// read its directive again. Nothing here touches the document except `scrollBehavior`.
import type { WallRoute } from '../app/routes';
import type { ScrollStep, WallArea, WallControlFrame, WallRemoteState } from '../gen';

/** How long auto-focus and the ambient timer wait after a control or a navigation (ms). */
export const REMOTE_HOLD_MS = 120_000;
/** The scroll range of a frame with the long fit, in tenths of a screen. */
export const FRAME_TENTHS_MAX = 70;
/** A long-fit frame is this many times as tall as its pane. */
export const LONG_FIT_SCREENS = 8;

export type ControlState = {
  /** The relay of the last frame (`null` before the first). */
  readonly incarnation: string | null;
  /** The last `navigate` or `control` seq of this incarnation (0 before the first). */
  readonly seq: number;
  /** The generation of `remote` (`null` before the first frame). */
  readonly generation: number | null;
  readonly remote: WallRemoteState;
  /** Unix ms until which auto-focus and the ambient timer wait. */
  readonly holdUntil: number;
};

const NO_REMOTE: WallRemoteState = { selected: null, frame_tenths: 0 };

export const initialControl: ControlState = {
  incarnation: null,
  seq: 0,
  generation: null,
  remote: NO_REMOTE,
  holdUntil: 0,
};

export type ControlEffect =
  | { readonly kind: 'none' }
  | { readonly kind: 'scroll'; readonly area: WallArea; readonly step: ScrollStep };

const NONE: ControlEffect = { kind: 'none' };

/** The areas that scroll by a relative effect. `page` uses `frame_tenths` (the page pane, B3); an
 * HTML artifact uses it too, and a Markdown artifact scrolls by the effect. */
const RELATIVE_AREAS: readonly WallArea[] = ['needs', 'stream', 'chat', 'changes', 'artifact'];

/**
 * The selectable refs, in reading order: mission → the needs items, then the tiles (no repeats);
 * race → the columns; every other mode → none. A ref is `station/session`.
 */
export function selectable(
  route: Pick<WallRoute, 'mode'>,
  needs: readonly string[],
  tiles: readonly string[],
  columns: readonly string[],
): readonly string[] {
  if (route.mode === 'mission') return [...new Set([...needs, ...tiles])];
  if (route.mode === 'race') return [...new Set(columns)];
  return [];
}

/**
 * The next or previous ref of `items`, with wrap. No selection (or a selection that is not in
 * `items`) gives the first (next) or the last (previous); empty `items` gives `null`.
 */
export function stepSelection(
  items: readonly string[],
  selected: string | null,
  dir: 'next' | 'previous',
): string | null {
  if (items.length === 0) return null;
  const at = selected === null ? -1 : items.indexOf(selected);
  if (at < 0) return (dir === 'next' ? items[0] : items[items.length - 1]) ?? null;
  const by = dir === 'next' ? 1 : -1;
  return items[(at + by + items.length) % items.length] ?? null;
}

/** The remote state that the wall draws: `state.remote` when `state.generation` is the shown
 * generation, else the default. */
export function shownRemote(state: ControlState, shown: number | null): WallRemoteState {
  return state.generation !== null && state.generation === shown ? state.remote : NO_REMOTE;
}

/** `station/session` of a selected session, as `data-ref` writes it. */
export const refOfSelected = (state: WallRemoteState): string | null =>
  state.selected === null ? null : `${state.selected.station_id}/${state.selected.session_id}`;

/** True while a remote hold stops auto-focus (the hold counts as a pin). */
export const holding = (holdUntil: number, now: number): boolean => now < holdUntil;

export function applyFrame(
  state: ControlState,
  frame: WallControlFrame,
  ctx: { readonly shown: number | null; readonly now: number },
): {
  readonly state: ControlState;
  readonly effect: ControlEffect;
  readonly reread: boolean;
} {
  const nothing = { state, effect: NONE, reread: false } as const;
  const event = frame.event;
  const fresh = frame.incarnation !== state.incarnation;
  // Rule 1: a new incarnation is accepted, and `seq` restarts from the frame.
  const known = fresh ? 0 : state.seq;
  const counts = event.kind !== 'sync';
  // Rule 2: a repeat of the same incarnation does nothing.
  if (counts && !fresh && frame.seq <= known) return nothing;
  // Rule 3: a control for an old view never acts on a new one.
  if (ctx.shown !== null && frame.generation < ctx.shown) return nothing;
  const seq = counts ? frame.seq : Math.max(known, frame.seq);
  const holdUntil = counts ? ctx.now + REMOTE_HOLD_MS : state.holdUntil;
  const base = { incarnation: frame.incarnation, seq, generation: frame.generation, holdUntil };
  // Rule 4: a newer generation than the one shown: keep the state, read the directive at once.
  if (ctx.shown === null || frame.generation > ctx.shown) {
    return {
      state: { ...base, remote: frame.state },
      effect: NONE,
      reread: true,
    };
  }
  // Rule 5: the shown generation.
  const effect: ControlEffect =
    event.kind === 'control' &&
    event.control.op === 'scroll' &&
    RELATIVE_AREAS.includes(event.control.area)
      ? { kind: 'scroll', area: event.control.area, step: event.control.step }
      : NONE;
  return { state: { ...base, remote: frame.state }, effect, reread: false };
}

/* ── the DOM side, kept small and testable with plain objects ─────────────────────────────── */

/** What a scroll needs from an element. */
export type ScrollBox = {
  readonly scrollTop: number;
  readonly clientHeight: number;
  readonly scrollHeight: number;
  scrollTo(options: { top: number; behavior: ScrollBehavior }): void;
};

/** `line_*` move 10 % of the height, `page_*` 90 %, `top` goes to 0 and `bottom` to the end. */
export function scrollTopFor(step: ScrollStep, box: Omit<ScrollBox, 'scrollTo'>): number {
  const end = Math.max(0, box.scrollHeight - box.clientHeight);
  const line = box.clientHeight * 0.1;
  const page = box.clientHeight * 0.9;
  const top = (() => {
    switch (step) {
      case 'line_up':
        return box.scrollTop - line;
      case 'line_down':
        return box.scrollTop + line;
      case 'page_up':
        return box.scrollTop - page;
      case 'page_down':
        return box.scrollTop + page;
      case 'top':
        return 0;
      case 'bottom':
        return end;
    }
  })();
  return Math.min(end, Math.max(0, top));
}

/** The element of a scroll area under `root`: `data-wall-scroll="<area>"`. */
export const scrollSelector = (area: WallArea): string => `[data-wall-scroll="${area}"]`;

/** Scrolls the element of the area; false when the wall does not show the area now. */
export function applyScroll(
  root: { querySelector(selector: string): ScrollBox | null },
  effect: ControlEffect,
  behavior: ScrollBehavior,
): boolean {
  if (effect.kind !== 'scroll') return false;
  const box = root.querySelector(scrollSelector(effect.area));
  if (box === null) return false;
  box.scrollTo({ top: scrollTopFor(effect.step, box), behavior });
  return true;
}

/** The absolute position of a long-fit frame: `frame_tenths` tenths of the pane height. */
export const longFrameTop = (tenths: number, clientHeight: number): number =>
  (Math.min(FRAME_TENTHS_MAX, Math.max(0, tenths)) * clientHeight) / 10;

/** True when the text of a scroll box shows its last line. */
export const atEnd = (
  box: Pick<ScrollBox, 'scrollTop' | 'clientHeight' | 'scrollHeight'>,
): boolean => box.scrollHeight - box.scrollTop - box.clientHeight <= 2;

/** Smooth, unless the board preference Reduce motion or the OS asks for less (DESIGN-SYSTEM §2.5). */
export function scrollBehavior(): ScrollBehavior {
  if (typeof document === 'undefined') return 'auto';
  const reduce =
    document.documentElement.dataset['motion'] === 'reduce' ||
    (typeof window !== 'undefined' &&
      window.matchMedia?.('(prefers-reduced-motion: reduce)').matches === true);
  return reduce ? 'auto' : 'smooth';
}

/** What `markSelected` needs from an element. */
export type Marked = {
  getAttribute(name: string): string | null;
  setAttribute(name: string, value: string): void;
  removeAttribute(name: string): void;
};

/**
 * Sets `data-selected="true"` on every element under `root` whose `data-ref` is `selected`, and
 * removes it from the others. A selected ref that the wall does not show marks nothing.
 */
export function markSelected(
  root: { querySelectorAll(selector: string): ArrayLike<Marked> },
  selected: string | null,
): void {
  const all = root.querySelectorAll('[data-ref]');
  for (let i = 0; i < all.length; i += 1) {
    const el = all[i] as Marked;
    if (selected !== null && el.getAttribute('data-ref') === selected) {
      if (el.getAttribute('data-selected') !== 'true') el.setAttribute('data-selected', 'true');
    } else if (el.getAttribute('data-selected') !== null) {
      el.removeAttribute('data-selected');
    }
  }
}

/**
 * The key listener of the wall: the capture listener on `target` (the document). A mirror (the
 * Remote page, B17) has none: it returns at once, so its keys reach the page. Returns the function
 * that removes the listener.
 */
export function listenKeys(
  target: Pick<Document, 'addEventListener' | 'removeEventListener'>,
  mirror: boolean,
  onKey: (event: KeyboardEvent) => void,
): () => void {
  if (mirror) return () => {};
  target.addEventListener('keydown', onKey, true);
  return () => target.removeEventListener('keydown', onKey, true);
}
