import { useEffect, useRef, useState } from 'react';
import { stationOrder } from '../app/sessions';
import type {
  CardStatus,
  DeckSession,
  Project,
  SessionExtras,
  SessionKey,
  StationView,
} from '../gen';
import type { Freshness } from '../runtime/state';
import { sessionFlags } from '../session/controller';
import type { SessionActions } from '../session/commands';
import { batchCountOf, batchCounts } from '../compare/compare';
import { Archive, ArchiveRestore, Icon, Plus, RotateCcw, TriangleAlert } from '../ui/icons';
import { archived } from '../ui/archive';
import { priority, sessionRef } from '../ui/localSet';
import { useProjects } from '../integrations/context';
import { projectGroups } from '../integrations/projectGroups';
import { useNow } from '../ui/useNow';
import { type Contested, cardKey, NO_REPO, repoName, stationName, statusGroups } from './fleet';
import { FleetStrip } from './FleetStrip';
import { resourceWarnings } from './resources';

/** How the lanes of the board are made (DL-141, DL-176). */
export type FleetGroup = 'station' | 'repo' | 'project';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly now?: number | undefined;
  /** DL-141: how the lanes are made; the default is by station. */
  readonly group?: FleetGroup | undefined;
  /** DL-141: the contested sessions, by `sessionRef`. */
  readonly contested?: ReadonlyMap<string, Contested> | undefined;
  /** DL-141: the actions of a session, for the Resume all of the offline tools. */
  readonly sessionActions?: ((key: SessionKey) => SessionActions) | undefined;
  /** DL-141: the card extras (mail, touched files), by `sessionRef`. */
  readonly extras?: ReadonlyMap<string, SessionExtras> | undefined;
  /**
   * DESIGN-SYSTEM §4.2, §4.4: the ended and lost strips of each lane fold into a collapsed
   * "Ended" group (the word and a count pill, as in the sidebar) with their Resume all and Archive,
   * and the lane count is the shown live strips. The board-wide "All ended" row then shows only
   * while two or more lanes have ended or lost sessions (one Resume all, Archive and Restore all
   * for every lane).
   * Off: the ended strips stay in their lane, the lane count is every strip, and one offline row
   * for the ended, lost and unreachable sessions closes the board.
   */
  readonly foldEnded?: boolean | undefined;
  /**
   * A station board without a filter: every online station has a lane, also one without a session
   * (its head and "No sessions.", with New session when `onSpawnAt` is given).
   */
  readonly emptyLanes?: boolean | undefined;
  /** Opens New session with this station picked (the New session of an empty lane). */
  readonly onSpawnAt?: ((stationId: string) => void) | undefined;
};

/** The five columns of the board; a column holds one or more deck statuses, in this order. */
export const COLUMNS: readonly {
  readonly id: string;
  readonly label: string;
  readonly statuses: readonly CardStatus[];
  readonly hint: string;
}[] = [
  {
    id: 'queued',
    label: 'Queued',
    statuses: ['queued'],
    hint: 'Accepted by the station, not started yet',
  },
  {
    id: 'working',
    label: 'Working',
    statuses: ['needs_you', 'working'],
    hint: 'A turn runs, or the session waits for you',
  },
  {
    id: 'verifying',
    label: 'Verifying',
    statuses: ['verifying'],
    hint: 'A check runs: tests, a build, a linter',
  },
  {
    id: 'idle',
    label: 'Idle',
    statuses: ['idle'],
    hint: 'The turn ended; the session waits for a prompt',
  },
  {
    id: 'offline',
    label: 'Offline',
    statuses: ['ended', 'unreachable', 'lost'],
    hint: 'Ended, lost, or its station is unreachable',
  },
];

/** The cards of one column, in the order of `statuses`, then newest first; starred cards first. */
export function columnCards(
  groups: Record<CardStatus, DeckSession[]>,
  statuses: readonly CardStatus[],
  starred?: ReadonlySet<string>,
): DeckSession[] {
  const cards = statuses.flatMap((status) => groups[status]);
  if (starred === undefined || starred.size === 0) return cards;
  const star = (c: DeckSession) => starred.has(sessionRef(c.station_id, c.summary.session_id));
  return [...cards.filter(star), ...cards.filter((c) => !star(c))];
}

/** The status order of a strip lane: what needs the owner first, the ended last. */
export const STRIP_ORDER: readonly CardStatus[] = [
  'needs_you',
  'working',
  'verifying',
  'queued',
  'idle',
  'unreachable',
  'lost',
  'ended',
];

