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
  fleetVersion,
  isLiveSession,
  repoNames,
  stationName,
} from '../board/fleet';
import { atWork } from '../board/agentCounts';
import { record } from '../board/activity';
import { FleetBoard, type FleetGroup } from '../board/FleetBoard';
import { HomePanel, type PanelTab, useWideOps } from '../board/HomePanel';
import { NeedsYouRail } from '../board/NeedsYouRail';
import { UsageGlance } from '../board/UsageGlance';
import type { RecountCall } from '../board/usageRecountCall';
import { limitChip, rateMeters } from '../board/usage';
import type { FleetActions } from '../fleet/fleet';
import type { DeckSession, SessionKey, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import { FleetSentence } from '../views/FleetSentence';
import { type StatusFilter, StatusSeg, statusMatches } from '../views/StatusSeg';
import { stationOrder } from './sessions';
import type { SessionActions } from '../session/commands';
import { useProjects } from '../integrations/context';
import { canShare, usePeople } from '../sharing/context';
import { openShare } from '../sharing/shareStore';
import { useDismiss } from '../ui/dismiss';
import { CircleHelp, Ellipsis, Icon, ListFilter, Plus, Search, Share2, X } from '../ui/icons';
import { Menu } from '../ui/Menu';
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
  /** B20: the owner's recount of old usage counts; shown in the Usage tab. */
  readonly recount?: RecountCall | undefined;
};

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

/** The provider words of the filters; a session without a known provider is "Other" (§1 rule 7:
 * no "unknown"). */
const PROVIDER_NAME: Record<string, string> = {
  claude: 'Claude',
  codex: 'Codex',
  unknown: 'Other',
};

/** The order of the provider chips: Claude, Codex, then Other. */
const PROVIDER_ORDER: readonly string[] = ['claude', 'codex', 'unknown'];

/**
 * The providers of the sessions, as the provider filter keys them (`unknown` for none), in the
 * chip order: the Provider row offers only a provider that is on the board (as the Repository
 * row), never a chip that filters to an empty board.
 */
export function providerKeys(sessions: readonly DeckSession[]): string[] {
  const keys = [...new Set(sessions.map((s) => s.summary.provider ?? 'unknown'))];
  const rank = (key: string) => {
    const i = PROVIDER_ORDER.indexOf(key);
    return i < 0 ? PROVIDER_ORDER.length : i;
  };
  return keys.sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
}

// The status filter and the summary sentence are the Watch header's too (views/): one control and
// one sentence for one meaning.
export { clearText } from '../views/FleetSentence';
export type { StatusFilter } from '../views/StatusSeg';
export { statusMatches } from '../views/StatusSeg';

/**
 * The sessions of the home under the filters: `scoped` without the status filter (the usage
 * glance and the Usage panel: a rate limit belongs to an account, not to a status), `visible` with
 * it (the board).
 */
