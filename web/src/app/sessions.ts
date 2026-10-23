import { cardKey, stationName } from '../board/fleet';
import type { CardStatus, DeckSession, Project, StationView } from '../gen';
import { projectGroups } from '../integrations/projectGroups';
import { sessionRef } from '../ui/localSet';
import { isHibernated } from '../views/hibernate';

/** Statuses of a session that runs or waits, in the order the sidebar lists them. */
const LIVE_ORDER: readonly CardStatus[] = ['needs_you', 'working', 'verifying', 'queued', 'idle'];

/** A hibernated session is resumable, so it lists with the live ones, after the idle ones (R6.6). */
export const isLive = (card: DeckSession): boolean =>
  LIVE_ORDER.includes(card.status) || isHibernated(card);

/** The name a session goes by: title, then callsign, then session id. */
export const sessionTitle = (card: DeckSession): string =>
  card.summary.title ?? card.summary.callsign ?? card.summary.session_id;

/** Case-insensitive match on title, callsign, session id, cwd, branch and model. */
export function matchesQuery(card: DeckSession, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (q === '') return true;
  const s = card.summary;
  return [s.title, s.callsign, s.session_id, s.cwd, s.branch, s.model].some(
    (field) => field?.toLowerCase().includes(q) === true,
  );
}

