import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import type { AssignActions } from '../assign/assign';
import { AssignBar } from '../assign/AssignBar';
import { ConflictRadar, useConflicts } from '../board/ConflictRadar';
import type { ConflictLoader } from '../board/conflicts';
import type { FleetExtras, SessionExtras } from '../gen';
import type { FeedItem } from '../board/feed';
import { Feed } from '../board/FeedPanel';
import {
  ALL,
  archivedHidden,
  contestedBy,
  emptyFilter,
  type FleetFilter,
  filterFleet,
  repoNames,
  stationName,
  statusGroups,
} from '../board/fleet';
import { record } from '../board/activity';
import { COLUMNS, columnCards, FleetBoard, FleetCard, OfflineTools } from '../board/FleetBoard';
import { batchCountOf, batchCounts } from '../compare/compare';
import { NeedsYouRail } from '../board/NeedsYouRail';
import { UsageMeters } from '../board/UsageMeters';
import { limitChip, rateMeters } from '../board/usage';
import type { FleetActions } from '../fleet/fleet';
import { FleetPanel } from '../fleet/FleetPanel';
import type { DeckSession, SessionKey, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import type { SessionActions } from '../session/commands';
import { canShare, usePeople } from '../sharing/context';
import { openShare } from '../sharing/shareStore';
import { ChevronDown, ChevronUp, CircleHelp, Icon, Plus, Search, Users } from '../ui/icons';
import { archived } from '../ui/archive';
import { priority, sessionRef } from '../ui/localSet';
import { useNow } from '../ui/useNow';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly assign: AssignActions | undefined;
  readonly fleet: FleetActions | undefined;
  readonly conflicts: ConflictLoader | undefined;
  readonly feed: readonly FeedItem[];
  readonly layout: 'desktop' | 'tablet' | 'phone';
  readonly sessionActions?: ((key: SessionKey) => SessionActions) | undefined;
  readonly onOpen: (key: SessionKey) => void;
  readonly onSpawn: () => void;
  /** DL-141: open the bottom panel on `tab`; a new `seq` asks again (the `c` key). */
  readonly panelRequest?:
    | { readonly tab: 'activity' | 'mail' | 'assign' | 'usage'; readonly seq: number }
    | undefined;
  /** DL-141: open the help overlay (the empty board's "What is this?"). */
  readonly onHelp?: (() => void) | undefined;
  /** DL-141: start a session with this first prompt on this station (an unrouted task). */
  readonly onSpawnWith?: ((station: string | null, prompt: string) => void) | undefined;
  /** DL-141: loads `GET /api/fleet/extras` (the card chips); absent in tests without a deck. */
  readonly extras?: (() => Promise<FleetExtras>) | undefined;
};

/** Changes when any session changes, so derived panels load again. */
function fleetVersion(sessions: readonly DeckSession[]): string {
  let newest = 0;
  for (const s of sessions) newest = Math.max(newest, s.summary.updated_at);
  return `${sessions.length}/${newest}`;
}

/** How often the card extras load again while the home is open. */
const EXTRAS_MS = 15_000;

