import { type ReactNode, useState } from 'react';
import type { AssignActions } from '../assign/assign';
import { AssignBar } from '../assign/AssignBar';
import { Feed } from '../board/FeedPanel';
import { ConflictRadar } from '../board/ConflictRadar';
import type { ConflictLoader } from '../board/conflicts';
import type { FeedItem } from '../board/feed';
import { ALL, emptyFilter, type FleetFilter, filterFleet, stationName } from '../board/fleet';
import { FleetBoard } from '../board/FleetBoard';
import { NeedsYouRail } from '../board/NeedsYouRail';
import { UsageMeters } from '../board/UsageMeters';
import type { FleetActions } from '../fleet/fleet';
import { FleetPanel } from '../fleet/FleetPanel';
import type { DeckSession, SessionKey, StationView } from '../gen';
import type { Freshness } from '../runtime/state';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly assign: AssignActions | undefined;
  readonly fleet: FleetActions | undefined;
  readonly conflicts: ConflictLoader | undefined;
  readonly feed: readonly FeedItem[];
  readonly onOpen: (key: SessionKey) => void;
  readonly onSpawn: () => void;
};

/** Changes when any session changes, so derived panels load again. */
function fleetVersion(sessions: readonly DeckSession[]): string {
  let newest = 0;
  for (const s of sessions) newest = Math.max(newest, s.summary.updated_at);
  return `${sessions.length}/${newest}`;
}

/** The card heading is visible; the component's own first heading stays for screen readers only. */
function HomeCard({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="fd-home-card">
      <h2 className="fd-home-card-title">{title}</h2>
      {children}
    </div>
  );
}

/** The fleet home: counts, filters, who needs you, the lanes, and the cards under them. */
export function FleetHome({
  sessions,
  stations,
  freshness,
  assign,
  fleet,
  conflicts,
  feed,
  onOpen,
  onSpawn,
}: Props) {
  const [filter, setFilter] = useState<FleetFilter>(emptyFilter);
  const visible = filterFleet(sessions, stations, filter);
  const stationIds = [
    ...new Set([...stations.map((s) => s.station_id), ...sessions.map((s) => s.station_id)]),
  ].sort();
  const needsYou = sessions.filter((s) => s.status === 'needs_you').length;
  return (
    <div className="fd-home">
      <header className="fd-home-head">
        <h1 className="fd-screen-title">Fleet</h1>
        <p className="fd-home-counts">
          {sessions.length === 1 ? '1 session' : `${sessions.length} sessions`} ·{' '}
          {needsYou === 1 ? '1 needs you' : `${needsYou} need you`}
        </p>
      </header>
      <form
        className="fd-filters"
        aria-label="Fleet filters"
        onSubmit={(event) => event.preventDefault()}
      >
        <label>
          Station
          <select
            value={filter.station}
            onChange={(event) => setFilter({ ...filter, station: event.target.value })}
          >
            <option value={ALL}>All stations</option>
            {stationIds.map((id) => (
              <option key={id} value={id}>
                {stationName(id, stations)}
              </option>
            ))}
            {filter.station && !stationIds.includes(filter.station) ? (
              <option value={filter.station}>Removed station: {filter.station}</option>
            ) : null}
          </select>
        </label>
        <label>
          Provider
          <select
            value={filter.provider}
            onChange={(event) => setFilter({ ...filter, provider: event.target.value })}
          >
            <option value={ALL}>All providers</option>
            <option value="claude">Claude</option>
            <option value="codex">Codex</option>
            <option value="unknown">Unknown</option>
          </select>
        </label>
        <label>
          Search
          <input
            type="search"
            value={filter.search}
            onChange={(event) => setFilter({ ...filter, search: event.target.value })}
            placeholder="Title, workspace, model…"
          />
        </label>
        <button type="button" onClick={() => setFilter(emptyFilter)}>
          Clear filters
        </button>
        <output aria-live="polite">
          {visible.length} of {sessions.length} sessions
        </output>
      </form>
      <NeedsYouRail sessions={sessions} stations={stations} onOpen={onOpen} />
      <FleetBoard sessions={visible} stations={stations} freshness={freshness} onOpen={onOpen} />
      <div className="fd-home-grid">
        <HomeCard title="Usage">
          <UsageMeters sessions={visible} stations={stations} />
        </HomeCard>
        {conflicts === undefined ? null : (
          <HomeCard title="Conflicts">
            <ConflictRadar
              load={conflicts}
              stations={stations}
              version={fleetVersion(sessions)}
              onOpen={onOpen}
            />
          </HomeCard>
        )}
        <HomeCard title="Fleet mail and tasks">
          <FleetPanel actions={fleet} sessions={sessions} />
        </HomeCard>
        {assign === undefined ? null : (
          <HomeCard title="Assign">
            <AssignBar actions={assign} onSpawn={onSpawn} />
          </HomeCard>
        )}
        <HomeCard title="Activity">
          <Feed items={feed} onOpen={onOpen} />
        </HomeCard>
      </div>
    </div>
  );
}
