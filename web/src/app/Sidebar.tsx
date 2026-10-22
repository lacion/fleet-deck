import { useEffect, useState } from 'react';
import { STATUS_LANES } from '../board/fleet';
import type { DeckSession, StationView } from '../gen';
import {
  Activity,
  Archive,
  ChevronDown,
  Eye,
  Folder,
  GitBranch,
  Paperclip,
  Icon,
  LayoutGrid,
  LogOut,
  FileText,
  Plus,
  Search,
  Server,
  Settings,
  Smartphone,
  SquareTerminal,
  Star,
  Users,
  Workflow,
} from '../ui/icons';
import { useIntegrations, useProjects } from '../integrations/context';
import { useSettings } from '../settings/context';
import { FolderGit2 } from '../integrations/icons';
import { shortModel } from '../ui/format';
import { Mark } from '../ui/Mark';
import { Menu } from '../ui/Menu';
import { GeneratedMark } from '../ui/generated';
import { archived as archivedSet } from '../ui/archive';
import { priority, sessionRef } from '../ui/localSet';
import { canShare, usePeople } from '../sharing/context';
import { isHibernated } from '../views/hibernate';
import { FocusLine, UnreadMark, unreadOf } from '../views/marks';
import type { Chrome } from './chrome';
import { DeviceLinkDialog } from './DeviceLinkDialog';
import { Avatar, LivePill } from './Layout';
import { PrefsRow } from './Prefs';
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
  isHibernated(card)
    ? 'Hibernated'
    : (STATUS_LANES.find((lane) => lane.status === card.status)?.label ?? card.status);

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

function Row({
  card,
  route,
  now,
  starred,
}: {
  card: DeckSession;
  route: Route;
  now: number;
  starred: boolean;
}) {
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
        <span className="fd-holder" data-status={card.status} aria-hidden="true" />
        <span className="fd-dot" data-status={card.status} aria-hidden="true" />
        <span className="fd-srow-title">
          <span className="fd-vh">{statusLabel(card)}: </span>
          {sessionTitle(card)}
          <UnreadMark count={unreadOf(card)} />
          {s.title !== null && s.title_generated ? <GeneratedMark /> : null}
          {starred ? (
            <span className="fd-srow-star" role="img" aria-label="Starred">
              <Icon of={Star} size={12} />
            </span>
          ) : null}
          {s.terminal != null ? <span className="fd-badge">In terminal</span> : null}
        </span>
        <span className="fd-srow-age">{relativeAge(s.updated_at, now)}</span>
        <FocusLine summary={s} />
        <span className="fd-srow-meta">
          <span className="fd-srow-model">{shortModel(s.model)}</span>
          {s.branch != null && s.branch !== '' ? (
            <span className="fd-srow-place">
              <Icon of={GitBranch} size={12} />
              {s.branch}
            </span>
          ) : s.cwd != null ? (
            <span className="fd-srow-place">
              <Icon of={Folder} size={12} />
              {basename(s.cwd)}
            </span>
          ) : null}
        </span>
      </a>
    </li>
  );
}

function Rows({
  cards,
  route,
  now,
  starred,
}: {
  cards: readonly DeckSession[];
  route: Route;
  now: number;
  starred: ReadonlySet<string>;
}) {
  return (
    <ul className="fd-srows">
      {cards.map((card) => {
        const ref = sessionRef(card.station_id, card.summary.session_id);
        return <Row key={ref} card={card} route={route} now={now} starred={starred.has(ref)} />;
      })}
    </ul>
  );
}

/** DL-176: how the sidebar groups the sessions. A browser setting, like the home grouping. */
export type SidebarGroup = 'station' | 'project';
const GROUP_KEY = 'fd-sidebar-group';

function readGroup(): SidebarGroup {
  try {
    return localStorage.getItem(GROUP_KEY) === 'project' ? 'project' : 'station';
  } catch {
    return 'station';
  }
}

function writeGroup(group: SidebarGroup) {
  try {
    localStorage.setItem(GROUP_KEY, group);
  } catch {
    // Storage is blocked: the grouping still works for this visit.
  }
}

const MORE_KEY = 'fd-sidebar-more';

/** The last state of the "More" fold of the navigation. A browser setting. */
export function readMore(): boolean {
  try {
    return localStorage.getItem(MORE_KEY) === '1';
  } catch {
    return false;
  }
}

export function writeMore(open: boolean) {
  try {
    localStorage.setItem(MORE_KEY, open ? '1' : '0');
  } catch {
    // Storage is blocked: the fold still works for this visit.
  }
}