/** The strips of one lane: starred first, then by `STRIP_ORDER`, then newest first. */
export function stripCards(
  cards: readonly DeckSession[],
  starred?: ReadonlySet<string>,
): DeckSession[] {
  const groups = statusGroups([...cards]);
  const ordered = columnCards(groups, STRIP_ORDER);
  if (starred === undefined || starred.size === 0) return ordered;
  const star = (c: DeckSession) => starred.has(sessionRef(c.station_id, c.summary.session_id));
  return [...ordered.filter(star), ...ordered.filter((c) => !star(c))];
}

const isOffline = (c: DeckSession) =>
  c.status === 'ended' || c.status === 'unreachable' || c.status === 'lost';

const isDone = (c: DeckSession) => c.status === 'ended' || c.status === 'lost';

/** The swimlane order: the known stations first, then the other station ids (the sidebar order). */
function laneOrder(sessions: readonly DeckSession[], stations: readonly StationView[]): string[] {
  return stationOrder(
    sessions.map((s) => s.station_id),
    stations,
  );
}

const isCleared = (card: DeckSession, refs: ReadonlySet<string>) =>
  refs.has(sessionRef(card.station_id, card.summary.session_id));

/** How long the Resume all result stays on the screen. */
const RESUMED_MS = 6000;

/**
 * The offline tools: Resume all, Archive, and Restore all (DL-141). `row`: the labelled row at the
 * end of the board ("All ended", the ended sessions of every lane); `lane`: the buttons inside a
 * lane's Ended group. The counts are `.fd-count` pills (as "Ended 2").
 */
export function OfflineTools({
  cards,
  freshness,
  sessionActions,
  variant = 'row',
}: {
  readonly cards: readonly DeckSession[];
  readonly freshness: Freshness;
  readonly sessionActions?: ((key: SessionKey) => SessionActions) | undefined;
  readonly variant?: 'row' | 'lane';
}) {
  const refs = archived.use();
  const [running, setRunning] = useState(false);
  const [report, setReport] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const actionsOf = (card: DeckSession) =>
    sessionActions?.({ station_id: card.station_id, session_id: card.summary.session_id }) ?? null;
  const resumable = cards.filter((c) => sessionFlags(c, freshness, actionsOf(c)).canResume);
  const clearable = cards
    .filter((c) => c.status === 'ended' || c.status === 'lost')
    .filter((c) => !isCleared(c, refs));
  const restorable = cards.filter((c) => isCleared(c, refs));
  const refsOf = (list: readonly DeckSession[]) =>
    list.map((c) => sessionRef(c.station_id, c.summary.session_id));
  const resumeAll = async () => {
    const targets = resumable.map((c) => actionsOf(c));
    setRunning(true);
    setReport(null);
    let ok = 0;
    for (const actions of targets) {
      try {
        if ((await actions?.resume())?.ok === true) ok += 1;
      } catch {
        // A failed resume counts as not resumed.
      }
    }
    setRunning(false);
    setReport(`Resumed ${ok} of ${targets.length}`);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setReport(null), RESUMED_MS);
  };
  return (
    <div className="fd-offline-tools" data-variant={variant}>
      {variant === 'lane' ? null : <span className="fd-label">All ended</span>}
      <button
        type="button"
        className="fd-ghost fd-sm"
        disabled={running || resumable.length === 0}
        onClick={() => void resumeAll()}
      >
        <Icon of={RotateCcw} size={12} />{' '}
        {running ? (
          'Resuming…'
        ) : (
          <>
            Resume all <span className="fd-count">{resumable.length}</span>
          </>
        )}
      </button>
      {report === null ? null : (
        <span className="fd-offline-report" role="status">
          {report}
        </span>
      )}
      <button
        type="button"
        className="fd-ghost fd-sm"
        disabled={clearable.length === 0}
        onClick={() => archived.add(refsOf(clearable))}
      >
        <Icon of={Archive} size={12} /> Archive <span className="fd-count">{clearable.length}</span>
      </button>
      {restorable.length === 0 ? null : (
        <button
          type="button"
          className="fd-ghost fd-sm"
          onClick={() => archived.remove(refsOf(restorable))}
        >
          <Icon of={ArchiveRestore} size={12} /> Restore all
        </button>
      )}
    </div>
  );
}

/** The sessions of one lane, made from `cards` by `laneOf`. */
type Lane = {
  readonly id: string;
  readonly name: string;
  readonly label: string;
  readonly cards: readonly DeckSession[];
};

