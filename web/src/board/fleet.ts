import { basename } from '../app/sessions';
import type { CardStatus, DeckSession, FileConflict, ProviderKind, StationView } from '../gen';
import { sessionKey } from '../runtime/state';
import { sessionRef } from '../ui/localSet';

export type FleetFilter = {
  readonly station: string;
  readonly provider: string;
  readonly repo: string;
  readonly search: string;
};
export const ALL = '';
export const emptyFilter: FleetFilter = { station: ALL, provider: ALL, repo: ALL, search: '' };

/** The repo name of a session without a repository. */
export const NO_REPO = 'Outside a repository';

/** `git@github.com:org/app.git` and `https://github.com/org/app.git` → `org/app`; no origin → the root's folder. */
export function repoName(card: DeckSession): string {
  const repo = card.summary.repo;
  if (repo === null || repo === undefined) return NO_REPO;
  if (repo.origin !== null && repo.origin !== '') {
    const parts = repo.origin
      .replace(/\.git$/, '')
      .split(/[/:]/)
      .filter((p) => p !== '');
    if (parts.length > 0) return parts.slice(-2).join('/');
  }
  return basename(repo.root);
}

/** The distinct repo names of `sessions`, sorted, with "Outside a repository" last. */
export function repoNames(sessions: readonly DeckSession[]): string[] {
  const names = [...new Set(sessions.map(repoName))];
  return names.sort((a, b) =>
    a === b ? 0 : a === NO_REPO ? 1 : b === NO_REPO ? -1 : a.localeCompare(b),
  );
}

/** An ended or lost session that the owner cleared is hidden; a session in any other status shows. */
export function archivedHidden(card: DeckSession, refs: ReadonlySet<string>): boolean {
  return (
    (card.status === 'ended' || card.status === 'lost') &&
    refs.has(sessionRef(card.station_id, card.summary.session_id))
  );
}

/** Who a session shares files with: the other sessions' titles and the files. */
export type Contested = { readonly with: readonly string[]; readonly files: readonly string[] };

/** The contested sessions of a conflict list, by `sessionRef`. */
export function contestedBy(list: readonly FileConflict[]): ReadonlyMap<string, Contested> {
  const found = new Map<string, { with: Set<string>; files: Set<string> }>();
  for (const conflict of list) {
    for (const touch of conflict.touches) {
      const ref = sessionRef(touch.station_id, touch.session_id);
      const entry = found.get(ref) ?? { with: new Set<string>(), files: new Set<string>() };
      let others = 0;
      for (const other of conflict.touches) {
        if (other.station_id === touch.station_id && other.session_id === touch.session_id)
          continue;
        entry.with.add(other.session_title ?? other.session_id);
        others += 1;
      }
      if (others > 0) {
        entry.files.add(conflict.file);
        found.set(ref, entry);
      }
    }
  }
  return new Map(
    [...found].map(([ref, e]) => [ref, { with: [...e.with], files: [...e.files] }] as const),
  );
}

const labels = {
  needs_you: 'Needs you',
  queued: 'Queued',
  working: 'Working',
  verifying: 'Verifying',
  idle: 'Idle',
  ended: 'Ended',
  unreachable: 'Unreachable',
  lost: 'Lost',
} as const satisfies Record<CardStatus, string>;
export const STATUS_LANES = (Object.keys(labels) as CardStatus[]).map((status) => ({
  status,
  label: labels[status],
}));
export const cardKey = (card: DeckSession): string =>
  sessionKey({ station_id: card.station_id, session_id: card.summary.session_id });
export function stationName(id: string, stations: readonly StationView[]): string {
  return stations.find((station) => station.station_id === id)?.name ?? id;
}

/** A live session runs or can run now: not ended, not lost, and its station is reachable (the
 * Watch rule, `watch/tiles.ts`). */
export const isLiveSession = (card: DeckSession): boolean =>
  card.status !== 'ended' && card.status !== 'lost' && card.status !== 'unreachable';

/** The count of the header sentences (fleet home, Watch): "1 live session", "4 live sessions". */
export const liveSessionsText = (n: number): string =>
  n === 1 ? '1 live session' : `${n} live sessions`;

/** Changes when any session changes, so derived panels load again. */
export function fleetVersion(sessions: readonly DeckSession[]): string {
  let newest = 0;
  for (const s of sessions) newest = Math.max(newest, s.summary.updated_at);
  return `${sessions.length}/${newest}`;
}

