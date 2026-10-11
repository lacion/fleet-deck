import { useEffect, useState } from 'react';
import { FleetBoard } from './board/FleetBoard';
import { NeedsYouRail } from './board/NeedsYouRail';
import { ALL, emptyFilter, filterFleet, type FleetFilter, stationName } from './board/fleet';
import type { StationActions } from './board/pairing';
import { StationsPanel } from './board/StationsPanel';
import type { DeckSession, SessionKey, StationView, WorkspaceListing } from './gen';
import { type Freshness, sessionKey, type Timeline } from './runtime/state';
import type { SessionActions } from './session/commands';
import { SessionView } from './session/SessionView';
import type { SpawnActions } from './spawn/spawn';
import { SpawnForm } from './spawn/SpawnForm';

type Props = {
  readonly sessions?: readonly DeckSession[];
  readonly stations?: readonly StationView[];
  readonly freshness?: Freshness;
  readonly actions?: StationActions | undefined;
  readonly timelines?: Readonly<Record<string, Timeline>>;
  /** Opens the live timeline of a session; returns the cancel function. */
  readonly watch?: ((key: SessionKey) => () => void) | undefined;
  readonly sessionActions?: ((key: SessionKey) => SessionActions) | undefined;
  readonly spawn?: SpawnActions | undefined;
  /** The session open at the first render (tests). */
  readonly initialSession?: SessionKey | null;
  /** The screen at the first render (tests). */
  readonly initialScreen?: Screen;
  /** The spawn form's directory listing at the first render (tests). */
  readonly initialListing?: WorkspaceListing | null;
};

type Screen = 'fleet' | 'stations' | 'spawn';

/** Screens read the same runtime snapshot; filters never open another connection. */
export function App({
  sessions = [],
  stations = [],
  freshness = 'stale',
  actions,
  timelines = {},
  watch,
  sessionActions,
  spawn,
  initialSession = null,
  initialScreen = 'fleet',
  initialListing = null,
}: Props) {
  const [screen, setScreen] = useState<Screen>(initialScreen);
  const [open, setOpen] = useState<SessionKey | null>(initialSession);
  const openId = open === null ? null : sessionKey(open);
  // `openId` identifies the session; a new `open` object with the same key
  // must not reopen the timeline.
  useEffect(() => {
    if (open === null || watch === undefined) return undefined;
    return watch(open);
  }, [openId, watch]);
  const selected =
    open === null
      ? undefined
      : sessions.find(
          (s) => s.station_id === open.station_id && s.summary.session_id === open.session_id,
        );
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
            onClick={() => {
              setOpen(null);
              setScreen('fleet');
            }}
          >
            Fleet
          </button>
          <button
            type="button"
            aria-pressed={screen === 'stations'}
            onClick={() => {
              setOpen(null);
              setScreen('stations');
            }}
          >
            Stations ({stations.length})
          </button>
          <button
            type="button"
            aria-pressed={screen === 'spawn'}
            onClick={() => {
              setOpen(null);
              setScreen('spawn');
            }}
          >
            New session
          </button>
        </nav>
      </header>
      {selected !== undefined && open !== null && openId !== null ? (
        <SessionView
          key={openId}
          session={selected}
          stations={stations}
          timeline={timelines[openId]}
          freshness={freshness}
          actions={sessionActions?.(open)}
          onBack={() => setOpen(null)}
        />
      ) : screen === 'spawn' ? (
        <SpawnForm
          stations={stations}
          freshness={freshness}
          actions={spawn}
          initialListing={initialListing}
          onOpen={(key) => {
            setScreen('fleet');
            setOpen(key);
          }}
        />
      ) : screen === 'stations' ? (
        <StationsPanel
          stations={stations}
          sessions={sessions}
          freshness={freshness}
          actions={actions}
        />
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
          <NeedsYouRail sessions={sessions} stations={stations} onOpen={setOpen} />
          <FleetBoard
            sessions={visible}
            stations={stations}
            freshness={freshness}
            onOpen={setOpen}
          />
        </>
      )}
    </div>
  );
}
