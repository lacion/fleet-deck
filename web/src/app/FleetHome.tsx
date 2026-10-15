import {
  type KeyboardEvent,
  type ReactNode,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from 'react';
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
} from '../board/fleet';
import { record } from '../board/activity';
import { FleetBoard, type FleetGroup } from '../board/FleetBoard';
import { NeedsYouRail } from '../board/NeedsYouRail';
import { UsageMeters } from '../board/UsageMeters';
import { limitChip, rateMeters } from '../board/usage';
import type { FleetActions } from '../fleet/fleet';
import { FleetPanel } from '../fleet/FleetPanel';
import type { DeckSession, SessionKey, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import type { SessionActions } from '../session/commands';
import { useProjects } from '../integrations/context';
import { canShare, usePeople } from '../sharing/context';
import { openShare } from '../sharing/shareStore';
import { useDismiss } from '../ui/dismiss';
import { ChevronDown, ChevronUp, CircleHelp, Icon, Plus, Search, Users, X } from '../ui/icons';
import { archived } from '../ui/archive';
import { sessionRef } from '../ui/localSet';

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
const PANEL_DEFAULT: PanelState = { open: false, tab: 'activity' };
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

type Group = FleetGroup;
const GROUP_KEY = 'fd-home-group';

function readGroup(): Group {
  try {
    const saved = localStorage.getItem(GROUP_KEY);
    return saved === 'repo' || saved === 'project' ? saved : 'station';
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

const PROVIDER_NAME: Record<string, string> = {
  claude: 'Claude',
  codex: 'Codex',
  unknown: 'Unknown',
};

/** One active filter, as the removable chip under the toolbar shows it. */
export type ActiveFilter = {
  readonly key: 'station' | 'provider' | 'repo' | 'archived';
  readonly text: string;
};

/** The active filters (not the search): station, provider, repo, and archived shown. */
export function activeFilters(
  filter: FleetFilter,
  archivedOn: boolean,
  stations: readonly StationView[],
): ActiveFilter[] {
  const list: ActiveFilter[] = [];
  if (filter.station !== ALL)
    list.push({ key: 'station', text: `Station: ${stationName(filter.station, stations)}` });
  if (filter.provider !== ALL)
    list.push({
      key: 'provider',
      text: `Provider: ${PROVIDER_NAME[filter.provider] ?? filter.provider}`,
    });
  if (filter.repo !== ALL) list.push({ key: 'repo', text: `Repo: ${filter.repo}` });
  if (archivedOn) list.push({ key: 'archived', text: 'Archived shown' });
  return list;
}

/** The filter with one active filter cleared; the search stays. */
export function clearFilter(filter: FleetFilter, key: ActiveFilter['key']): FleetFilter {
  if (key === 'station') return { ...filter, station: ALL };
  if (key === 'provider') return { ...filter, provider: ALL };
  if (key === 'repo') return { ...filter, repo: ALL };
  return filter;
}

/** The Filters menu: a `<details>` popover that Escape or a press outside closes. */
function FiltersMenu({ active, children }: { active: number; children: ReactNode }) {
  const ref = useRef<HTMLDetailsElement>(null);
  const [open, setOpen] = useState(false);
  const close = () => {
    if (ref.current) ref.current.open = false;
  };
  useDismiss(ref, open, close);
  const onKeyDown = (event: KeyboardEvent<HTMLDetailsElement>) => {
    if (event.key !== 'Escape' || !ref.current?.open) return;
    event.stopPropagation();
    close();
    ref.current.querySelector('summary')?.focus();
  };
  return (
    <details
      className="fd-menu fd-filters-menu"
      ref={ref}
      onKeyDown={onKeyDown}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="fd-ghost fd-sm">{active > 0 ? `Filters (${active})` : 'Filters'}</summary>
      <div className="fd-menu-body fd-filters" role="group" aria-label="Fleet filters">
        {children}
      </div>
    </details>
  );
}

/** The fleet home: toolbar, the strip board, who needs you, and the bottom panel. */
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
  const [picked, setGroup] = useState<Group>(readGroup);
  // DL-176: the grouping "Project" needs the project list of the owner's login.
  const hasProjects = useProjects().length > 0;
  const group: Group = picked === 'project' && !hasProjects ? 'station' : picked;
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

  const chip = (label: string, selected: boolean, pick: () => void, key: string) => (
    <button key={key} type="button" className="fd-chip" aria-pressed={selected} onClick={pick}>
      {label}
    </button>
  );
  const row = (label: string, chips: ReactNode) => (
    <div className="fd-filters-row">
      <span className="fd-label">{label}</span>
      <div className="fd-filters-chips">{chips}</div>
    </div>
  );
  const filterRows = (
    <>
      {row(
        'Station',
        <>
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
            ? chip(
                `Removed station: ${filter.station}`,
                true,
                () => {},
                `removed-${filter.station}`,
              )
            : null}
        </>,
      )}
      {row(
        'Provider',
        <>
          {chip(
            'All',
            filter.provider === ALL,
            () => setFilter({ ...filter, provider: ALL }),
            'p-all',
          )}
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
        </>,
      )}
      {repos.length >= 2 || filter.repo !== ALL
        ? row(
            'Repo',
            <>
              {chip('All', filter.repo === ALL, () => setFilter({ ...filter, repo: ALL }), 'r-all')}
              {[...new Set([...repos, ...(filter.repo === ALL ? [] : [filter.repo])])].map((name) =>
                chip(
                  name,
                  filter.repo === name,
                  () => setFilter({ ...filter, repo: name }),
                  `r-${name}`,
                ),
              )}
            </>,
          )
        : null}
      {archivedCount > 0
        ? row(
            'Archived',
            chip(
              `Archived (${archivedCount})`,
              showArchived,
              () => setArchivedOn(!archivedOn),
              'archived',
            ),
          )
        : null}
    </>
  );
  const limit = limitChip(rateMeters(sessions, stations));
  const active = activeFilters(filter, showArchived, stations);
  const pickGroup = (next: Group) => {
    setGroup(next);
    writeGroup(next);
  };
  const header = (
    <header className="fd-home-head">
      <h1 className="fd-page-title">Fleet</h1>
      <p className="fd-home-counts">
        <span>{sessions.length === 1 ? '1 session' : `${sessions.length} sessions`}</span>
        {needsYou === 0 ? (
          <span className="fd-home-needs" data-clear="true">
            All clear
          </span>
        ) : (
          <span className="fd-home-needs">
            {needsYou === 1 ? '1 needs you' : `${needsYou} need you`}
          </span>
        )}
        <span>{stationCount === 1 ? '1 station' : `${stationCount} stations`}</span>
        {conflictCount > 0 ? (
          <span className="fd-home-conf">
            {conflictCount === 1 ? '1 conflict' : `${conflictCount} conflicts`}
          </span>
        ) : null}
        {limit === null ? null : (
          <span className="fd-badge" data-tone={limit.tone}>
            {limit.text}
          </span>
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
      <FiltersMenu active={active.length}>{filterRows}</FiltersMenu>
      {phone ? null : (
        <select
          aria-label="Group by"
          className="fd-sm"
          value={group}
          onChange={(event) => pickGroup(event.target.value as Group)}
        >
          <option value="station">Station</option>
          <option value="repo">Repo</option>
          {hasProjects ? <option value="project">Project</option> : null}
        </select>
      )}
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
  const activeRow =
    active.length === 0 && !filtered ? null : (
      <div className="fd-active-filters">
        {active.map((a) => (
          <button
            key={a.key}
            type="button"
            className="fd-chip"
            aria-pressed="true"
            onClick={() => {
              if (a.key === 'archived') setArchivedOn(false);
              else setFilter(clearFilter(filter, a.key));
            }}
          >
            {a.text} <Icon of={X} size={12} />
          </button>
        ))}
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

  const waiting = sessions.some((s) => s.summary.open_requests.length > 0);
  if (phone) {
    return (
      <div className="fd-home" data-layout="phone">
        {header}
        {radar}
        {activeRow}
        {rail('strip')}
        {empty ?? (
          <FleetBoard
            sessions={visible}
            stations={stations}
            freshness={freshness}
            group="station"
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
    <div className="fd-home" data-layout={layout} data-rail={waiting ? 'true' : 'false'}>
      <div className="fd-home-main">
        {header}
        {radar}
        {activeRow}
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