/** Filters never change the source projections or the cached runtime state. */
export function filterFleet(
  sessions: readonly DeckSession[],
  stations: readonly StationView[],
  filter: FleetFilter,
): DeckSession[] {
  const search = filter.search.trim().toLowerCase();
  return sessions.filter((card) => {
    const s = card.summary;
    if (filter.station && card.station_id !== filter.station) return false;
    if (filter.provider && (s.provider ?? 'unknown') !== filter.provider) return false;
    if (filter.repo && repoName(card) !== filter.repo) return false;
    const text = [
      s.title,
      s.session_id,
      card.station_id,
      stationName(card.station_id, stations),
      s.cwd,
      s.model,
      s.provider_instance,
    ].join('\n');
    return !search || text.toLowerCase().includes(search);
  });
}

/** Use the deck status, not the last station activity: link loss can change it. */
export function statusGroups(sessions: readonly DeckSession[]): Record<CardStatus, DeckSession[]> {
  const groups: Record<CardStatus, DeckSession[]> = {
    needs_you: [],
    queued: [],
    working: [],
    verifying: [],
    idle: [],
    ended: [],
    unreachable: [],
    lost: [],
  };
  for (const card of sessions) groups[card.status].push(card);
  for (const { status } of STATUS_LANES) {
    groups[status].sort(
      (a, b) => b.summary.updated_at - a.summary.updated_at || cardKey(a).localeCompare(cardKey(b)),
    );
  }
  return groups;
}

/**
 * When each session came into its status: a rising number, for one card (by `cardKey`). A session
 * keeps its place in its status group while it works (each tool call changes its `updated_at`), and
 * moves only when its status changes; a session that comes into a status goes to the top of its
 * group. Linear never moves a row under the pointer: a click lands on the row that the eye chose.
 */
export type Arrivals = {
  last: number;
  readonly of: Map<string, { readonly status: CardStatus; readonly seq: number }>;
};

export const newArrivals = (): Arrivals => ({ last: 0, of: new Map() });

/**
 * The arrivals of the board for the page life: one record for the fleet lanes, the sidebar rows and
 * the j/k keys, so they agree. A render only reads it; the app and the fleet board write it after a
 * render (`rememberArrivals`).
 */
export const BOARD_ARRIVALS = newArrivals();

/** Forgets the arrivals of the board (tests: every test starts as a fresh board). */
export function resetBoardArrivals(): void {
  BOARD_ARRIVALS.last = 0;
  BOARD_ARRIVALS.of.clear();
}

/**
 * The order of two sessions of one status: the newest arrival first; a session that has no arrival
 * in its status yet (it just came into it) before the others. 0 when neither has one: a stable sort
 * then keeps the newest-first order of `statusGroups`.
 */
export function byArrival(arrivals: Arrivals): (a: DeckSession, b: DeckSession) => number {
  const seq = (card: DeckSession): number | null => {
    const seen = arrivals.of.get(cardKey(card));
    return seen !== undefined && seen.status === card.status ? seen.seq : null;
  };
  return (a, b) => {
    const x = seq(a);
    const y = seq(b);
    if (x === y) return 0;
    if (x === null) return -1;
    if (y === null) return 1;
    return y - x;
  };
}

/**
 * Gives every session that has no arrival in its status one, in the order that the board shows
 * (`byArrival`), so the next render keeps the places: the top session of a group gets the highest
 * number.
 */
export function rememberArrivals(cards: readonly DeckSession[], arrivals: Arrivals): void {
  const order = byArrival(arrivals);
  for (const group of Object.values(statusGroups(cards))) {
    for (const card of [...group].sort(order).reverse()) {
      const key = cardKey(card);
      if (arrivals.of.get(key)?.status === card.status) continue;
      arrivals.last += 1;
      arrivals.of.set(key, { status: card.status, seq: arrivals.last });
    }
  }
}

/** Observed session data is not the configured provider inventory. */
export function observedProviders(sessions: readonly DeckSession[], station: string): string[] {
  const providers = new Set<string>();
  for (const card of sessions) {
    if (card.station_id !== station) continue;
    const s = card.summary;
    providers.add(
      `${providerText(s.provider)} · ${s.provider_instance ?? 'instance unknown'} · ${s.provider_version ?? 'version unknown'}`,
    );
  }
  return [...providers].sort();
}
function providerText(kind: ProviderKind | null): string {
  return kind === 'claude' ? 'Claude' : kind === 'codex' ? 'Codex' : 'Unknown';
}
export function lastSeen(value: number | null): string {
  if (value === null) return 'Never';
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? 'Unknown' : date.toISOString();
}
export function usageText(card: DeckSession): string {
  const usage = card.summary.usage;
  return `Input ${usage.input_tokens ?? 'unknown'} · Output ${usage.output_tokens ?? 'unknown'} · Cost ${usage.cost_usd === null ? 'unknown' : `${usage.cost_inferred ? '≈ ' : ''}$${usage.cost_usd.toFixed(4)}${usage.cost_inferred ? ' inferred' : ''}`}`;
}
