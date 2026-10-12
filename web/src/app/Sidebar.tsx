import { useEffect, useState } from 'react';
import { STATUS_LANES } from '../board/fleet';
import type { DeckSession, StationView } from '../gen';
import {
  Folder,
  GitBranch,
  Icon,
  LayoutGrid,
  LogOut,
  FileText,
  Plus,
  Search,
  Server,
  SquareTerminal,
  TriangleAlert,
  Zap,
} from '../ui/icons';
import { shortModel } from '../ui/format';
import type { Chrome } from './chrome';
import { formatHash, type Route } from './routes';
import { basename, relativeAge, type SidebarModel, sessionTitle, sidebarModel } from './sessions';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly route: Route;
  /** 'list' is the phone home: the session list without the bottom links. */
  readonly variant?: 'full' | 'list';
  readonly chrome?: Chrome | undefined;
  readonly terminalCount: number;
  readonly onNavigate: (route: Route) => void;
  readonly onOpenTerminals: () => void;
  /** Unix ms of "now" (tests). */
  readonly now?: number;
};

const statusLabel = (card: DeckSession) =>
  STATUS_LANES.find((lane) => lane.status === card.status)?.label ?? card.status;

/** Re-renders every `ms` so that the ages stay right. */
function useNow(given: number | undefined, ms: number): number {
  const [now, setNow] = useState(() => given ?? Date.now());
  useEffect(() => {
    if (given !== undefined) return undefined;
    const timer = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(timer);
  }, [given, ms]);
  return given ?? now;
}

function Row({ card, route, now }: { card: DeckSession; route: Route; now: number }) {
  const s = card.summary;
  const open =
    route.name === 'session' && route.station === card.station_id && route.session === s.session_id;
  const href = formatHash({
    name: 'session',
    station: card.station_id,
    session: s.session_id,
    tab: 'chat',
  });
  return (
    <li>
      <a
        href={href}
        className="fd-srow"
        data-status={card.status}
        aria-current={open ? 'page' : undefined}
      >
        <span className="fd-dot" data-status={card.status} aria-hidden="true" />
        <span className="fd-srow-title">
          <span className="fd-vh">{statusLabel(card)}: </span>
          {sessionTitle(card)}
          {s.terminal != null ? (
            <span className="fd-badge" data-tone="act">
              In terminal
            </span>
          ) : null}
        </span>
        <span className="fd-srow-age">{relativeAge(s.updated_at, now)}</span>
        <span className="fd-srow-meta">
          {shortModel(s.model)}
          {s.branch != null && s.branch !== '' ? (
            <>
              {' · '}
              <Icon of={GitBranch} size={12} />
              {s.branch}
            </>
          ) : s.cwd != null ? (
            <>
              {' · '}
              <Icon of={Folder} size={12} />
              {basename(s.cwd)}
            </>
          ) : null}
        </span>
      </a>
    </li>
  );
}

function Rows({ cards, route, now }: { cards: readonly DeckSession[]; route: Route; now: number }) {
  return (
    <ul className="fd-srows">
      {cards.map((card) => (
        <Row
          key={`${card.station_id}/${card.summary.session_id}`}
          card={card}
          route={route}
          now={now}
        />
      ))}
    </ul>
  );
}

function Groups({
  model,
  route,
  now,
  withNeeds,
}: {
  model: SidebarModel;
  route: Route;
  now: number;
  withNeeds: boolean;
}) {
  return (
    <>
      {withNeeds && model.needs.length > 0 ? (
        <section className="fd-sgroup" data-group="needs" aria-label="Needs you">
          <h3 className="fd-sgroup-title fd-label">Needs you · {model.needs.length}</h3>
          <Rows cards={model.needs} route={route} now={now} />
        </section>
      ) : null}
      {model.groups.map((group) => (
        <section
          key={group.id}
          className="fd-sgroup"
          data-station={group.id}
          aria-label={`Station ${group.name}`}
        >
          <h3 className="fd-sgroup-title">
            <span
              className="fd-rdot"
              data-reachable={group.reachable ? 'true' : 'false'}
              aria-hidden="true"
            />
            <span className="fd-sgroup-name fd-label">{group.name}</span>
            <span className="fd-vh">{group.reachable ? 'reachable' : 'unreachable'}</span>
            <span className="fd-count">{group.count}</span>
          </h3>
          <Rows cards={group.live} route={route} now={now} />
          {group.ended.length > 0 ? (
            <details className="fd-sended">
              <summary className="fd-label">Ended · {group.ended.length}</summary>
              <Rows cards={group.ended} route={route} now={now} />
            </details>
          ) : null}
        </section>
      ))}
    </>
  );
}

