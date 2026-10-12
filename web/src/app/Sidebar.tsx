import { useEffect, useState } from 'react';
import { STATUS_LANES } from '../board/fleet';
import { providerLabel } from '../board/lanes';
import type { DeckSession, StationView } from '../gen';
import type { Chrome } from './chrome';
import { formatHash, type Route } from './routes';
import { relativeAge, type SidebarModel, sessionTitle, sidebarModel } from './sessions';

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
        <span className="fd-sdot" aria-hidden="true" />
        <span className="fd-srow-main">
          <span className="fd-srow-title">
            <span className="fd-vh">{statusLabel(card)}: </span>
            {sessionTitle(card)}
            {s.terminal != null ? <span className="fd-srow-mark">In terminal</span> : null}
          </span>
          <span className="fd-srow-meta">
            {providerLabel(s.provider)} · {s.model ?? 'default model'} ·{' '}
            {relativeAge(s.updated_at, now)}
          </span>
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

function Groups({ model, route, now }: { model: SidebarModel; route: Route; now: number }) {
  return (
    <>
      {model.needs.length > 0 ? (
        <section className="fd-sgroup" data-group="needs" aria-label="Needs you">
          <h3 className="fd-sgroup-title">Needs you · {model.needs.length}</h3>
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
            <span className="fd-sgroup-name">{group.name}</span>
            <span className="fd-vh">{group.reachable ? 'reachable' : 'unreachable'}</span>
            <span className="fd-count">{group.count}</span>
          </h3>
          <Rows cards={group.live} route={route} now={now} />
          {group.ended.length > 0 ? (
            <details className="fd-sended">
              <summary>Ended · {group.ended.length}</summary>
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
  return (
    <aside className="fd-sidebar" aria-label="Sessions" data-variant={variant}>
      <div className="fd-sidebar-top">
        <div className="fd-sidebar-brand">
          <span className="fd-wordmark">FLEET DECK</span>
          <span
            className="fd-cdot"
            role="img"
            data-freshness={chrome?.freshness ?? 'none'}
            title={chrome?.label ?? 'Not connected'}
            aria-label={chrome?.label ?? 'Not connected'}
          />
        </div>
        <button type="button" className="fd-primary" onClick={() => onNavigate({ name: 'new' })}>
          New session
        </button>
        <input
          id="fd-sidebar-search"
          type="search"
          aria-label="Search sessions"
          placeholder="Search sessions"
          autoComplete="off"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
      </div>
      <div className="fd-sidebar-list">
        {empty ? (
          <p className="fd-empty">No sessions yet.</p>
        ) : nothingFound ? (
          <p className="fd-empty">No matching sessions.</p>
        ) : (
          <Groups model={model} route={route} now={now} />
        )}
      </div>
      {variant === 'full' ? (
        <div className="fd-sidebar-bottom">
          <nav className="fd-sidebar-links" aria-label="Board pages">
            <a href="#/fleet" aria-current={route.name === 'fleet' ? 'page' : undefined}>
              Fleet
            </a>
            <a href="#/plans" aria-current={route.name === 'plans' ? 'page' : undefined}>
              Plans
            </a>
            <a href="#/stations" aria-current={route.name === 'stations' ? 'page' : undefined}>
              Stations · {stations.length}
            </a>
            {terminalCount > 0 ? (
              <button type="button" className="fd-linkbtn" onClick={onOpenTerminals}>
                Terminals · {terminalCount}
              </button>
            ) : null}
          </nav>
          {chrome === undefined ? null : (
            <div className="fd-sidebar-conn" data-freshness={chrome.freshness}>
              <span role="status">{chrome.label}</span>
              <button type="button" onClick={chrome.onLogout}>
                Log out
              </button>
            </div>
          )}
        </div>
      ) : null}
    </aside>
  );
}