function lanesOf(
  sessions: readonly DeckSession[],
  stations: readonly StationView[],
  group: FleetGroup,
  projects: readonly Project[],
): Lane[] {
  if (group === 'project') {
    return projectGroups(sessions, projects).map((g) => ({
      id: g.id,
      name: g.name,
      label: `Project ${g.name}`,
      cards: g.cards,
    }));
  }
  if (group === 'repo') {
    const names = [...new Set(sessions.map(repoName))].sort((a, b) =>
      a === b ? 0 : a === NO_REPO ? 1 : b === NO_REPO ? -1 : a.localeCompare(b),
    );
    return names.map((name) => ({
      id: name,
      name,
      label: `Repository ${name}`,
      cards: sessions.filter((s) => repoName(s) === name),
    }));
  }
  return laneOrder(sessions, stations).map((id) => {
    const name = stationName(id, stations);
    return {
      id,
      name,
      label: `Station ${name}`,
      cards: sessions.filter((s) => s.station_id === id),
    };
  });
}

/**
 * The lanes of the online stations (at their last report: a stale board keeps it) that have no
 * session: after the other lanes, in the deck's station order.
 */
export function emptyStationLanes(
  lanes: readonly { readonly id: string }[],
  stations: readonly StationView[],
): Lane[] {
  const taken = new Set(lanes.map((l) => l.id));
  return stations
    .filter((s) => s.reachable && !taken.has(s.station_id))
    .map((s) => ({ id: s.station_id, name: s.name, label: `Station ${s.name}`, cards: [] }));
}

/** A lane with only ended or lost sessions goes after the lanes with a live session. */
const settled = (lane: Lane) =>
  lane.cards.every((c) => c.status === 'ended' || c.status === 'lost');

/** The platform names of the station traits, as the owner reads them. */
const OS_NAME: Readonly<Record<string, string>> = {
  linux: 'Linux',
  macos: 'macOS',
  darwin: 'macOS',
  omarchy: 'Omarchy',
  windows: 'Windows',
};

/** The station facts of a lane head: platform, and the reach (dot and word). */
function StationFacts({
  station,
  freshness,
}: {
  readonly station: StationView;
  readonly freshness: Freshness;
}) {
  const traits = station.info?.traits;
  const up = freshness !== 'stale' && station.reachable;
  return (
    <>
      {traits === undefined ? null : (
        <span className="fd-lane-platform">
          {OS_NAME[traits.os.toLowerCase()] ?? traits.os} · {traits.arch}
        </span>
      )}
      <span className="fd-lane-reach" data-up={up ? 'true' : 'false'}>
        {up ? 'online' : 'offline'}
      </span>
    </>
  );
}

/** A slot meter shows one tick per slot up to this many; a bigger station shows the count only. */
const MAX_TICKS = 16;

/**
 * The slot meter of a station lane: "3 of 12", the needs-you slots first (DESIGN-SYSTEM §4.4). A
 * station that runs no session shows its free slots as words ("12 free", as the New session
 * station pills): a row of empty ticks says nothing more.
 */
function LaneSlots({
  station,
  cards,
}: {
  readonly station: StationView;
  readonly cards: readonly DeckSession[];
}) {
  const info = station.info;
  if (info == null || info.max_sessions == null) return null;
  const max = info.max_sessions;
  const used = Math.min(info.live_sessions, max);
  const needs = Math.min(cards.filter((c) => c.status === 'needs_you').length, used);
  if (max > 0 && info.live_sessions === 0)
    return <span className="fd-lane-slots fd-lane-free">{max} free</span>;
  return (
    <span className="fd-lane-slots">
      {max > 0 && max <= MAX_TICKS ? (
        <span className="fd-slots" aria-hidden="true">
          {Array.from({ length: max }, (_, i) => (
            <span key={i} data-slot={i < needs ? 'needs_you' : i < used ? 'used' : undefined} />
          ))}
        </span>
      ) : null}
      {info.live_sessions} of {max}
    </span>
  );
}

/**
 * The cards of the board-wide offline row of a folded board: every ended or lost session, when
 * two or more lanes have one. One press then resumes, clears or restores the ended sessions of
 * every station (after a station restart, or to tidy the board), without opening each Ended
 * fold. With one such lane, its Ended row has the same tools: no row. An unreachable session is
 * not ended, so it makes no row.
 */
export function boardEnded(
  lanes: readonly { readonly cards: readonly DeckSession[] }[],
): DeckSession[] {
  if (lanes.filter((lane) => lane.cards.some(isDone)).length < 2) return [];
  const done = lanes.flatMap((lane) => lane.cards.filter(isDone));
  return done.filter((c, i) => done.findIndex((d) => cardKey(d) === cardKey(c)) === i);
}

