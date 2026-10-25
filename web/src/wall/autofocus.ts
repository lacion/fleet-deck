// R13 P1 (design §4.4): auto-focus. A new open request in the wall filter takes the wall to
// `focus` on its session for `FOCUS_HOLD_S`, then back to the mode it had. The decision is a pure
// function of the requests, the time and the current view; it follows the request order only.
import type { WallMode } from '../app/routes';

/** How long the wall stays on a session after a new request (seconds). */
export const FOCUS_HOLD_S = 60;

/** One open request of a session in the wall filter. */
export type Ask = {
  /** `<station>/<session>` (`refOf`). */
  readonly ref: string;
  readonly requestId: string;
  /** Unix ms (`requested_at`). */
  readonly at: number;
};

export type AutoState = {
  /** The mode that the wall shows now. */
  readonly mode: WallMode;
  /** The session that `focus` and `review` show, as `<station>/<session>`; `null`: none. */
  readonly session: string | null;
  /** The hold that auto-focus started, with the view to return to. */
  readonly hold: {
    readonly requestId: string;
    readonly ref: string;
    readonly until: number;
    readonly back: { readonly mode: WallMode; readonly session: string | null };
  } | null;
  /** New requests that wait for their turn, oldest first. */
  readonly queue: readonly Ask[];
};

export const initialAuto = (mode: WallMode, session: string | null): AutoState => ({
  mode,
  session,
  hold: null,
  queue: [],
});

/** The identity of a request: `<station>/<session>/<request id>`; an id alone is not unique. */
export const askKey = (a: { readonly ref: string; readonly requestId: string }): string =>
  `${a.ref}/${a.requestId}`;

const byAge = (a: Ask, b: Ask) => a.at - b.at || askKey(a).localeCompare(askKey(b));

const sameQueue = (a: readonly Ask[], b: readonly Ask[]): boolean =>
  a.length === b.length && a.every((ask, i) => askKey(ask) === askKey(b[i] as Ask));

/**
 * The next state. `pin` turns auto-focus off. A request that is in `current` and not in `previous`
 * is new. The hold ends when its request closes or the time ends; the oldest waiting request then
 * starts its own hold. A change of the view by the owner (another mode or session while a hold
 * runs) ends the hold and drops the queue.
 */
export function nextAutoFocus(
  previous: readonly Ask[],
  current: readonly Ask[],
  now: number,
  state: AutoState,
  pin: boolean,
): AutoState {
  if (pin)
    return state.hold === null && state.queue.length === 0
      ? state
      : { ...state, hold: null, queue: [] };
  const open = new Set(current.map(askKey));
  const known = new Set(previous.map(askKey));
  let { mode, session, hold, queue } = state;
  if (hold !== null && (mode !== 'focus' || session !== hold.ref)) {
    hold = null;
    queue = [];
  }
  const waiting = new Set(queue.map(askKey));
  const fresh = current.filter((a) => !known.has(askKey(a)) && !waiting.has(askKey(a)));
  queue = [...queue, ...fresh].filter((a) => open.has(askKey(a))).sort(byAge);
  if (hold !== null && (!open.has(askKey(hold)) || now >= hold.until)) {
    mode = hold.back.mode;
    session = hold.back.session;
    hold = null;
  }
  if (hold === null) {
    const [first, ...rest] = queue;
    if (first !== undefined) {
      hold = {
        requestId: first.requestId,
        ref: first.ref,
        until: now + FOCUS_HOLD_S * 1000,
        back: { mode, session },
      };
      mode = 'focus';
      session = first.ref;
      queue = rest;
    }
  }
  if (
    mode === state.mode &&
    session === state.session &&
    hold === state.hold &&
    sameQueue(queue, state.queue)
  ) {
    return state;
  }
  return { mode, session, hold, queue };
}
