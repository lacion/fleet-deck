import { useEffect, useState } from 'react';
import { stationName } from '../board/fleet';
import { sessionTitle } from '../app/sessions';
import type { ArtifactKind, DeckArtifact, DeckSession, StationView } from '../gen';
import { useLayout } from '../app/viewport';
import { Icon, ListFilter, Paperclip, Search, X } from '../ui/icons';
import { Menu } from '../ui/Menu';
import { EmptyState, Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import type { ArtifactActions } from './actions';
import { ArtifactList, visible } from './ArtifactList';
import { KIND_LABEL, KINDS } from './kinds';
import { type ListState, useArtifactList } from './useArtifactList';

const SEARCH_MS = 250;
/** The summary sentence under the title (DESIGN-SYSTEM §4.3). */
const ARTIFACTS_DESC = 'Reports, diagrams and previews that sessions published.';

type Filters = {
  readonly q: string;
  readonly kind: ArtifactKind | 'all';
  readonly station: string;
  readonly session: string;
};

/** The list of one state: loading, error with "Try again", empty, no match, or the rows. */
export function ArtifactsListBody({
  state,
  filtered,
  now,
  where,
  onRetry,
}: {
  readonly state: ListState;
  readonly filtered: boolean;
  readonly now: number;
  readonly where: (a: DeckArtifact) => string;
  readonly onRetry: () => void;
}) {
  if (state.kind === 'loading') {
    return (
      <p role="status" className="fd-note">
        Loading the artifacts…
      </p>
    );
  }
  if (state.kind === 'error') {
    return (
      <div className="fd-art-state">
        <p role="alert" className="fd-note fd-art-error">
          {state.text}
        </p>
        <button type="button" className="fd-ghost fd-sm" onClick={onRetry}>
          Try again
        </button>
      </div>
    );
  }
  if (state.items.length === 0) {
    return (
      <EmptyState
        icon={Paperclip}
        title={filtered ? 'No artifact matches' : 'No artifacts yet'}
        body={
          filtered
            ? 'Change the search or a filter to see more.'
            : 'A session publishes one, or you publish a file from its Files tab.'
        }
      />
    );
  }
  return <ArtifactList items={state.items} now={now} where={where} />;
}

type Props = {
  readonly actions: ArtifactActions | undefined;
  readonly stations: readonly StationView[];
  readonly sessions: readonly DeckSession[];
  /** Unix ms of "now" (tests). */
  readonly now?: number;
  /** The list at the first render, so that a static render shows it (tests). */
  readonly initial?: ListState | undefined;
};

/** The global Artifacts page (`#/artifacts`): search by title, kind, station and session. */
export function ArtifactsPage({ actions, stations, sessions, now: givenNow, initial }: Props) {
  const now = useNow(givenNow, 30_000);
  const [filters, setFilters] = useState<Filters>({
    q: '',
    kind: 'all',
    station: '',
    session: '',
  });
  // The query goes to the deck after a pause, not on every key.
  const [q, setQ] = useState('');
  useEffect(() => {
    const timer = setTimeout(() => setQ(filters.q), SEARCH_MS);
    return () => clearTimeout(timer);
  }, [filters.q]);
  const [retry, setRetry] = useState(0);
  if (actions === undefined) {
    return (
      <Page title="Artifacts" description={ARTIFACTS_DESC} className="fd-art-page">
        <p className="fd-note">Artifacts need a live board.</p>
      </Page>
    );
  }
  return (
    <Loaded
      actions={actions}
      stations={stations}
      sessions={sessions}
      now={now}
      filters={filters}
      setFilters={setFilters}
      q={q}
      retry={retry}
      onRetry={() => setRetry((n) => n + 1)}
      initial={initial}
    />
  );
}

function Loaded({
  actions,
  stations,
  sessions,
  now,
  filters,
  setFilters,
  q,
  retry,
  onRetry,
  initial,
}: Omit<Props, 'now' | 'actions'> & {
  readonly actions: ArtifactActions;
  readonly now: number;
  readonly filters: Filters;
  readonly setFilters: (next: Filters) => void;
  readonly q: string;
  readonly retry: number;
  readonly onRetry: () => void;
}) {
  const layout = useLayout();
  const fetched = useArtifactList(
    actions,
    { station: filters.station, session: filters.session, q },
    0,
    retry,
  );
  const state: ListState =
    initial !== undefined
      ? initial
      : fetched.kind === 'ok'
        ? { kind: 'ok', items: visible(fetched.items, filters.kind) }
        : fetched;
  const stationSessions = sessions.filter(
    (s) => filters.station === '' || s.station_id === filters.station,
  );
  const filtered =
    filters.q.trim() !== '' ||
    filters.kind !== 'all' ||
    filters.station !== '' ||
    filters.session !== '';
  const where = (a: DeckArtifact): string => {
    const card =
      a.session_id === null
        ? undefined
        : sessions.find(
            (s) => s.station_id === a.station_id && s.summary.session_id === a.session_id,
          );
    const station = a.station_name || stationName(a.station_id, stations);
    return card === undefined ? station : `${station} · ${sessionTitle(card)}`;
  };
  // A phone shows the search and one Filters menu with the three selects; the chosen filters show
  // as removable pressed chips under the search (the fleet's FiltersMenu pattern).
  const phone = layout === 'phone';
  const session = stationSessions.find((s) => s.summary.session_id === filters.session);
  const chosen: readonly { key: 'kind' | 'station' | 'session'; text: string }[] = [
    ...(filters.kind === 'all' ? [] : [{ key: 'kind' as const, text: KIND_LABEL[filters.kind] }]),
    ...(filters.station === ''
      ? []
      : [{ key: 'station' as const, text: stationName(filters.station, stations) }]),
    ...(filters.session === ''
      ? []
      : [
          {
            key: 'session' as const,
            text: session === undefined ? filters.session : sessionTitle(session),
          },
        ]),
  ];
  // A select of the header (§4.3): a quiet ghost select, marked while it filters.
  const select = (
    label: string,
    value: string,
    set: (v: string) => void,
    options: readonly { value: string; label: string }[],
  ) => (
    <label key={label} className="fd-art-filter">
      <span className={phone ? 'fd-label' : 'fd-vh'}>{label}</span>
      <select
        className={phone ? undefined : 'fd-ghost fd-sm'}
        data-active={!phone && value !== options[0]?.value ? 'true' : undefined}
        value={value}
        onChange={(e) => set(e.target.value)}
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </label>
  );
  const selects = [
    select(
      'Kind',
      filters.kind,
      (kind) => setFilters({ ...filters, kind: kind as Filters['kind'] }),
      [
        { value: 'all', label: 'All kinds' },
        ...KINDS.map((k) => ({ value: k, label: KIND_LABEL[k] })),
      ],
    ),
    select(
      'Station',
      filters.station,
      (station) => setFilters({ ...filters, station, session: '' }),
      [
        { value: '', label: 'All stations' },
        ...stations.map((s) => ({ value: s.station_id, label: s.name })),
      ],
    ),
    select('Session', filters.session, (s) => setFilters({ ...filters, session: s }), [
      { value: '', label: 'All sessions' },
      ...stationSessions.map((s) => ({ value: s.summary.session_id, label: sessionTitle(s) })),
    ]),
  ];
  const filterForm = (
    <form
      className="fd-art-filters"
      aria-label="Filter the artifacts"
      data-layout={phone ? 'phone' : undefined}
      onSubmit={(e) => e.preventDefault()}
    >
      <label className="fd-field fd-art-search">
        <span className="fd-vh">Search by title</span>
        <Icon of={Search} size={14} />
        <input
          type="search"
          className={phone ? undefined : 'fd-sm'}
          aria-label="Search by title"
          placeholder="Search by title"
          autoComplete="off"
          value={filters.q}
          onChange={(e) => setFilters({ ...filters, q: e.target.value })}
        />
      </label>
      {phone ? (
        <span className="fd-art-filtermenu">
          <Menu
            summary={
              <>
                <Icon of={ListFilter} size={14} />
                {chosen.length > 0 ? `Filters (${chosen.length})` : 'Filters'}
              </>
            }
            summaryClass="fd-ghost"
          >
            {() => (
              <div className="fd-art-filtergrid" role="group" aria-label="Artifact filters">
                {selects}
              </div>
            )}
          </Menu>
        </span>
      ) : (
        selects
      )}
      {phone && chosen.length > 0 ? (
        <div className="fd-art-chosen">
          {chosen.map((c) => (
            <button
              key={c.key}
              type="button"
              className="fd-chip"
              aria-pressed="true"
              aria-label={`Remove the filter ${c.text}`}
              onClick={() =>
                setFilters(
                  c.key === 'kind'
                    ? { ...filters, kind: 'all' }
                    : c.key === 'station'
                      ? { ...filters, station: '', session: '' }
                      : { ...filters, session: '' },
                )
              }
            >
              {c.text} <Icon of={X} size={12} />
            </button>
          ))}
        </div>
      ) : null}
    </form>
  );
  return (
    <Page
      title="Artifacts"
      description={ARTIFACTS_DESC}
      className="fd-art-page"
      filters={filterForm}
      wide
    >
      <ArtifactsListBody
        state={state}
        filtered={filtered}
        now={now}
        where={where}
        onRetry={onRetry}
      />
    </Page>
  );
}
