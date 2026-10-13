import { type CSSProperties, useEffect, useRef, useState } from 'react';
import { firstLine, isStartFailure } from '../app/failures';
import { formatHash } from '../app/routes';
import { basename, relativeAge } from '../app/sessions';
import type { CardStatus, DeckSession, SessionExtras, SessionKey, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import { sessionFlags } from '../session/controller';
import type { SessionActions } from '../session/commands';
import { runningSubagents } from '../session/timeline';
import { modelFamily, shortModel } from '../ui/format';
import {
  Archive,
  ArchiveRestore,
  Folder,
  GitBranch,
  Icon,
  Layers,
  Mail,
  RotateCcw,
  Star,
  TriangleAlert,
  Workflow,
} from '../ui/icons';
import { archived } from '../ui/archive';
import { priority, sessionRef } from '../ui/localSet';
import { useNow } from '../ui/useNow';
import { buckets } from './activity';
import { type Contested, cardKey, NO_REPO, repoName, stationName, statusGroups } from './fleet';
import { Sparkline } from './Sparkline';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly now?: number | undefined;
  /** DL-141: how the lanes are made; the default is by station. */
  readonly group?: 'station' | 'repo' | undefined;
  /** DL-141: the contested sessions, by `sessionRef`. */
  readonly contested?: ReadonlyMap<string, Contested> | undefined;
  /** DL-141: the actions of a session, for the Offline column's Resume all. */
  readonly sessionActions?: ((key: SessionKey) => SessionActions) | undefined;
  /** DL-141: the card extras (mail, touched files), by `sessionRef`. */
  readonly extras?: ReadonlyMap<string, SessionExtras> | undefined;
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

const STATUS_NOTE: Record<CardStatus, string> = {
  queued: 'Queued',
  working: 'Working',
  verifying: 'Verifying',
  needs_you: 'Needs you',
  idle: 'Waiting for a prompt',
  ended: 'Ended',
  unreachable: 'Station unreachable',
  lost: 'Lost',
};

function CardNote({ card }: { card: DeckSession }) {
  const s = card.summary;
  const ask = s.open_requests[0];
  if (ask !== undefined) {
    return (
      <span className="fd-card-note">
        <span className="fd-card-ask">
          <Icon of={TriangleAlert} size={12} /> {ask.title}
        </span>
      </span>
    );
  }
  const tool = s.running_tools[0];
  if (tool !== undefined) {
    return (
      <span className="fd-card-note">
        <span className="fd-card-tool">
          {tool.input_summary === '' ? tool.title : `${tool.title}: ${tool.input_summary}`}
        </span>
      </span>
    );
  }
  const error = s.last_error;
  if (error != null && isStartFailure(card)) {
    return (
      <span className="fd-card-note" data-note="start_failed">
        <span className="fd-card-error">Failed to start: {firstLine(error.message)}</span>
      </span>
    );
  }
  if (
    error !== null &&
    error !== undefined &&
    (card.status === 'idle' || card.status === 'ended' || card.status === 'lost')
  ) {
    return (
      <span className="fd-card-note">
        <span className="fd-card-error">
          {error.code}: {error.message}
        </span>
      </span>
    );
  }
  return (
    <span className="fd-card-note" data-note={card.status}>
      {STATUS_NOTE[card.status]}
    </span>
  );
}

/** One session card: the whole card is the link to the session. */
export function FleetCard({
  card,
  stations,
  freshness,
  now,
  showStation,
  contested,
  extra,
}: {
  readonly card: DeckSession;
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly now: number;
  readonly showStation: boolean;
  readonly contested?: Contested | undefined;
  readonly extra?: SessionExtras | undefined;
}) {
  const s = card.summary;
  const ref = sessionRef(card.station_id, s.session_id);
  const starred = priority.use().has(ref);
  const cleared = archived.use().has(ref) && (card.status === 'ended' || card.status === 'lost');
  const bypass = s.mode === 'bypassPermissions' || s.mode === 'dontAsk';
  const subagents = runningSubagents(s);
  const where =
    s.branch !== null && s.branch !== '' ? (
      <>
        <Icon of={GitBranch} size={12} /> {s.branch}
      </>
    ) : s.cwd !== null && s.cwd !== '' ? (
      <>
        <Icon of={Folder} size={12} /> {basename(s.cwd)}
      </>
    ) : null;
  const files = extra?.files ?? [];
  const activity = buckets(ref, now);
  const station = showStation
    ? `${stationName(card.station_id, stations)}${where === null ? '' : ' · '}`
    : '';
  return (
    <li className="fd-cardwrap">
      <a
        className="fd-card"
        href={formatHash({
          name: 'session',
          station: card.station_id,
          session: s.session_id,
          tab: 'chat',
        })}
        data-session={s.session_id}
        data-station={card.station_id}
        data-status={card.status}
        data-contested={contested === undefined ? undefined : 'true'}
        data-archived={cleared ? 'true' : undefined}
      >
        <span className="fd-card-head">
          <span className="fd-dot" data-status={card.status} aria-hidden="true" />
          <span className="fd-card-title">{s.title ?? s.callsign ?? s.session_id}</span>
          {starred ? (
            <span className="fd-card-star" title="Priority">
              <Icon of={Star} size={12} />
            </span>
          ) : null}
          {extra !== undefined && extra.mail_pending > 0 ? (
            <span className="fd-card-mail" title="Mail waiting for delivery">
              <Icon of={Mail} size={12} /> {extra.mail_pending}
            </span>
          ) : null}
        </span>
        <span className="fd-card-where">
          {station === '' && where === null ? null : (
            <span className="fd-card-place">
              {station}
              {where}
            </span>
          )}
          <span className="fd-badge" data-family={modelFamily(s.model) ?? undefined}>
            {shortModel(s.model)}
          </span>
        </span>
        <CardNote card={card} />
        {files.length === 0 ? null : (
          <span className="fd-card-files">
            {files.map((path) => (
              <span
                key={path}
                className="fd-filechip"
                title={path}
                data-hot={contested?.files.includes(path) ? 'true' : undefined}
              >
                {basename(path)}
              </span>
            ))}
          </span>
        )}
        {contested === undefined ? null : (
          <span className="fd-card-contested">
            <Icon of={TriangleAlert} size={12} /> contested with {contested.with.join(', ')}
          </span>
        )}
        <span className="fd-card-foot">
          {bypass ? (
            <span className="fd-badge" data-tone="hazard" title="Permission mode">
              {s.mode}
            </span>
          ) : null}
          {cleared ? (
            <span className="fd-badge" data-tone="muted">
              Archived
            </span>
          ) : null}
          {subagents > 0 ? (
            <span className="fd-badge" title="Subagents running" data-running-subagents={subagents}>
              <Icon of={Workflow} size={12} /> {subagents}
            </span>
          ) : null}
          {s.background_tasks.length > 0 ? (
            <span className="fd-badge" title="Background tasks">
              <Icon of={Layers} size={12} /> {s.background_tasks.length}
            </span>
          ) : null}
          {s.terminal != null ? (
            <span className="fd-badge" data-tone="act">
              In terminal
            </span>
          ) : null}
          {s.usage.cost_usd !== null ? (
            <span className="fd-badge">${s.usage.cost_usd.toFixed(2)}</span>
          ) : null}
          {freshness === 'stale' ? (
            <span className="fd-badge" data-tone="muted">
              Stale
            </span>
          ) : null}
          {activity.some((n) => n > 0) ? <Sparkline values={activity} /> : null}
          <span className="fd-card-age">{relativeAge(s.updated_at, now)}</span>
        </span>
      </a>
    </li>
  );
}

/** The swimlane order: the known stations first, then the other station ids. */
function laneOrder(sessions: readonly DeckSession[], stations: readonly StationView[]): string[] {
  const present = new Set(sessions.map((s) => s.station_id));
  const known = stations.map((s) => s.station_id).filter((id) => present.has(id));
  const rest = [...present].filter((id) => !known.includes(id)).sort();
  return [...new Set([...known, ...rest])];
}

const isCleared = (card: DeckSession, refs: ReadonlySet<string>) =>
  refs.has(sessionRef(card.station_id, card.summary.session_id));

/** How long the Resume all result stays on the screen. */
const RESUMED_MS = 6000;

/** The Offline column's head tools: Resume all, Clear, and Restore all (DL-141). */
export function OfflineTools({
  cards,
  freshness,
  sessionActions,
}: {
  readonly cards: readonly DeckSession[];
  readonly freshness: Freshness;
  readonly sessionActions?: ((key: SessionKey) => SessionActions) | undefined;
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
    <span className="fd-colhead-tools">
      <button
        type="button"
        className="fd-ghost fd-sm"
        disabled={running || resumable.length === 0}
        onClick={() => void resumeAll()}
      >
        <Icon of={RotateCcw} size={12} />{' '}
        {running ? 'Resuming…' : `Resume all (${resumable.length})`}
      </button>
      {report === null ? null : (
        <span className="fd-colhead-report" role="status">
          {report}
        </span>
      )}
      <button
        type="button"
        className="fd-ghost fd-sm"
        disabled={clearable.length === 0}
        onClick={() => archived.add(refsOf(clearable))}
      >
        <Icon of={Archive} size={12} /> Clear ({clearable.length})
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
    </span>
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
  group: 'station' | 'repo',
): Lane[] {
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

/** The deck projection owns the column; cards retain the station-scoped identity. */
export function FleetBoard({
  sessions,
  stations,
  freshness,
  now: given,
  group = 'station',
  contested,
  sessionActions,
  extras,
}: Props) {
  const now = useNow(given, 30000);
  const starred = priority.use();
  const groups = statusGroups(sessions);
  const counts = COLUMNS.map((column) => columnCards(groups, column.statuses).length);
  // An empty column stays visible but narrow, so the cards get the width.
  const cols = counts.map((n) => (n > 0 ? 'minmax(190px, 1fr)' : 'minmax(96px, 0.4fr)'));
  const minWidth = counts.reduce((sum, n) => sum + (n > 0 ? 190 : 96), 10 * (COLUMNS.length - 1));
  const style = { '--fd-cols': cols.join(' '), minWidth } as CSSProperties;
  return (
    <main className="fd-board" aria-label="Fleet board" style={style}>
      <div className="fd-colheads">
        {COLUMNS.map((column, i) => (
          <span key={column.id} className="fd-colhead" title={column.hint}>
            <span className="fd-label">{column.label}</span>{' '}
            <span className="fd-count">{counts[i]}</span>
            {column.id === 'offline' && counts[i] !== 0 ? (
              <OfflineTools
                cards={columnCards(groups, column.statuses)}
                freshness={freshness}
                sessionActions={sessionActions}
              />
            ) : null}
          </span>
        ))}
      </div>
      {lanesOf(sessions, stations, group).map((lane) => {
        const ownGroups = statusGroups([...lane.cards]);
        const contestedCount =
          contested === undefined
            ? 0
            : lane.cards.filter((c) =>
                contested.has(sessionRef(c.station_id, c.summary.session_id)),
              ).length;
        return (
          <section
            key={lane.id}
            className="fd-swim"
            data-station={group === 'station' ? lane.id : undefined}
            data-repo={group === 'repo' ? lane.id : undefined}
            aria-label={lane.label}
          >
            <h2 className="fd-swim-head">
              <span className="fd-swim-name">{lane.name}</span>{' '}
              <span className="fd-count">
                {lane.cards.length === 1 ? '1 session' : `${lane.cards.length} sessions`}
              </span>
              {group === 'repo' && contestedCount > 0 ? (
                <span className="fd-swim-conf">▲ {contestedCount} contested</span>
              ) : null}
              <span className="fd-swim-rule" aria-hidden="true" />
            </h2>
            <div className="fd-swim-grid">
              {COLUMNS.map((column) => (
                <ul key={column.id} className="fd-col">
                  {columnCards(ownGroups, column.statuses, starred).map((card) => (
                    <FleetCard
                      key={cardKey(card)}
                      card={card}
                      stations={stations}
                      freshness={freshness}
                      now={now}
                      showStation={group === 'repo'}
                      contested={contested?.get(
                        sessionRef(card.station_id, card.summary.session_id),
                      )}
                      extra={extras?.get(sessionRef(card.station_id, card.summary.session_id))}
                    />
                  ))}
                </ul>
              ))}
            </div>
          </section>
        );
      })}
    </main>
  );
}