/** The session list of the board: search, needs you, one group per station, page links. */
export function Sidebar({
  sessions,
  stations,
  route,
  variant = 'full',
  chrome,
  terminalCount,
  onNavigate,
  onOpenTerminals,
  now: givenNow,
}: Props) {
  const [query, setQuery] = useState('');
  const now = useNow(givenNow, 30000);
  const model = sidebarModel(sessions, stations, query);
  const empty = sessions.length === 0;
  const nothingFound = !empty && model.groups.length === 0 && model.needs.length === 0;
  const full = variant === 'full';
  const first = model.needs[0];
  return (
    <aside className="fd-sidebar" aria-label="Sessions" data-variant={variant}>
      <div className="fd-sidebar-top">
        <div className="fd-sidebar-brand">
          <Icon of={Zap} />
          <span className="fd-wordmark">FLEET DECK</span>
          {full ? null : (
            <button
              type="button"
              className="fd-iconbtn"
              aria-label="New session"
              onClick={() => onNavigate({ name: 'new' })}
            >
              <Icon of={Plus} />
            </button>
          )}
          <span
            className="fd-wspill"
            data-freshness={chrome?.freshness ?? 'none'}
            title={chrome?.label ?? 'Not connected'}
          >
            <span className="fd-wspill-dot" aria-hidden="true" />
            {chrome === undefined ? 'OFFLINE' : chrome.freshness === 'live' ? 'LIVE' : 'STALE'}
            <span className="fd-vh">: {chrome?.label ?? 'Not connected'}</span>
          </span>
        </div>
        {full ? (
          <button
            type="button"
            className="fd-primary fd-newbtn"
            onClick={() => onNavigate({ name: 'new' })}
          >
            <Icon of={Plus} /> New session{' '}
            <span className="fd-kbd" aria-hidden="true">
              N
            </span>
          </button>
        ) : null}
        <div className="fd-field">
          <Icon of={Search} />
          <input
            id="fd-sidebar-search"
            type="search"
            aria-label="Search sessions"
            placeholder="Search sessions"
            autoComplete="off"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
          <span className="fd-kbd" aria-hidden="true">
            /
          </span>
        </div>
        {full && first !== undefined ? (
          <a
            className="fd-needschip"
            href={formatHash({
              name: 'session',
              station: first.station_id,
              session: first.summary.session_id,
              tab: 'chat',
            })}
          >
            <Icon of={TriangleAlert} size={14} />
            {model.needs.length === 1 ? '1 needs you' : `${model.needs.length} need you`}
          </a>
        ) : null}
      </div>
      <div className="fd-sidebar-list">
        {empty ? (
          <p className="fd-empty">No sessions yet.</p>
        ) : nothingFound ? (
          <p className="fd-empty">No matching sessions.</p>
        ) : (
          <Groups model={model} route={route} now={now} withNeeds={!full} />
        )}
      </div>
      {full ? (
        <div className="fd-sidebar-bottom">
          <nav className="fd-sidebar-links" aria-label="Board pages">
            <a href="#/fleet" aria-current={route.name === 'fleet' ? 'page' : undefined}>
              <Icon of={LayoutGrid} />
              Fleet
            </a>
            <a href="#/plans" aria-current={route.name === 'plans' ? 'page' : undefined}>
              <Icon of={FileText} />
              Plans
            </a>
            <a href="#/stations" aria-current={route.name === 'stations' ? 'page' : undefined}>
              <Icon of={Server} />
              Stations
              <span className="fd-count">{stations.length}</span>
            </a>
            {terminalCount > 0 ? (
              <button type="button" className="fd-linkbtn" onClick={onOpenTerminals}>
                <Icon of={SquareTerminal} />
                Terminals
                <span className="fd-count">{terminalCount}</span>
              </button>
            ) : null}
          </nav>
          {chrome === undefined ? null : (
            <button type="button" className="fd-ghost fd-sm" onClick={chrome.onLogout}>
              <Icon of={LogOut} size={14} /> Log out
            </button>
          )}
        </div>
      ) : null}
    </aside>
  );
}
