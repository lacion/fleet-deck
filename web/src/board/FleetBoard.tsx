import { basename, relativeAge } from '../app/sessions';
import { formatHash } from '../app/routes';
import type { CardStatus, DeckSession, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import { runningSubagents } from '../session/timeline';
import { modelFamily, shortModel } from '../ui/format';
import { GitBranch, Folder, Icon, Layers, TriangleAlert, Workflow } from '../ui/icons';
import { useNow } from '../ui/useNow';
import { cardKey, stationName, statusGroups } from './fleet';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly now?: number | undefined;
};

/** The five columns of the board; a column holds one or more deck statuses, in this order. */
export const COLUMNS: readonly {
  readonly id: string;
  readonly label: string;
  readonly statuses: readonly CardStatus[];
}[] = [
  { id: 'queued', label: 'Queued', statuses: ['queued'] },
  { id: 'working', label: 'Working', statuses: ['needs_you', 'working'] },
  { id: 'verifying', label: 'Verifying', statuses: ['verifying'] },
  { id: 'idle', label: 'Idle', statuses: ['idle'] },
  { id: 'offline', label: 'Offline', statuses: ['ended', 'unreachable', 'lost'] },
];

/** The cards of one column, in the order of `statuses`, then newest first. */
export function columnCards(
  groups: Record<CardStatus, DeckSession[]>,
  statuses: readonly CardStatus[],
): DeckSession[] {
  return statuses.flatMap((status) => groups[status]);
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
}: {
  readonly card: DeckSession;
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly now: number;
  readonly showStation: boolean;
}) {
  const s = card.summary;
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
      >
        <span className="fd-card-head">
          <span className="fd-dot" data-status={card.status} aria-hidden="true" />
          <span className="fd-card-title">{s.title ?? s.callsign ?? s.session_id}</span>
          <span className="fd-badge" data-family={modelFamily(s.model) ?? undefined}>
            {shortModel(s.model)}
          </span>
        </span>
        {where === null && !showStation ? null : (
          <span className="fd-card-where">
            {station}
            {where}
          </span>
        )}
        <CardNote card={card} />
        <span className="fd-card-foot">
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

/** The deck projection owns the column; cards retain the station-scoped identity. */
export function FleetBoard({ sessions, stations, freshness, now: given }: Props) {
  const now = useNow(given, 30000);
  const groups = statusGroups(sessions);
  const counts = COLUMNS.map((column) => columnCards(groups, column.statuses).length);
  return (
    <main className="fd-board" aria-label="Fleet board">
      <div className="fd-colheads">
        {COLUMNS.map((column, i) => (
          <span key={column.id} className="fd-colhead">
            <span className="fd-label">{column.label}</span>{' '}
            <span className="fd-count">{counts[i]}</span>
          </span>
        ))}
      </div>
      {laneOrder(sessions, stations).map((id) => {
        const own = sessions.filter((s) => s.station_id === id);
        const ownGroups = statusGroups(own);
        const name = stationName(id, stations);
        return (
          <section key={id} className="fd-swim" data-station={id} aria-label={`Station ${name}`}>
            <h2 className="fd-swim-head">
              <span className="fd-swim-name">{name}</span>{' '}
              <span className="fd-count">
                {own.length === 1 ? '1 session' : `${own.length} sessions`}
              </span>
              <span className="fd-swim-rule" aria-hidden="true" />
            </h2>
            <div className="fd-swim-grid">
              {COLUMNS.map((column) => (
                <ul key={column.id} className="fd-col">
                  {columnCards(ownGroups, column.statuses).map((card) => (
                    <FleetCard
                      key={cardKey(card)}
                      card={card}
                      stations={stations}
                      freshness={freshness}
                      now={now}
                      showStation={false}
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