/** The routes that the pages inside the "More" fold own. */
const MORE_ROUTES: ReadonlySet<Route['name']> = new Set([
  'plans',
  'automations',
  'artifacts',
  'artifact',
  'projects',
  'project',
  'files',
  'shares',
  'spaces',
]);

function Groups({
  model,
  by,
  route,
  now,
  withNeeds,
  starred,
}: {
  model: SidebarModel;
  by: SidebarGroup;
  route: Route;
  now: number;
  withNeeds: boolean;
  starred: ReadonlySet<string>;
}) {
  const [showArchived, setShowArchived] = useState(false);
  // A session that needs you lists once: in "Needs you". The count of a group is its shown rows.
  const needsApart = withNeeds && model.needs.length > 0;
  const placeWord = by === 'project' ? 'in' : 'on';
  return (
    <>
      {needsApart ? (
        <section className="fd-sgroup" data-group="needs" aria-label="Needs you">
          <h3 className="fd-sgroup-title">
            <span className="fd-sgroup-name">Needs you</span>
            <span className="fd-count" data-tone="act">
              {model.needs.length}
            </span>
          </h3>
          <Rows cards={model.needs} route={route} now={now} starred={starred} />
        </section>
      ) : null}
      {model.groups.map((group) => {
        const live = needsApart ? group.live.filter((c) => c.status !== 'needs_you') : group.live;
        // The station total (with its needs-you and ended sessions) goes in the title only.
        const total =
          group.count === live.length
            ? undefined
            : `${group.count} ${group.count === 1 ? 'session' : 'sessions'} ${placeWord} ${group.name}`;
        return (
          <section
            key={group.id}
            className="fd-sgroup"
            data-station={by === 'station' ? group.id : undefined}
            data-project={by === 'project' ? group.id : undefined}
            aria-label={`${by === 'project' ? 'Project' : 'Station'} ${group.name}`}
          >
            <h3 className="fd-sgroup-title">
              <span className="fd-sgroup-name fd-wide">{group.name}</span>
              {/* The count right after the name, as in the Needs you group and the fleet lanes. */}
              <span className="fd-count" title={total}>
                {live.length}
              </span>
              {by === 'project' ? null : (
                <span
                  className="fd-rdot"
                  data-reachable={group.reachable ? 'true' : 'false'}
                  aria-hidden="true"
                />
              )}
              {by === 'project' ? null : (
                <span className="fd-vh">{group.reachable ? 'reachable' : 'unreachable'}</span>
              )}
            </h3>
            <Rows cards={live} route={route} now={now} starred={starred} />
            {group.ended.length > 0 ? (
              <details className="fd-sended">
                <summary className="fd-label">
                  Ended <span className="fd-count">{group.ended.length}</span>
                </summary>
                <Rows cards={group.ended} route={route} now={now} starred={starred} />
                <button
                  type="button"
                  className="fd-ghost fd-sm fd-sended-clear"
                  aria-label={`Archive the ended sessions of ${group.name}`}
                  onClick={() =>
                    archivedSet.add(
                      group.ended
                        .filter((c) => c.status === 'ended' || c.status === 'lost')
                        .map((c) => sessionRef(c.station_id, c.summary.session_id)),
                    )
                  }
                >
                  <Icon of={Archive} size={14} /> Clear
                </button>
              </details>
            ) : null}
          </section>
        );
      })}
      {model.archived.length > 0 ? (
        <section className="fd-sgroup" data-group="archived" aria-label="Archived">
          <button
            type="button"
            className="fd-ghost fd-sm fd-sidebar-archived"
            aria-expanded={showArchived}
            onClick={() => setShowArchived(!showArchived)}
          >
            <Icon of={Archive} size={14} /> Archived ({model.archived.length})
          </button>
          {showArchived ? (
            <Rows cards={model.archived} route={route} now={now} starred={starred} />
          ) : null}
        </section>
      ) : null}
    </>
  );
}

type LinksProps = {
  readonly route: Route;
  /** The sessions that need you: the count of the Fleet row. */
  readonly needs: number;
  readonly stations: readonly StationView[];
  readonly terminalCount: number;
  readonly guest: boolean;
  readonly projectsLink: boolean;
  readonly onOpenTerminals: () => void;
};

