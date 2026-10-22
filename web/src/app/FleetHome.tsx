import { type KeyboardEvent, type ReactNode, useEffect, useRef, useState } from 'react';
import type { AssignActions } from '../assign/assign';
import { ConflictRadar, useConflicts } from '../board/ConflictRadar';
import type { ConflictLoader } from '../board/conflicts';
import type { FleetExtras, SessionExtras } from '../gen';
import type { FeedItem } from '../board/feed';
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
import { HomePanel, type PanelTab, useWideOps } from '../board/HomePanel';
import { NeedsYouRail } from '../board/NeedsYouRail';
import { UsageGlance } from '../board/UsageGlance';
import { limitChip, rateMeters } from '../board/usage';
import type { FleetActions } from '../fleet/fleet';
import type { DeckSession, SessionKey, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import type { SessionActions } from '../session/commands';
import { useProjects } from '../integrations/context';
import { canShare, usePeople } from '../sharing/context';
import { openShare } from '../sharing/shareStore';
import { useDismiss } from '../ui/dismiss';
import { CircleHelp, Icon, ListFilter, Plus, Search, Users, X } from '../ui/icons';
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
  /** DL-141: open the ops panel on `tab`; a new `seq` asks again (the `c` key). */
  readonly panelRequest?: { readonly tab: PanelTab; readonly seq: number } | undefined;
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

const PROVIDER_NAME: Record<string, string> = {
  claude: 'Claude',
  codex: 'Codex',
  unknown: 'Unknown',
};

/** The status filter of the page header (DESIGN-SYSTEM §4.3); not stored. */
export type StatusFilter = 'all' | 'working' | 'idle' | 'needs_you';

const STATUS_FILTERS: readonly { readonly id: StatusFilter; readonly label: string }[] = [
  { id: 'all', label: 'All' },
  { id: 'working', label: 'Working' },
  { id: 'idle', label: 'Idle' },
  { id: 'needs_you', label: 'Needs you' },
];

/** True when `s` passes the status filter: Working holds a running, checking or queued
 * session; Needs you holds a session with an open request. */
export function statusMatches(s: DeckSession, status: StatusFilter): boolean {
  if (status === 'all') return true;
  if (status === 'needs_you') return s.status === 'needs_you' || s.summary.open_requests.length > 0;
  if (status === 'idle') return s.status === 'idle';
  return s.status === 'working' || s.status === 'verifying' || s.status === 'queued';
}

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
      <summary className="fd-ghost fd-sm">
        <Icon of={ListFilter} size={14} />
        {active > 0 ? `Filters (${active})` : 'Filters'}
      </summary>
      <div className="fd-menu-body fd-filters" role="group" aria-label="Fleet filters">
        {children}
      </div>
    </details>
  );
}