/** "now", "5 min", "3 h", "2 d" from a unix-ms time (DESIGN-SYSTEM §5). */
export function relativeAge(updatedAt: number, now: number): string {
  const seconds = Math.max(0, Math.floor((now - updatedAt) / 1000));
  if (seconds < 60) return 'now';
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h`;
  return `${Math.floor(hours / 24)} d`;
}

/** The last path segment of a directory (the title of a shell tab). */
export function basename(path: string): string {
  const parts = path.split('/').filter((p) => p !== '');
  return parts[parts.length - 1] ?? path;
}

const byNewest = (a: DeckSession, b: DeckSession) => b.summary.updated_at - a.summary.updated_at;

/**
 * The order in which the board first saw each session in its status: a row keeps its place while
 * its status stays, and a session that enters a status goes after the ones that were in it. The
 * rows no longer change places on every event of a session (the row under the pointer moved while
 * the owner went to click it). The sessions of one snapshot that are new in their status are
 * numbered the last activity first, so a fresh board opens with the newest rows on top. One record
 * for the whole board: the sidebar rows and the j/k keys follow one order.
 */
const statusSeen = new Map<string, { readonly status: CardStatus; readonly seq: number }>();
let nextSeq = 0;

/** Numbers the sessions that are new in their status, after the ones that the board knows. */
export function noteStatusOrder(cards: readonly DeckSession[]): void {
  const fresh = cards
    .filter((c) => statusSeen.get(cardKey(c))?.status !== c.status)
    .sort((a, b) => byNewest(a, b) || cardKey(a).localeCompare(cardKey(b)));
  for (const c of fresh) statusSeen.set(cardKey(c), { status: c.status, seq: nextSeq++ });
}

const seqOf = (card: DeckSession): number =>
  statusSeen.get(cardKey(card))?.seq ?? Number.MAX_SAFE_INTEGER;

/** The status order of two sessions that `noteStatusOrder` numbered: the first seen first. */
export const byStatusOrder = (a: DeckSession, b: DeckSession): number =>
  seqOf(a) - seqOf(b) || cardKey(a).localeCompare(cardKey(b));

/** Forgets the status order (tests: every test starts as a fresh board). */
export function resetStatusOrder(): void {
  statusSeen.clear();
  nextSeq = 0;
}

/** When a session started to wait: its oldest open request, else its last update. */
const waitingSince = (card: DeckSession): number =>
  card.summary.open_requests.reduce(
    (oldest, r) => Math.min(oldest, r.requested_at),
    card.summary.open_requests[0]?.requested_at ?? card.summary.updated_at,
  );

/** The order of the needs-you sessions: the longest wait first, as the Needs you panel of the
 * fleet home lists its requests (NeedsYouRail). */
const byLongestWait = (a: DeckSession, b: DeckSession) =>
  waitingSince(a) - waitingSince(b) || byNewest(a, b);
const rank = (card: DeckSession) => {
  const at = LIVE_ORDER.indexOf(card.status);
  return at < 0 ? LIVE_ORDER.length : at;
};

export type StationGroup = {
  readonly id: string;
  readonly name: string;
  readonly reachable: boolean;
  readonly count: number;
  readonly live: readonly DeckSession[];
  readonly ended: readonly DeckSession[];
};

export type SidebarModel = {
  readonly needs: readonly DeckSession[];
  readonly groups: readonly StationGroup[];
  /** Ended or lost sessions that the owner cleared; they are not in the groups or the counts. */
  readonly archived: readonly DeckSession[];
};

/** The per-browser sets of the sidebar (DL-141): starred sessions and archived sessions. */
export type SidebarSets = {
  readonly starred: ReadonlySet<string>;
  readonly archived: ReadonlySet<string>;
};

const NO_SETS: SidebarSets = { starred: new Set(), archived: new Set() };

const refOf = (card: DeckSession) => sessionRef(card.station_id, card.summary.session_id);

/** An archived session hides only while it is ended or lost; any other status shows it. */
export const isHidden = (card: DeckSession, archived: ReadonlySet<string>): boolean =>
  (card.status === 'ended' || card.status === 'lost') &&
  !isHibernated(card) &&
  archived.has(refOf(card));

/**
 * The order of the station groups: the stations in the order of the station list, then the other
 * station ids by id. It is the lane order of the fleet board (FleetBoard `laneOrder`), so that the
 * sidebar and the lanes list the stations in one order.
 */
export function stationOrder(ids: readonly string[], stations: readonly StationView[]): string[] {
  const present = new Set(ids);
  const known = stations.map((s) => s.station_id).filter((id) => present.has(id));
  const rest = [...present].filter((id) => !known.includes(id)).sort();
  return [...new Set([...known, ...rest])];
}

/**
 * The sidebar groups: needs you, then one group per station in the lane order of the board
 * (`stationOrder`). Starred rows first; the live rows then by status, and in a status in the
 * order that the board first saw them there (`byStatusOrder`: they keep their places on an
 * event); the ended rows the newest first (they do not change while the owner looks).
 * With `projects` (DL-176) the groups are the projects that hold a shown session, by name, and "No
 * project" last; their `reachable` is always true.
 */
export function sidebarModel(
  sessions: readonly DeckSession[],
  stations: readonly StationView[],
  query: string,
  sets: SidebarSets = NO_SETS,
  projects?: readonly Project[],
): SidebarModel {
  // Every session, also one that the query hides: it keeps its place when the query clears.
  noteStatusOrder(sessions);
  const matching = sessions.filter((c) => matchesQuery(c, query));
  const shown = matching.filter((c) => !isHidden(c, sets.archived));
  const starFirst = (a: DeckSession, b: DeckSession) =>
    Number(sets.starred.has(refOf(b))) - Number(sets.starred.has(refOf(a)));
  const group = (
    id: string,
    name: string,
    reachable: boolean,
    own: readonly DeckSession[],
  ): StationGroup => ({
    id,
    name,
    reachable,
    count: own.length,
    live: own
      .filter(isLive)
      .sort((a, b) => starFirst(a, b) || rank(a) - rank(b) || byStatusOrder(a, b)),
    ended: own.filter((c) => !isLive(c)).sort((a, b) => starFirst(a, b) || byNewest(a, b)),
  });
  // The sidebar lists a needs-you session once, in "Needs you": a group shows only its other rows.
  // A group without such a row would show an empty header, so the list drops it (a station with
  // no session, or a search without a match there). The Stations page still lists every station.
  const showsRows = (g: StationGroup) =>
    g.ended.length > 0 || g.live.some((c) => c.status !== 'needs_you');
  const ids = stationOrder(
    [...stations.map((s) => s.station_id), ...sessions.map((s) => s.station_id)],
    stations,
  );
  const groups =
    projects === undefined
      ? ids
          .map((id) =>
            group(
              id,
              stationName(id, stations),
              stations.find((s) => s.station_id === id)?.reachable === true,
              shown.filter((c) => c.station_id === id),
            ),
          )
          .filter(showsRows)
      : projectGroups(shown, projects)
          .map((g) => group(g.id, g.name, true, g.cards))
          .filter(showsRows);
  return {
    needs: shown
      .filter((c) => c.status === 'needs_you')
      .sort((a, b) => starFirst(a, b) || byLongestWait(a, b)),
    groups,
    archived: matching.filter((c) => isHidden(c, sets.archived)).sort(byNewest),
  };
}

/** The live sessions in the order of the station groups, each once (the j/k order). */
export function sidebarOrder(model: SidebarModel): readonly DeckSession[] {
  // The order of the rows: the "Needs you" group lists those sessions first, once.
  if (model.needs.length === 0) return model.groups.flatMap((g) => g.live);
  return [
    ...model.needs,
    ...model.groups.flatMap((g) => g.live.filter((c) => c.status !== 'needs_you')),
  ];
}

/** The two groups of the palette: the live sessions, then the ended and lost ones. */
export type PaletteGroups = {
  readonly live: readonly DeckSession[];
  readonly ended: readonly DeckSession[];
};

/**
 * The palette order: needs you (the longest wait first, as the Needs you panel), then the running
 * ones (working, verifying, queued), then idle and hibernated, then ended and lost.
 */
const paletteRank = (card: DeckSession): number => {
  if (card.status === 'needs_you') return 0;
  if (card.status === 'working' || card.status === 'verifying' || card.status === 'queued') {
    return 1;
  }
  return isLive(card) ? 2 : 3;
};

/**
 * Sessions for the palette ("Go to session"), in two groups: live, then ended. Inside a rank the
 * newest first; an archived session (`archived`, the cleared set) shows only when the query
 * matches it, after the other ended ones. At most `max` sessions in all.
 */
export function paletteMatches(
  sessions: readonly DeckSession[],
  query: string,
  archived: ReadonlySet<string> = NO_SETS.archived,
  max = 50,
): PaletteGroups {
  const searching = query.trim() !== '';
  const hidden = (c: DeckSession) => isHidden(c, archived);
  const top = sessions
    .filter((c) => matchesQuery(c, query) && (searching || !hidden(c)))
    .sort(
      (a, b) =>
        paletteRank(a) - paletteRank(b) ||
        Number(hidden(a)) - Number(hidden(b)) ||
        (a.status === 'needs_you' && b.status === 'needs_you'
          ? byLongestWait(a, b)
          : byNewest(a, b)),
    )
    .slice(0, max);
  return { live: top.filter(isLive), ended: top.filter((c) => !isLive(c)) };
}