/** The page links of the board: the main ones, then the "More" fold. */
function BoardLinks({
  route,
  needs,
  stations,
  terminalCount,
  guest,
  projectsLink,
  onOpenTerminals,
}: LinksProps) {
  // R11.4: a guest sees the fleet and the watch page only; the owner sees Shares.
  const people = usePeople();
  const shares = canShare(people);
  const inMore = MORE_ROUTES.has(route.name);
  const [more, setMore] = useState(() => inMore || readMore());
  // A page inside the fold opens it. The owner can still close it there.
  useEffect(() => {
    if (inMore) setMore(true);
  }, [inMore]);
  return (
    <nav className="fd-sidebar-links" aria-label="Board pages">
      {/* The home and the Fleet page are the same screen on a desktop. */}
      {/* The name says what the number means: "Fleet, 3 need you". (A text inside the pill would
          read "Fleet , 3": the pill is a flex item, and the name gets a space before it.) */}
      <a
        href="#/fleet"
        aria-label={
          needs > 0 ? `Fleet, ${needs} ${needs === 1 ? 'needs you' : 'need you'}` : undefined
        }
        aria-current={route.name === 'fleet' || route.name === 'home' ? 'page' : undefined}
      >
        <Icon of={LayoutGrid} />
        Fleet
        {needs > 0 ? (
          <span className="fd-count" data-tone="act" aria-hidden="true">
            {needs}
          </span>
        ) : null}
      </a>
      <a href="#/watch" aria-current={route.name === 'watch' ? 'page' : undefined}>
        <Icon of={Eye} />
        Watch
      </a>
      {guest ? null : (
        <>
          <a href="#/activity" aria-current={route.name === 'activity' ? 'page' : undefined}>
            <Icon of={Activity} />
            Activity
          </a>
          <a href="#/stations" aria-current={route.name === 'stations' ? 'page' : undefined}>
            <Icon of={Server} />
            Stations
            <span className="fd-count">{stations.length}</span>
          </a>
        </>
      )}
      {guest && !shares ? null : (
        <details
          className="fd-sidebar-more"
          open={more}
          onToggle={(event) => {
            const next = event.currentTarget.open;
            // The attribute follows the state; only a press of the owner differs from it.
            if (next === more) return;
            setMore(next);
            writeMore(next);
          }}
        >
          <summary>
            <span>More</span>
            <Icon of={ChevronDown} size={14} />
          </summary>
          {guest ? null : (
            <>
              <a href="#/plans" aria-current={route.name === 'plans' ? 'page' : undefined}>
                <Icon of={FileText} />
                Plans
              </a>
              <a
                href="#/automations"
                aria-current={route.name === 'automations' ? 'page' : undefined}
              >
                <Icon of={Workflow} />
                Automations
              </a>
              <a
                href="#/artifacts"
                aria-current={
                  route.name === 'artifacts' || route.name === 'artifact' ? 'page' : undefined
                }
              >
                <Icon of={Paperclip} />
                Artifacts
              </a>
              {projectsLink ? (
                <a
                  href="#/projects"
                  aria-current={
                    route.name === 'projects' || route.name === 'project' ? 'page' : undefined
                  }
                >
                  <Icon of={FolderGit2} />
                  Projects
                </a>
              ) : null}
              <a href="#/files" aria-current={route.name === 'files' ? 'page' : undefined}>
                <Icon of={Folder} />
                Files
              </a>
            </>
          )}
          {shares ? (
            <a
              href="#/shares"
              aria-current={route.name === 'shares' || route.name === 'spaces' ? 'page' : undefined}
            >
              <Icon of={Users} />
              Shares
            </a>
          ) : null}
          {terminalCount > 0 && !guest ? (
            <button type="button" className="fd-linkbtn" onClick={onOpenTerminals}>
              <Icon of={SquareTerminal} />
              Terminals
              <span className="fd-count">{terminalCount}</span>
            </button>
          ) : null}
        </details>
      )}
    </nav>
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
  const [addingDevice, setAddingDevice] = useState(false);
  const now = useNow(givenNow, 30000);
  const starred = priority.use();
  const archived = archivedSet.use();
  const projects = useProjects();
  const [group, setGroup] = useState<SidebarGroup>(readGroup);
  // The choice "Project" shows only while the board knows a project (the owner's login).
  const by: SidebarGroup = group === 'project' && projects.length > 0 ? 'project' : 'station';
  const model = sidebarModel(
    sessions,
    stations,
    query,
    { starred, archived },
    by === 'project' ? projects : undefined,
  );
  const empty = sessions.length === 0;
  const nothingFound =
    !empty && model.groups.length === 0 && model.needs.length === 0 && model.archived.length === 0;
  const full = variant === 'full';
  // R3: the projects are the owner's (the deck answers 403 to any other login).
  const projectsLink = useIntegrations()?.actions.canAdmin === true;
  // DL-172: the settings are the owner's login only (no token, no member).
  const settingsLink = useSettings() !== undefined;
  const guest = usePeople().member !== null;
  // DL-176: the grouping (Station | Project), only while the board knows a project.
  const groupSeg =
    projects.length > 0 ? (
      <div className="fd-seg fd-sidebar-group" role="group" aria-label="Group sessions by">
        {(['station', 'project'] as const).map((choice) => (
          <button
            key={choice}
            type="button"
            aria-pressed={by === choice}
            onClick={() => {
              setGroup(choice);
              writeGroup(choice);
            }}
          >
            {choice === 'station' ? 'Station' : 'Project'}
          </button>
        ))}
      </div>
    ) : null;
  return (
    <aside className="fd-sidebar" aria-label="Sessions" data-variant={variant}>
      <div className="fd-sidebar-top">
        {/* The phone has its own top bar (Layout): the brand row is the desktop's. */}
        {full ? (
          <div className="fd-sidebar-brand">
            <Mark />
            <span className="fd-wordmark">Fleet Deck</span>
            <LivePill chrome={chrome} />
          </div>
        ) : null}
        {/* One row (§4.2): New session, then the search. On the desktop the search folds to its
            icon and "/" until it has the focus or a text; then New session folds to its icon. */}
        <div className="fd-sidebar-actions" data-query={query === '' ? undefined : 'true'}>
          {full && !guest ? (
            <button
              type="button"
              className="fd-primary fd-newbtn"
              onClick={() => onNavigate({ name: 'new' })}
            >
              <Icon of={Plus} /> <span className="fd-newbtn-text">New session</span>{' '}
              <span className="fd-kbd" aria-hidden="true">
                N
              </span>
            </button>
          ) : null}
          <div className="fd-field fd-sidebar-search">
            <Icon of={Search} />
            <input
              id="fd-sidebar-search"
              type="search"
              aria-label="Search sessions"
              aria-keyshortcuts="/"
              placeholder="Search sessions"
              autoComplete="off"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
            <span className="fd-kbd" aria-hidden="true">
              /
            </span>
          </div>
          {/* The phone: the grouping sits in the search row; the list has no head row. */}
          {full ? null : groupSeg}
          {/* The phone starts a session with New in the bottom bar (§4.8): no second button here. */}
        </div>
      </div>
      {full ? (
        <BoardLinks
          route={route}
          needs={model.needs.length}
          stations={stations}
          terminalCount={terminalCount}
          guest={guest}
          projectsLink={projectsLink}
          onOpenTerminals={onOpenTerminals}
        />
      ) : null}
      <div className="fd-sidebar-list">
        {full ? (
          <div className="fd-sidebar-listhead">
            <span className="fd-label">Sessions</span>
            {groupSeg}
          </div>
        ) : null}
        {empty ? (
          // The phone list names the New of its bottom bar; a guest cannot start a session.
          <p className="fd-empty">
            {full || guest ? 'No sessions yet.' : 'No sessions yet. Start one with New.'}
          </p>
        ) : nothingFound ? (
          <p className="fd-empty">No matching sessions.</p>
        ) : (
          <Groups model={model} by={by} route={route} now={now} withNeeds starred={starred} />
        )}
      </div>
      {full ? (
        <div className="fd-sidebar-foot">
          {/* §4.2 item 5: settings, the theme segments, the compact toggle, then the avatar. */}
          {settingsLink && !guest ? (
            <a
              href="#/settings"
              className="fd-iconbtn fd-sidebar-settings"
              aria-label="Settings"
              aria-current={route.name === 'settings' ? 'page' : undefined}
            >
              <Icon of={Settings} />
            </a>
          ) : null}
          <PrefsRow stations={stations} getPalette={chrome?.getPalette} />
          <div className="fd-sidebar-account">
            {chrome === undefined ? null : (
              <Menu summary={<Avatar />} summaryClass="fd-iconbtn" label="Account">
                {(close) => (
                  <>
                    {chrome.addDevice === undefined ? null : (
                      <button
                        type="button"
                        onClick={() => {
                          close();
                          setAddingDevice(true);
                        }}
                      >
                        <Icon of={Smartphone} size={14} /> Add a device
                      </button>
                    )}
                    <button
                      type="button"
                      onClick={() => {
                        close();
                        chrome.onLogout();
                      }}
                    >
                      <Icon of={LogOut} size={14} /> Log out
                    </button>
                  </>
                )}
              </Menu>
            )}
          </div>
          {addingDevice && chrome?.addDevice !== undefined ? (
            <DeviceLinkDialog load={chrome.addDevice} onClose={() => setAddingDevice(false)} />
          ) : null}
        </div>
      ) : null}
    </aside>
  );
}
