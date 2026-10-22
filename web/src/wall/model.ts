// R13 P1: the pure parts of the wall that are not shapes: the filter, the needs-you list, the
// mode cycle and the counts of the ambient view.
import { WALL_MODES, type WallMode, type WallRoute } from '../app/routes';
import { repoName } from '../board/fleet';
import { type RateMeter, scopeText } from '../board/usage';
import type { DeckSession, OpenRequest, RequestKind } from '../gen';
import type { ReviewState } from '../review/useReview';
import { emptyWatchFilter, refOf, watchSessions } from '../watch/tiles';
import type { Ask } from './autofocus';

/** The sessions that pass the station and repository filter of the route. */
export function wallSessions(
  sessions: readonly DeckSession[],
  route: Pick<WallRoute, 'stations' | 'repo'>,
): readonly DeckSession[] {
  return sessions.filter(
    (c) =>
      (route.stations.length === 0 || route.stations.includes(c.station_id)) &&
      (route.repo === '' || repoName(c) === route.repo),
  );
}

/** A session with a live process has requests that matter; an ended one has none that count. */
const takesAnswer = (c: DeckSession): boolean =>
  c.status !== 'ended' && c.status !== 'lost' && c.status !== 'unreachable';

export type Need = {
  readonly card: DeckSession;
  readonly request: OpenRequest;
};

/** The open requests of the filtered sessions, oldest first. */
export function needsList(sessions: readonly DeckSession[]): readonly Need[] {
  const out: Need[] = [];
  for (const card of sessions) {
    if (!takesAnswer(card)) continue;
    for (const request of card.summary.open_requests) out.push({ card, request });
  }
  return out.sort(
    (a, b) =>
      a.request.requested_at - b.request.requested_at ||
      a.request.request_id.localeCompare(b.request.request_id),
  );
}

/** The badge of a request on the wall: what kind of answer it waits for. */
export const NEED_LABEL: Readonly<Record<RequestKind, string>> = {
  question: 'Question',
  command: 'Permission',
  file_change: 'Permission',
  tool: 'Permission',
};

/** The usage line of the wall: "Claude 5-hour window · 62%" (no raw scope key). */
export function usageLine(meter: RateMeter): string {
  return `${meter.provider} ${scopeText(meter.scope)} · ${Math.round(meter.usedPercent ?? 0)}%`;
}

/** The requests as the auto-focus function reads them. */
export const asksOf = (needs: readonly Need[]): readonly Ask[] =>
  needs.map((n) => ({
    ref: refOf(n.card),
    requestId: n.request.request_id,
    at: n.request.requested_at,
  }));

/** The first line of a text, cut to `max` characters. */
export function oneLine(text: string, max = 120): string {
  const line =
    text
      .split('\n')
      .find((l) => l.trim() !== '')
      ?.trim() ?? '';
  return line.length <= max ? line : `${line.slice(0, max - 1)}…`;
}

/** The sessions that get a tile, needs-you first (the Watch order). */
export const tileSessions = (sessions: readonly DeckSession[]): readonly DeckSession[] =>
  watchSessions(sessions, emptyWatchFilter);

export type ModeTargets = {
  /** The session that `focus` and `review` use when the route has none. */
  readonly session: { readonly station: string; readonly id: string } | null;
};

/**
 * The route after `←` (-1) or `→` (+1): the next mode in the cycle that has a target. `focus` and
 * `review` use the route session, then `fallback` (the focused tile, else the first tile); `race`
 * and `artifact` need their target in the route; `mission` and `ambient` always work.
 */
export function stepMode(
  route: WallRoute,
  by: 1 | -1,
  fallback: { readonly station: string; readonly id: string } | null,
): WallRoute {
  const at = WALL_MODES.indexOf(route.mode);
  for (let i = 1; i <= WALL_MODES.length; i += 1) {
    const mode: WallMode =
      WALL_MODES[(at + by * i + WALL_MODES.length * 2) % WALL_MODES.length] ?? 'mission';
    const target = withMode(route, mode, fallback);
    if (target !== null) return target;
  }
  return route;
}

/** `route` in `mode`, or `null` when the mode has no target. */
export function withMode(
  route: WallRoute,
  mode: WallMode,
  fallback: { readonly station: string; readonly id: string } | null,
): WallRoute | null {
  if (mode === 'focus' || mode === 'review') {
    const session = route.session ?? fallback;
    return session === null ? null : { ...route, mode, session };
  }
  if (mode === 'race') return route.batch === undefined ? null : { ...route, mode };
  if (mode === 'artifact') return route.artifact === undefined ? null : { ...route, mode };
  return { ...route, mode };
}

const sameDay = (a: number, b: number): boolean => {
  const x = new Date(a);
  const y = new Date(b);
  return (
    x.getFullYear() === y.getFullYear() &&
    x.getMonth() === y.getMonth() &&
    x.getDate() === y.getDate()
  );
};

/** The counts of the ambient view. "Done today": ended sessions whose last update is today. */
export function ambientCounts(
  sessions: readonly DeckSession[],
  now: number,
): { readonly running: number; readonly needs: number; readonly done: number } {
  let running = 0;
  let needs = 0;
  let done = 0;
  for (const c of sessions) {
    if (c.status === 'working' || c.status === 'verifying') running += 1;
    if (c.status === 'needs_you') needs += 1;
    if (c.status === 'ended' && sameDay(c.summary.updated_at, now)) done += 1;
  }
  return { running, needs, done };
}

/** A string that changes when the fleet read model changes (the ambient idle timer reads it). */
export function fleetSignature(sessions: readonly DeckSession[]): string {
  return sessions
    .map(
      (c) =>
        `${refOf(c)}|${c.status}|${c.summary.updated_at}|${c.summary.open_requests
          .map((r) => r.request_id)
          .join(',')}`,
    )
    .join('\n');
}

/**
 * The review state with the comments that were sent and are open: a draft is the owner's note (a
 * thread that starts with one is dropped, and so is a draft reply), and a resolved thread folds
 * behind a button that the wall does not have.
 */
export function sentOnly(state: ReviewState): ReviewState {
  if (state.data === null) return state;
  return {
    ...state,
    data: {
      ...state.data,
      threads: state.data.threads
        .filter((t) => t.comments[0]?.state !== 'draft' && t.comments[0]?.resolved !== true)
        .map((t) => ({ ...t, comments: t.comments.filter((c) => c.state !== 'draft') })),
    },
  };
}