/** The fleet home (DESIGN-SYSTEM §4.4): the header, who needs you, the strip board, and the ops panel. */
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
  const [status, setStatus] = useState<StatusFilter>('all');
  const [bellSlot, setBellSlot] = useState<HTMLElement | null>(null);
  const [picked, setGroup] = useState<Group>(readGroup);
  // DL-176: the grouping "Project" needs the project list of the owner's login.
  const hasProjects = useProjects().length > 0;
  const group: Group = picked === 'project' && !hasProjects ? 'station' : picked;
  const [archivedOn, setArchivedOn] = useState(false);
  const cleared = archived.use();
  const { list, problem } = useConflicts(conflicts, fleetVersion(sessions));
  const [openTab, setOpenTab] = useState<PanelTab | null>(null);
  const phone = layout === 'phone';
  const wide = useWideOps() && !phone;
  const archivedCount = sessions.filter((s) => archivedHidden(s, cleared)).length;
  const showArchived = archivedOn && archivedCount > 0;
  const shown = showArchived ? sessions : sessions.filter((s) => !archivedHidden(s, cleared));
  const visible = filterFleet(shown, stations, filter).filter((s) =>
    statusMatches(s, phone ? 'all' : status),
  );
  const repos = repoNames(shown);
  const contested = contestedBy(list ?? []);
  const conflictCount = list?.length ?? 0;
  const stationIds = [
    ...new Set([...stations.map((s) => s.station_id), ...sessions.map((s) => s.station_id)]),
  ].sort();
  // An open request counts also when its station became unreachable (the rail still shows it).
  const needsYou = sessions.filter(
    (s) => s.status === 'needs_you' || s.summary.open_requests.length > 0,
  ).length;
  const stationCount = stations.length;
  const unknownProvider = sessions.some(
    (s) => s.summary.provider === null || s.summary.provider === 'unknown',
  );
  const filtered =
    (!phone && status !== 'all') ||
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
      <div className="fd-home-titles">
        <h1 className="fd-page-title">Fleet</h1>
        <p className="fd-home-counts">
          <span>{sessions.length === 1 ? '1 session' : `${sessions.length} sessions`}</span>
          <span className="fd-home-on">
            {' '}
            on <span>{stationCount === 1 ? '1 station' : `${stationCount} stations`}</span>
          </span>
          .{' '}
          {needsYou === 0 ? (
            <span className="fd-home-needs" data-clear="true">
              All clear
            </span>
          ) : (
            <span className="fd-home-needs">
              {needsYou === 1 ? '1 needs you' : `${needsYou} need you`}
            </span>
          )}
          .
          {conflictCount > 0 ? (
            <>
              {' '}
              <span className="fd-home-conf">
                {conflictCount === 1 ? '1 conflict' : `${conflictCount} conflicts`}
              </span>
              .
            </>
          ) : null}
          {limit === null ? null : (
            <>
              {' '}
              <span className="fd-badge" data-tone={limit.tone}>
                {limit.text}
              </span>
            </>
          )}
        </p>
      </div>
      {phone ? null : (
        <div className="fd-seg fd-home-status" role="group" aria-label="Status">
          {STATUS_FILTERS.map((f) => (
            <button
              key={f.id}
              type="button"
              aria-pressed={status === f.id}
              onClick={() => setStatus(f.id)}
            >
              {f.label}
            </button>
          ))}
        </div>
      )}
      <div className="fd-field fd-home-search">
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
          className="fd-sm fd-home-group"
          value={group}
          onChange={(event) => pickGroup(event.target.value as Group)}
        >
          <option value="station">Group: Station</option>
          <option value="repo">Group: Repo</option>
          {hasProjects ? <option value="project">Group: Project</option> : null}
        </select>
      )}
      {canShare(people) ? (
        <button
          type="button"
          className={phone ? 'fd-iconbtn fd-home-share' : 'fd-ghost fd-sm fd-home-share'}
          title="Share board…"
          onClick={() =>
            openShare({ kind: 'board', target: '', label: 'Every session of the deck' })
          }
        >
          <Icon of={Users} size={14} /> <span className="fd-home-share-text">Share board…</span>
        </button>
      ) : null}
      {phone ? null : <span className="fd-home-bell" ref={setBellSlot} />}
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
      bellSlot={variant === 'side' ? bellSlot : undefined}
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
  // The waiting mail rows of the wide Mail & tasks section (from the card extras).
  const waitingMail = sessions.flatMap((s) => {
    const count = cardExtras?.get(sessionRef(s.station_id, s.summary.session_id))?.mail_pending;
    return count === undefined || count === 0
      ? []
      : [
          {
            key: sessionRef(s.station_id, s.summary.session_id),
            name: s.summary.callsign ?? s.summary.title ?? s.summary.session_id,
            count,
          },
        ];
  });
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
      onState={setOpenTab}
      wide={wide}
      waiting={waitingMail}
    />
  );
  // The ops panel (DESIGN-SYSTEM §4.4): usage, the conflict radar, then (wide) Mail & tasks and
  // the Activity and Assign buttons, or the tabs. The usage glance steps aside while the Usage tab
  // is open.
  const ops = (
    <div className="fd-ops" data-tab={openTab ?? undefined}>
      {phone || openTab === 'usage' ? null : <UsageGlance sessions={visible} stations={stations} />}
      {radar}
      {panel}
    </div>
  );

  const waiting = sessions.some((s) => s.summary.open_requests.length > 0);
  if (phone) {
    return (
      <div className="fd-home" data-layout="phone">
        {header}
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
        {ops}
      </div>
    );
  }
  return (
    <div className="fd-home" data-layout={layout} data-rail={waiting ? 'true' : 'false'}>
      <div className="fd-home-main">
        {header}
        {activeRow}
        <div className="fd-lanes">
          {rail('side')}
          <FleetBoard
            sessions={visible}
            stations={stations}
            freshness={freshness}
            group={group}
            contested={contested}
            sessionActions={sessionActions}
            extras={cardExtras}
            foldEnded
          />
          {empty}
        </div>
      </div>
      {ops}
    </div>
  );
}