export function scopeFleet(
  shown: readonly DeckSession[],
  stations: readonly StationView[],
  filter: FleetFilter,
  status: StatusFilter,
): { readonly scoped: readonly DeckSession[]; readonly visible: readonly DeckSession[] } {
  const scoped = filterFleet(shown, stations, filter);
  return { scoped, visible: scoped.filter((s) => statusMatches(s, status)) };
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
  if (filter.repo !== ALL) list.push({ key: 'repo', text: `Repository: ${filter.repo}` });
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

/** The widest Filters menu (home.css `.fd-filters-menu .fd-menu-body`). */
const FILTERS_W = 480;

/**
 * Where the Filters menu hangs from its button: from its left edge when the menu fits in the main
 * column there, else from its right edge (the button is near the end of the header).
 */
export function filtersAlign(
  button: { readonly left: number },
  column: { readonly right: number },
  viewport: number,
): 'start' | 'end' {
  const width = Math.min(FILTERS_W, viewport - 32);
  return button.left + width <= column.right ? 'start' : 'end';
}

/** The Filters menu: a `<details>` popover that Escape or a press outside closes. It hangs from
 * its button (never from the edge of the header); its trigger has the trailing chevron of every
 * menu trigger of the board (`summary[data-variant=toggle]`). */
function FiltersMenu({ active, children }: { active: number; children: ReactNode }) {
  const ref = useRef<HTMLDetailsElement>(null);
  const [open, setOpen] = useState(false);
  const [align, setAlign] = useState<'start' | 'end'>('end');
  // The click on the summary comes before the toggle: the menu opens on its side at once.
  const place = () => {
    const details = ref.current;
    if (details === null || details.open) return;
    const button = details.querySelector('summary')?.getBoundingClientRect();
    const column = details.closest('.fd-home-main')?.getBoundingClientRect();
    if (button === undefined || column === undefined) return;
    setAlign(filtersAlign(button, column, window.innerWidth));
  };
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
      data-align={align}
      ref={ref}
      onKeyDown={onKeyDown}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="fd-ghost fd-sm" data-variant="toggle" onClick={place}>
        <Icon of={ListFilter} size={14} />
        Filters
        {active > 0 ? (
          <>
            {' '}
            <span className="fd-count">{active}</span>
          </>
        ) : null}
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
  recount,
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
  const { scoped, visible } = scopeFleet(shown, stations, filter, phone ? 'all' : status);
  const repos = repoNames(shown);
  const providers = providerKeys(shown);
  const contested = contestedBy(list ?? []);
  const conflictCount = list?.length ?? 0;
  // The station chips follow the order of the lanes (the deck's station order), not the ids.
  const stationIds = stationOrder(
    [...stations.map((s) => s.station_id), ...sessions.map((s) => s.station_id)],
    stations,
  );
  // An open request counts also when its station became unreachable (the rail still shows it).
  const needsYou = sessions.filter(
    (s) => s.status === 'needs_you' || s.summary.open_requests.length > 0,
  ).length;
  // The header sentence (§4.3): the live sessions, and the stations that run one (an online
  // station without a live session has a lane, but runs none of them). Ended and archived
  // sessions have their own rows (the Ended folds, Filters).
  const live = sessions.filter(isLiveSession);
  const liveStations = new Set(live.map((s) => s.station_id)).size;
  const filtered =
    (!phone && status !== 'all') ||
    filter.station !== ALL ||
    filter.provider !== ALL ||
    filter.repo !== ALL ||
    filter.search.trim() !== '';

  const chip = (label: ReactNode, selected: boolean, pick: () => void, key: string) => (
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
      {providers.length >= 2 || filter.provider !== ALL
        ? row(
            'Provider',
            <>
              {chip(
                'All',
                filter.provider === ALL,
                () => setFilter({ ...filter, provider: ALL }),
                'p-all',
              )}
              {[
                ...new Set([...providers, ...(filter.provider === ALL ? [] : [filter.provider])]),
              ].map((key) =>
                chip(
                  PROVIDER_NAME[key] ?? key,
                  filter.provider === key,
                  () => setFilter({ ...filter, provider: key }),
                  key,
                ),
              )}
            </>,
          )
        : null}
      {repos.length >= 2 || filter.repo !== ALL
        ? row(
            'Repository',
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
      {/* The row says Archived, so its one chip shows "Show" and the count; a hidden word keeps
          its name whole ("Show archived 30", §5). */}
      {archivedCount > 0
        ? row(
            'Archived',
            chip(
              <>
                Show <span className="fd-vh">archived </span>
                <span className="fd-count">{archivedCount}</span>
              </>,
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
  const shareable = canShare(people);
  const shareBoard = () =>
    openShare({ kind: 'board', target: '', label: 'A link to every session on this deck.' });
  const waiting = sessions.some((s) => s.summary.open_requests.length > 0);
  // The phone has no tooltips: Share board and the notification switch are the rows of one menu
  // (§4.8). With a request, the switch is in the head of the Needs you strip instead.
  const phoneMenu =
    !shareable && waiting ? null : (
      <Menu summary={<Icon of={Ellipsis} />} summaryClass="fd-iconbtn" label="Board options">
        {(close) => (
          <>
            {shareable ? (
              <button
                type="button"
                className="fd-home-share"
                onClick={() => {
                  close();
                  shareBoard();
                }}
              >
                <Icon of={Share2} /> Share board…
              </button>
            ) : null}
            <span className="fd-home-bell" ref={setBellSlot} />
          </>
        )}
      </Menu>
    );
  const header = (
    <header className="fd-home-head">
      <div className="fd-home-titles">
        <h1 className="fd-page-title">Fleet</h1>
        <p className="fd-home-counts">
          <FleetSentence
            live={live.length}
            stations={liveStations}
            needsYou={needsYou}
            conflicts={conflictCount}
            atWork={atWork(live)}
          />
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
      {phone ? null : <StatusSeg value={status} onChange={setStatus} className="fd-home-status" />}
      <div className="fd-field fd-home-search">
        <Icon of={Search} />
        <input
          type="search"
          id="fd-home-search"
          aria-label="Search sessions"
          aria-keyshortcuts="/"
          placeholder="Search sessions"
          value={filter.search}
          onChange={(event) => setFilter({ ...filter, search: event.target.value })}
        />
      </div>
      <FiltersMenu active={active.length}>{filterRows}</FiltersMenu>
      {phone ? null : (
        <select
          aria-label="Group by"
          className="fd-ghost fd-sm fd-home-group"
          value={group}
          onChange={(event) => pickGroup(event.target.value as Group)}
        >
          <option value="station">Group by station</option>
          <option value="repo">Group by repository</option>
          {hasProjects ? <option value="project">Group by project</option> : null}
        </select>
      )}
      {phone ? (
        phoneMenu
      ) : (
        <>
          {shareable ? (
            <button
              type="button"
              className="fd-ghost fd-sm fd-home-share"
              title="Share board…"
              aria-label="Share board…"
              onClick={shareBoard}
            >
              <Icon of={Share2} size={14} />{' '}
              <span className="fd-home-share-text" aria-hidden="true">
                Share board…
              </span>
              <span className="fd-home-share-short" aria-hidden="true">
                Share
              </span>
            </button>
          ) : null}
          {/* With no request the Needs you panel is absent: its notification switch moves here. */}
          <span className="fd-home-bell" ref={setBellSlot} />
        </>
      )}
    </header>
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
      bellSlot={bellSlot}
      bellRow={phone}
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
  const radar =
    conflicts === undefined ? null : (
      <ConflictRadar list={list} problem={problem} stations={stations} onOpen={onOpen} />
    );
  // The top of the ops panel (DESIGN-SYSTEM §4.4): usage and the conflict radar. The usage glance
  // steps aside while the Usage tab is open; the phone shows the radar under the Needs you strip.
  const lead = (
    <>
      {phone || openTab === 'usage' ? null : <UsageGlance sessions={scoped} stations={stations} />}
      {phone ? null : radar}
    </>
  );
  const panel = (
    <HomePanel
      hasAssign={assign !== undefined}
      guest={guest}
      stations={stations}
      sessions={sessions}
      visible={scoped}
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
      lead={wide ? lead : undefined}
      recount={recount}
    />
  );
  // The ops panel: the lead, then (wide) the scrolling sections and the fixed foot of Activity,
  // Assign and All usage (HomePanel), or the tabs.
  const ops = (
    <div className="fd-ops" data-tab={openTab ?? undefined} data-wide={wide ? 'true' : undefined}>
      {wide ? null : lead}
      {panel}
    </div>
  );
  // Without a filter, the station board shows every online station, an empty one too (§4.4).
  const spawnAt =
    guest || onSpawnWith === undefined
      ? undefined
      : (stationId: string) => onSpawnWith(stationId, '');

  if (phone) {
    return (
      <div className="fd-home" data-layout="phone">
        {header}
        {activeRow}
        {rail('strip')}
        {radar}
        {empty ?? (
          <FleetBoard
            sessions={visible}
            stations={stations}
            freshness={freshness}
            group="station"
            contested={contested}
            sessionActions={sessionActions}
            extras={cardExtras}
            foldEnded
            emptyLanes={!filtered}
            onSpawnAt={spawnAt}
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
            emptyLanes={!filtered && group === 'station' && visible.length > 0}
            onSpawnAt={spawnAt}
          />
          {empty}
        </div>
      </div>
      {ops}
    </div>
  );
}
