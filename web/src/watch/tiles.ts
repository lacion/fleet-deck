// R6.2 (DL-169): the watch page, the first explored form of "watch many sessions" (DL-157).
// A wall of compact live tiles: the model of one tile and of the page. Nothing here is
// shared with the rest of the board, so the owner can pick another form later.
import { repoName } from '../board/fleet';
import type { DeckSession, EventEnvelope, OpenRequest } from '../gen';
import { buildTimeline, type Item } from '../session/timeline';
import { questionForm } from '../session/question';

/** The most tiles; one timeline subscription each, and a socket holds 32 (the rest is spare). */
export const MAX_TILES = 24;
/** The lines of chat that a tile shows. */
export const TILE_LINES = 4;
/** How long Focus stays on one session before it moves to the next one that needs you. */
export const FOCUS_MS = 20_000;
/** The longest line, in characters. */
export const LINE_CHARS = 140;

export const ALL = '';

/** A session with a live process: a hibernated one has none, so it has no tile. */
const isLive = (card: DeckSession): boolean =>
  card.status !== 'ended' && card.status !== 'lost' && card.status !== 'unreachable';

export type WatchFilter = {
  /** A station id, or `ALL`. */
  readonly station: string;
  /** A repository name (`repoName`), or `ALL`. */
  readonly repo: string;
  readonly needsOnly: boolean;
};

export const emptyWatchFilter: WatchFilter = { station: ALL, repo: ALL, needsOnly: false };

export type TileLine = {
  readonly key: string;
  readonly kind: 'message' | 'tool';
  readonly text: string;
};

const cut = (text: string): string => {
  const line =
    text
      .split('\n')
      .map((l) => l.trim())
      .find((l) => l !== '') ?? '';
  return line.length <= LINE_CHARS ? line : `${line.slice(0, LINE_CHARS - 1)}…`;
};

function lineOf(item: Item): TileLine | null {
  if (item.type === 'message') {
    const text = cut(item.text);
    return text === '' ? null : { key: item.key, kind: 'message', text };
  }
  if (item.type === 'tool') {
    const call = item.input === '' ? item.title : `${item.title}: ${item.input}`;
    const mark = item.status === 'failed' || item.status === 'denied' ? ` (${item.status})` : '';
    return { key: item.key, kind: 'tool', text: cut(call) + mark };
  }
  if (item.type === 'subagent') {
    return { key: item.key, kind: 'tool', text: cut(`Subagent: ${item.title}`) };
  }
  return null;
}

/** The last `max` agent text and tool lines of a timeline, oldest first. */
export function tileLines(
  events: readonly EventEnvelope[],
  max: number = TILE_LINES,
): readonly TileLine[] {
  const lines = buildTimeline(events)
    .items.map(lineOf)
    .filter((l): l is TileLine => l !== null);
  return lines.slice(-max);
}

/** The sessions that a tile can show: live ones that pass the filter, needs-you first. */
export function watchSessions(
  sessions: readonly DeckSession[],
  filter: WatchFilter,
): readonly DeckSession[] {
  const rank = (c: DeckSession) => (c.status === 'needs_you' ? 0 : 1);
  return sessions
    .filter(isLive)
    .filter((c) => filter.station === ALL || c.station_id === filter.station)
    .filter((c) => filter.repo === ALL || repoName(c) === filter.repo)
    .filter((c) => !filter.needsOnly || c.status === 'needs_you')
    .sort(
      (a, b) =>
        rank(a) - rank(b) ||
        b.summary.updated_at - a.summary.updated_at ||
        a.summary.session_id.localeCompare(b.summary.session_id),
    );
}

export const refOf = (card: DeckSession): string => `${card.station_id}/${card.summary.session_id}`;

/** The sessions that Focus cycles through: those that need you, the oldest request first. */
export function focusOrder(
  sessions: readonly DeckSession[],
  filter: WatchFilter,
): readonly string[] {
  return sessions
    .filter(isLive)
    .filter((c) => filter.station === ALL || c.station_id === filter.station)
    .filter((c) => filter.repo === ALL || repoName(c) === filter.repo)
    .filter((c) => c.status === 'needs_you')
    .sort(
      (a, b) =>
        (a.summary.open_requests[0]?.requested_at ?? 0) -
          (b.summary.open_requests[0]?.requested_at ?? 0) || refOf(a).localeCompare(refOf(b)),
    )
    .map(refOf);
}

/**
 * The session that Focus shows after `current`. With one candidate it stays on it; when
 * `current` is not a candidate any more it starts at the first one; with none it is null.
 */
export function nextFocus(order: readonly string[], current: string | null): string | null {
  if (order.length === 0) return null;
  const at = current === null ? -1 : order.indexOf(current);
  if (at < 0) return order[0] ?? null;
  return order[(at + 1) % order.length] ?? null;
}

/** The request that a tile answers in place, and whether its options are plain buttons. */
export function tileRequest(card: DeckSession): { request: OpenRequest; plain: boolean } | null {
  const request = card.summary.open_requests[0];
  if (request === undefined) return null;
  return { request, plain: questionForm(request) === null };
}

/** The station ids and repository names of the live sessions, for the filter menus. */
export function filterChoices(sessions: readonly DeckSession[]): {
  readonly stations: readonly string[];
  readonly repos: readonly string[];
} {
  const live = sessions.filter(isLive);
  return {
    stations: [...new Set(live.map((c) => c.station_id))].sort(),
    repos: [...new Set(live.map(repoName))].sort(),
  };
}
