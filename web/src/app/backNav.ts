// The phone's Back of a session pane: go back in the history while the current entry is not the
// first one of this board start, else (a deep link or a reload) go to the home list.
//
// Every history entry of the board carries a position `fdPos` in its state. The entry that the
// board starts on has position 0; each new entry (a hash change without `fdPos`) gets the position
// of the previous entry plus one. Browser Back and Forward restore the state of their entry, so
// the position always says how far the current entry is from the first one.

const KEY = 'fdPos';

/** The part of `History` that the position logic uses (a fake in the unit test). */
export type HistoryLike = Pick<History, 'state' | 'replaceState'>;

let position = 0;

/** The history of the page; none without a window (server rendering, unit tests). */
function browserHistory(): HistoryLike | undefined {
  return typeof window === 'undefined' ? undefined : window.history;
}

function stateRecord(h: HistoryLike): Record<string, unknown> {
  const state: unknown = h.state;
  return typeof state === 'object' && state !== null ? { ...state } : {};
}

function positionOf(state: Record<string, unknown>): number | null {
  const pos = state[KEY];
  return typeof pos === 'number' && Number.isFinite(pos) ? pos : null;
}

/** The board starts: the current entry is position 0 (also after a reload). */
export function startBackNav(h: HistoryLike = window.history): void {
  h.replaceState({ ...stateRecord(h), [KEY]: 0 }, '');
  position = 0;
}

/** A hash change (App calls it on every one): an entry without a position gets the next one. */
export function noteNavigation(h: HistoryLike = window.history): void {
  const state = stateRecord(h);
  const known = positionOf(state);
  if (known === null) {
    position += 1;
    h.replaceState({ ...state, [KEY]: position }, '');
  } else {
    position = known;
  }
}

/** True when the current entry is not the first one, so `history.back()` stays inside the board. */
export function canGoBack(h: HistoryLike | undefined = browserHistory()): boolean {
  return h !== undefined && (positionOf(stateRecord(h)) ?? 0) > 0;
}

/** Tests only: forget the position. */
export function resetBackNav(): void {
  position = 0;
}