/** The card extras by `sessionRef`; a failed load keeps the last good value. */
function useExtras(
  load: (() => Promise<FleetExtras>) | undefined,
): ReadonlyMap<string, SessionExtras> | undefined {
  const [map, setMap] = useState<ReadonlyMap<string, SessionExtras> | undefined>(undefined);
  const loadRef = useRef(load);
  loadRef.current = load;
  const active = load !== undefined;
  useEffect(() => {
    if (!active) return undefined;
    let live = true;
    const run = () => {
      loadRef
        .current?.()
        .then((value) => {
          if (live)
            setMap(new Map(value.sessions.map((x) => [sessionRef(x.station_id, x.session_id), x])));
        })
        .catch(() => {});
    };
    run();
    const timer = setInterval(run, EXTRAS_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [active]);
  return map;
}

const WIDE = '(min-width: 1280px)';

/** True from 1280 px: the needs-you rail is a side column. Server and test snapshot: true. */
function useWide(): boolean {
  return useSyncExternalStore(
    (notify) => {
      const query = window.matchMedia(WIDE);
      query.addEventListener('change', notify);
      return () => query.removeEventListener('change', notify);
    },
    () => window.matchMedia(WIDE).matches,
    () => true,
  );
}

type PanelTab = 'activity' | 'mail' | 'assign' | 'usage';
type PanelState = { readonly open: boolean; readonly tab: PanelTab };
const PANEL_KEY = 'fd-home-panel';
const PANEL_DEFAULT: PanelState = { open: true, tab: 'activity' };
const PANEL_TABS: readonly PanelTab[] = ['activity', 'mail', 'assign', 'usage'];

function readPanel(hasAssign: boolean): PanelState {
  try {
    const raw = localStorage.getItem(PANEL_KEY);
    if (raw === null) return PANEL_DEFAULT;
    const v: unknown = JSON.parse(raw);
    if (typeof v !== 'object' || v === null) return PANEL_DEFAULT;
    const { open, tab } = v as { open?: unknown; tab?: unknown };
    if (typeof open !== 'boolean' || !PANEL_TABS.includes(tab as PanelTab)) return PANEL_DEFAULT;
    if (tab === 'assign' && !hasAssign) return PANEL_DEFAULT;
    return { open, tab: tab as PanelTab };
  } catch {
    return PANEL_DEFAULT;
  }
}

function writePanel(state: PanelState) {
  try {
    localStorage.setItem(PANEL_KEY, JSON.stringify(state));
  } catch {
    // Storage is blocked: the panel still works for this visit.
  }
}

type Group = 'station' | 'repo';
const GROUP_KEY = 'fd-home-group';

function readGroup(): Group {
  try {
    return localStorage.getItem(GROUP_KEY) === 'repo' ? 'repo' : 'station';
  } catch {
    return 'station';
  }
}

function writeGroup(group: Group) {
  try {
    localStorage.setItem(GROUP_KEY, group);
  } catch {
    // Storage is blocked: the grouping still works for this visit.
  }
}

const PANEL_LABEL: Record<PanelTab, string> = {
  activity: 'Activity',
  mail: 'Mail & tasks',
  assign: 'Assign',
  usage: 'Usage',
};

/** The fleet home: header, filters, the columns, who needs you, and the bottom panel. */
export function FleetHome({
  sessions,
  stations,
  freshness,
  assign,
  fleet,
  conflicts,
  feed,
  layout,
  sessionActions,
  onOpen,
  onSpawn,
  panelRequest,
  onHelp,
  onSpawnWith,
  extras,
}: Props) {
  const cardExtras = useExtras(extras);
  // R11.4: a guest has no new session and no mail; the owner can share the whole board.
  const people = usePeople();
  const guest = people.member !== null;
  const [, setActivity] = useState(0);
  // The cards read the buckets while they render, so a record asks for one more render.
  useEffect(() => {
    record(sessions, Date.now());
    setActivity((n) => n + 1);
  }, [sessions]);
  const [filter, setFilter] = useState<FleetFilter>(emptyFilter);
  const [group, setGroup] = useState<Group>(readGroup);
  const [archivedOn, setArchivedOn] = useState(false);
  const cleared = archived.use();
  const { list, problem } = useConflicts(conflicts, fleetVersion(sessions));
  const wide = useWide();
  const phone = layout === 'phone';
  const archivedCount = sessions.filter((s) => archivedHidden(s, cleared)).length;
  const showArchived = archivedOn && archivedCount > 0;
  const shown = showArchived ? sessions : sessions.filter((s) => !archivedHidden(s, cleared));
  const visible = filterFleet(shown, stations, filter);
  const repos = repoNames(shown);
  const contested = contestedBy(list ?? []);
  const conflictCount = list?.length ?? 0;
  const stationIds = [
    ...new Set([...stations.map((s) => s.station_id), ...sessions.map((s) => s.station_id)]),
  ].sort();
  const needsYou = sessions.filter((s) => s.status === 'needs_you').length;
  const stationCount = stations.length;
  const unknownProvider = sessions.some(
    (s) => s.summary.provider === null || s.summary.provider === 'unknown',
  );
  const filtered =
    filter.station !== ALL ||
    filter.provider !== ALL ||
    filter.repo !== ALL ||
    filter.search.trim() !== '';

  const limit = limitChip(rateMeters(sessions, stations));
  const header = (
    <header className="fd-home-head">
      <h1 className="fd-wordmark">FLEET</h1>
      <p className="fd-home-counts">
        {sessions.length === 1 ? '1 session' : `${sessions.length} sessions`} ·{' '}
        {needsYou === 1 ? '1 needs you' : `${needsYou} need you`} ·{' '}
        {stationCount === 1 ? '1 station' : `${stationCount} stations`}
        {conflictCount > 0 ? (
          <>
            {' · '}
            <span className="fd-home-conf">
              {conflictCount === 1 ? '1 conflict' : `${conflictCount} conflicts`}
            </span>
          </>
        ) : null}
        {limit === null ? null : (
          <>
            {' · '}
            <span className="fd-badge" data-tone={limit.tone}>
              {limit.text}
            </span>
          </>
        )}
      </p>
      <div className="fd-field">
        <Icon of={Search} />
        <input
          type="search"
          aria-label="Filter sessions"
          placeholder="Filter sessions"
          value={filter.search}
          onChange={(event) => setFilter({ ...filter, search: event.target.value })}
        />
      </div>
      {canShare(people) ? (
        <button
          type="button"
          className="fd-ghost fd-sm"
          onClick={() =>
            openShare({ kind: 'board', target: '', label: 'Every session of the deck' })
          }
        >
          <Icon of={Users} size={14} /> Share board…
        </button>
      ) : null}
    </header>
  );
  const radar =
    conflicts === undefined ? null : (
      <ConflictRadar list={list} problem={problem} stations={stations} onOpen={onOpen} />
    );
  const chip = (label: string, selected: boolean, pick: () => void, key: string) => (
    <button key={key} type="button" className="fd-chip" aria-pressed={selected} onClick={pick}>
      {label}
    </button>
  );
  const pickGroup = (next: Group) => {
    setGroup(next);
    writeGroup(next);
  };
  const filters = (
    <div className="fd-filters" role="group" aria-label="Fleet filters">
      <span className="fd-label">Station</span>
      {chip('All', filter.station === ALL, () => setFilter({ ...filter, station: ALL }), 'all')}
      {stationIds.map((id) =>
        chip(
          stationName(id, stations),
          filter.station === id,
          () => setFilter({ ...filter, station: id }),
          id,
        ),
      )}
      {filter.station !== ALL && !stationIds.includes(filter.station)
        ? chip(`Removed station: ${filter.station}`, true, () => {}, `removed-${filter.station}`)
        : null}
      <span className="fd-label">Provider</span>
      {chip('All', filter.provider === ALL, () => setFilter({ ...filter, provider: ALL }), 'p-all')}
      {chip(
        'Claude',
        filter.provider === 'claude',
        () => setFilter({ ...filter, provider: 'claude' }),
        'claude',
      )}
      {chip(
        'Codex',
        filter.provider === 'codex',
        () => setFilter({ ...filter, provider: 'codex' }),
        'codex',
      )}
      {unknownProvider
        ? chip(
            'Unknown',
            filter.provider === 'unknown',
            () => setFilter({ ...filter, provider: 'unknown' }),
            'unknown',
          )
        : null}
      {repos.length >= 2 || filter.repo !== ALL ? (
        <>
          <span className="fd-label">Repo</span>
          {chip('All', filter.repo === ALL, () => setFilter({ ...filter, repo: ALL }), 'r-all')}
          {[...new Set([...repos, ...(filter.repo === ALL ? [] : [filter.repo])])].map((name) =>
            chip(
              name,
              filter.repo === name,
              () => setFilter({ ...filter, repo: name }),
              `r-${name}`,
            ),
          )}
        </>
      ) : null}
      {archivedCount > 0
        ? chip(
            `Archived (${archivedCount})`,
            showArchived,
            () => setArchivedOn(!archivedOn),
            'archived',
          )
        : null}
      {phone ? null : (
        <>
          <span className="fd-label">Group</span>
          {chip('Station', group === 'station', () => pickGroup('station'), 'g-station')}
          {chip('Repo', group === 'repo', () => pickGroup('repo'), 'g-repo')}
        </>
      )}
      {filtered ? (
        <output className="fd-count fd-filters-count" aria-live="polite">
          {visible.length} of {shown.length}
        </output>
      ) : null}
    </div>
  );
  const rail = (variant: 'side' | 'strip') => (
    <NeedsYouRail
      sessions={sessions}
      stations={stations}
      onOpen={onOpen}
      sessionActions={sessionActions}
      variant={variant}
      freshness={freshness}
    />
  );
  const empty =
    visible.length === 0 ? (
      <div className="fd-lanes-empty">
        <p className="fd-empty">
          {sessions.length === 0
            ? guest
              ? 'No shared sessions yet.'
              : 'No sessions yet. Start one with New session.'
            : 'No session matches the filters.'}
        </p>
        <div className="fd-lanes-empty-actions">
          {guest ? null : (
            <button type="button" className="fd-primary" onClick={onSpawn}>
              <Icon of={Plus} /> New session
            </button>
          )}
          {onHelp === undefined ? null : (
            <button type="button" className="fd-ghost" onClick={onHelp}>
              <Icon of={CircleHelp} /> What is this?
            </button>
          )}
        </div>
      </div>
    ) : null;
  const panel = (
    <HomePanel
      hasAssign={assign !== undefined}
      guest={guest}
      stations={stations}
      sessions={sessions}
      visible={visible}
      feed={feed}
      fleet={fleet}
      assign={assign}
      onOpen={onOpen}
      onSpawn={(stationId, text) =>
        onSpawnWith === undefined
          ? onSpawn()
          : onSpawnWith(stationId === '' ? null : stationId, text)
      }
      request={panelRequest}
    />
  );

  if (phone) {
    return (
      <div className="fd-home" data-layout="phone">
        {header}
        {radar}
        {filters}
        {rail('strip')}
        {empty ?? (
          <PhoneColumns
            visible={visible}
            stations={stations}
            freshness={freshness}
            contested={contested}
            sessionActions={sessionActions}
            extras={cardExtras}
          />
        )}
        {panel}
      </div>
    );
  }
  return (
    <div className="fd-home" data-layout={layout}>
      <div className="fd-home-main">
        {header}
        {radar}
        {filters}
        {wide ? null : rail('strip')}
        <div className="fd-lanes">
          <FleetBoard
            sessions={visible}
            stations={stations}
            freshness={freshness}
            group={group}
            contested={contested}
            sessionActions={sessionActions}
            extras={cardExtras}
          />
          {empty}
        </div>
        {panel}
      </div>
      {wide ? rail('side') : null}
    </div>
  );
}

/** Phone: one section for each column that has a card. */
function PhoneColumns({
  visible,
  stations,
  freshness,
  contested,
  sessionActions,
  extras,
}: {
  visible: readonly DeckSession[];
  stations: readonly StationView[];
  freshness: Freshness;
  contested: ReturnType<typeof contestedBy>;
  sessionActions: ((key: SessionKey) => SessionActions) | undefined;
  extras: ReadonlyMap<string, SessionExtras> | undefined;
}) {
  const now = useNow(undefined, 30000);
  const starred = priority.use();
  const groups = statusGroups(visible);
  const batches = batchCounts(visible);
  return (
    <>
      {COLUMNS.map((column) => {
        const cards = columnCards(groups, column.statuses, starred);
        if (cards.length === 0) return null;
        return (
          <section key={column.id} className="fd-phone-col" aria-label={column.label}>
            <h2 className="fd-label">
              {column.label} <span className="fd-count">{cards.length}</span>
              {column.id === 'offline' ? (
                <OfflineTools cards={cards} freshness={freshness} sessionActions={sessionActions} />
              ) : null}
            </h2>
            <ul className="fd-col">
              {cards.map((card) => (
                <FleetCard
                  key={`${card.station_id}/${card.summary.session_id}`}
                  card={card}
                  stations={stations}
                  freshness={freshness}
                  now={now}
                  showStation
                  contested={contested.get(sessionRef(card.station_id, card.summary.session_id))}
                  extra={extras?.get(sessionRef(card.station_id, card.summary.session_id))}
                  batchCount={batchCountOf(card, batches)}
                />
              ))}
            </ul>
          </section>
        );
      })}
    </>
  );
}

/** The bottom panel: activity, mail and tasks, assign, usage. */
function HomePanel({
  hasAssign,
  guest,
  stations,
  sessions,
  visible,
  feed,
  fleet,
  assign,
  onOpen,
  onSpawn,
  request,
}: {
  hasAssign: boolean;
  /** A guest has no mail and no assign tab (the deck refuses the calls). */
  guest: boolean;
  stations: readonly StationView[];
  sessions: readonly DeckSession[];
  visible: readonly DeckSession[];
  feed: readonly FeedItem[];
  fleet: FleetActions | undefined;
  assign: AssignActions | undefined;
  onOpen: (key: SessionKey) => void;
  onSpawn: (stationId: string, text: string) => void;
  request: Props['panelRequest'];
}) {
  const [state, setState] = useState<PanelState>(() => readPanel(hasAssign));
  const seen = useRef<number | null>(null);
  const seq = request?.seq;
  const asked = request?.tab;
  useEffect(() => {
    if (seq === undefined || asked === undefined || seq === seen.current) return;
    seen.current = seq;
    const next: PanelState = {
      open: true,
      tab: asked === 'assign' && !hasAssign ? 'activity' : asked,
    };
    setState(next);
    writePanel(next);
  }, [seq, asked, hasAssign]);
  const set = (next: PanelState) => {
    setState(next);
    writePanel(next);
  };
  const tab: PanelTab =
    (state.tab === 'assign' && !hasAssign) || (guest && state.tab === 'mail')
      ? 'activity'
      : state.tab;
  const tabs = PANEL_TABS.filter((t) => (t !== 'assign' || hasAssign) && !(guest && t === 'mail'));
  return (
    <section className="fd-homepanel" data-open={state.open} aria-label="Fleet panel">
      <div className="fd-homepanel-head">
        <div role="tablist" aria-label="Fleet panels" className="fd-homepanel-tabs">
          {tabs.map((t) => (
            <button
              key={t}
              type="button"
              role="tab"
              className="fd-chip"
              aria-selected={state.open && tab === t}
              aria-controls="fd-homepanel-body"
              onClick={() => set({ open: true, tab: t })}
            >
              {PANEL_LABEL[t]}
            </button>
          ))}
        </div>
        <button
          type="button"
          className="fd-iconbtn fd-sm"
          aria-expanded={state.open}
          aria-controls="fd-homepanel-body"
          aria-label={state.open ? 'Collapse panel' : 'Expand panel'}
          onClick={() => set({ ...state, open: !state.open })}
        >
          <Icon of={state.open ? ChevronDown : ChevronUp} />
        </button>
      </div>
      <div
        id="fd-homepanel-body"
        className="fd-homepanel-body"
        role="tabpanel"
        hidden={!state.open}
      >
        {tab === 'activity' ? <Feed items={feed} onOpen={onOpen} /> : null}
        {tab === 'mail' && !guest ? (
          <FleetPanel actions={fleet} sessions={sessions} stations={stations} />
        ) : null}
        {tab === 'assign' && assign !== undefined ? (
          <AssignBar actions={assign} onSpawn={onSpawn} />
        ) : null}
        {tab === 'usage' ? <UsageMeters sessions={visible} stations={stations} /> : null}
      </div>
    </section>
  );
}