/** The board: one lane for each station, repo or project; one strip for each session. */
export function FleetBoard({
  sessions,
  stations,
  freshness,
  now: given,
  group = 'station',
  contested,
  sessionActions,
  extras,
  foldEnded = false,
  emptyLanes = false,
  onSpawnAt,
}: Props) {
  const now = useNow(given, 30000);
  const starred = priority.use();
  const projects = useProjects();
  const batches = batchCounts(sessions);
  const all = lanesOf(sessions, stations, group, projects);
  const lanes = [
    ...all.filter((l) => !settled(l)),
    ...all.filter(settled),
    ...(emptyLanes && group === 'station' ? emptyStationLanes(all, stations) : []),
  ];
  const offline = foldEnded ? boardEnded(lanes) : sessions.filter(isOffline);
  const strip = (card: DeckSession) => (
    <FleetStrip
      key={cardKey(card)}
      card={card}
      stations={stations}
      freshness={freshness}
      now={now}
      showStation={group !== 'station'}
      contested={contested?.get(sessionRef(card.station_id, card.summary.session_id))}
      extra={extras?.get(sessionRef(card.station_id, card.summary.session_id))}
      batchCount={batchCountOf(card, batches)}
    />
  );
  return (
    <main className="fd-board" aria-label="Fleet board">
      {lanes.map((lane) => {
        const contestedCount =
          contested === undefined
            ? 0
            : lane.cards.filter((c) =>
                contested.has(sessionRef(c.station_id, c.summary.session_id)),
              ).length;
        const view =
          group === 'station' ? stations.find((s) => s.station_id === lane.id) : undefined;
        const warnings = group === 'station' ? resourceWarnings(view?.info?.resources) : [];
        const ordered = stripCards(lane.cards, starred);
        const live = foldEnded ? ordered.filter((c) => !isDone(c)) : ordered;
        const done = foldEnded ? ordered.filter(isDone) : [];
        // The count after the name is the shown strips (the sidebar's rule): a folded lane's
        // Ended row carries its own count, and the lane total stays in the title. A lane with no
        // shown strip has no count: a zero pill is nothing to act on (§4.2, §5).
        const total =
          live.length === lane.cards.length
            ? undefined
            : `${lane.cards.length} ${lane.cards.length === 1 ? 'session' : 'sessions'} ${group === 'station' ? 'on' : 'in'} ${lane.name}`;
        return (
          <section
            key={lane.id}
            className="fd-lane"
            data-station={group === 'station' ? lane.id : undefined}
            data-repo={group === 'repo' ? lane.id : undefined}
            data-project={group === 'project' ? lane.id : undefined}
            aria-label={lane.label}
          >
            <div className="fd-lane-head">
              <h2 className="fd-lane-name fd-wide">{lane.name}</h2>
              {live.length === 0 ? null : (
                <span className="fd-count" title={total}>
                  {live.length}
                </span>
              )}
              {view === undefined ? null : <StationFacts station={view} freshness={freshness} />}
              {warnings.length > 0 ? (
                <span className="fd-lane-warn" title={warnings.map((w) => w.text).join('\n')}>
                  <Icon of={TriangleAlert} size={12} /> Low resources
                </span>
              ) : null}
              {group === 'repo' && contestedCount > 0 ? (
                <span className="fd-lane-conf">
                  <Icon of={TriangleAlert} size={12} /> {contestedCount} contested
                </span>
              ) : null}
              {view === undefined ? null : <LaneSlots station={view} cards={lane.cards} />}
            </div>
            {live.length === 0 ? null : <ul className="fd-strips">{live.map(strip)}</ul>}
            {lane.cards.length > 0 ? null : (
              <div className="fd-lane-none">
                <p className="fd-note">No sessions.</p>
                {onSpawnAt === undefined ? null : (
                  <button
                    type="button"
                    className="fd-ghost fd-sm"
                    aria-label={`New session on ${lane.name}`}
                    onClick={() => onSpawnAt(lane.id)}
                  >
                    <Icon of={Plus} size={12} /> New session
                  </button>
                )}
              </div>
            )}
            {done.length === 0 ? null : (
              <details className="fd-lane-ended">
                <summary>
                  Ended <span className="fd-count">{done.length}</span>
                </summary>
                <ul className="fd-strips">{done.map(strip)}</ul>
                <OfflineTools
                  cards={done}
                  freshness={freshness}
                  sessionActions={sessionActions}
                  variant="lane"
                />
              </details>
            )}
          </section>
        );
      })}
      {offline.length === 0 ? null : (
        <OfflineTools
          cards={offline}
          freshness={freshness}
          sessionActions={sessionActions}
          variant="row"
        />
      )}
    </main>
  );
}
