import { useState } from 'react';
import { FleetBoard } from './board/FleetBoard';
import { ALL, emptyFilter, filterFleet, type FleetFilter, stationName } from './board/fleet';
import { StationsPanel } from './board/StationsPanel';
import type { DeckSession, StationView } from './gen';
import type { Freshness } from './runtime/state';

type Props = {
  readonly sessions?: readonly DeckSession[];
  readonly stations?: readonly StationView[];
  readonly freshness?: Freshness;
};

/** Screens read the same runtime snapshot; filters never open another connection. */
export function App({ sessions = [], stations = [], freshness = 'stale' }: Props) {
  const [screen, setScreen] = useState<'fleet' | 'stations'>('fleet');
  const [filter, setFilter] = useState<FleetFilter>(emptyFilter);
  const visible = filterFleet(sessions, stations, filter);
  const stationIds = [
    ...new Set([...stations.map((s) => s.station_id), ...sessions.map((s) => s.station_id)]),
  ].sort();
  return (
    <div className="fd-app">
      <header className="fd-header">
        <span className="fd-wordmark">FLEET DECK</span>
        <nav aria-label="Board screens">
          <button
            type="button"
            aria-pressed={screen === 'fleet'}
            onClick={() => setScreen('fleet')}
          >
            Fleet
          </button>
          <button
            type="button"
            aria-pressed={screen === 'stations'}
            onClick={() => setScreen('stations')}
          >
            Stations ({stations.length})
          </button>
        </nav>
      </header>
      {screen === 'stations' ? (
        <StationsPanel stations={stations} sessions={sessions} freshness={freshness} />
      ) : (
        <>
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
          <FleetBoard sessions={visible} stations={stations} freshness={freshness} />
        </>
      )}
    </div>
  );
}
