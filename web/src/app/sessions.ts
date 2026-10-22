import { stationName } from '../board/fleet';
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
 * The sidebar groups: needs you, then one group per station sorted by name. Starred rows first.
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
    live: own.filter(isLive).sort((a, b) => starFirst(a, b) || rank(a) - rank(b) || byNewest(a, b)),
    ended: own.filter((c) => !isLive(c)).sort((a, b) => starFirst(a, b) || byNewest(a, b)),
  });
  const ids = [
    ...new Set([...stations.map((s) => s.station_id), ...sessions.map((s) => s.station_id)]),
  ];
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
          .filter((g) => query.trim() === '' || g.count > 0)
          .sort((a, b) => a.name.localeCompare(b.name))
      : projectGroups(shown, projects).map((g) => group(g.id, g.name, true, g.cards));
  return {
    needs: shown
      .filter((c) => c.status === 'needs_you')
      .sort((a, b) => starFirst(a, b) || byNewest(a, b)),
    groups,
    archived: matching.filter((c) => isHidden(c, sets.archived)).sort(byNewest),
  };
}

/** The live sessions in the order of the station groups, each once (the j/k order). */
export function sidebarOrder(model: SidebarModel): readonly DeckSession[] {
  return model.groups.flatMap((g) => g.live);
}

/** Sessions for the palette: needs you first, then the newest; at most `max`. */
export function paletteMatches(
  sessions: readonly DeckSession[],
  query: string,
  max = 12,
): readonly DeckSession[] {
  return sessions
    .filter((c) => matchesQuery(c, query))
    .sort(
      (a, b) =>
        Number(b.status === 'needs_you') - Number(a.status === 'needs_you') || byNewest(a, b),
    )
    .slice(0, max);
}
