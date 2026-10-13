import { useEffect, useState, useSyncExternalStore } from 'react';
import type { AssignActions } from './assign/assign';
import { AutomationsPage } from './automations/AutomationsPage';
import type { AutomationActions } from './automations/automations';
import type { FeedItem } from './board/feed';
import type { ConflictLoader } from './board/conflicts';
import { stationName } from './board/fleet';
import { markOnStart, type PlanActions } from './board/plans';
import type { StationActions } from './board/pairing';
import { PlanLibrary } from './board/PlanLibrary';
import type { RaceSummary } from './compare/compare';
import { ComparePage } from './compare/ComparePage';
import { StationsPanel } from './board/StationsPanel';
import type { FleetActions } from './fleet/fleet';
import type {
  DeckPlan,
  DeckSession,
  FleetExtras,
  SessionKey,
  StationView,
  WorkspaceListing,
} from './gen';
import type { Chrome, Notice } from './app/chrome';
import { startFailures } from './app/failures';
import { FilesPage } from './app/FilesPage';
import { FleetHome } from './app/FleetHome';
import { HelpOverlay } from './app/HelpOverlay';
import { useGlobalKeys } from './app/keys';
import { Layout } from './app/Layout';
import { More } from './app/More';
import { Palette } from './app/Palette';
import { formatHash, type Navigate, parseHash, type Route, useRoute } from './app/routes';
import { SessionRoute } from './app/SessionRoute';
import { basename, sidebarModel, sidebarOrder } from './app/sessions';
import { Sidebar } from './app/Sidebar';
import { type Layout as LayoutName, useLayout, useTouch } from './app/viewport';
import type { Freshness, Timeline } from './runtime/state';
import type { Outcome, SessionActions } from './session/commands';
import type { FileActions } from './session/workspace';
import type { SpawnActions } from './spawn/spawn';
import { SpawnForm } from './spawn/SpawnForm';
import type { Watch } from './terminal/handover';
import { TerminalDock } from './terminal/TerminalDock';
import { type TermTab, type TerminalStore, tabId } from './terminal/store';
import { archived } from './ui/archive';
import { priority } from './ui/localSet';
import { useNow } from './ui/useNow';

type Props = {
  readonly sessions?: readonly DeckSession[];
  readonly stations?: readonly StationView[];
  readonly freshness?: Freshness;
  readonly actions?: StationActions | undefined;
  readonly timelines?: Readonly<Record<string, Timeline>>;
  /** Opens the live timeline of a session; returns the cancel function. */
  readonly watch?: ((key: SessionKey) => () => void) | undefined;
  readonly sessionActions?: ((key: SessionKey) => SessionActions) | undefined;
  /** DL-141: the file service of any folder of a station (the Files page). */
  readonly filesFor?: ((station: string, workspace: string) => FileActions) | undefined;
  readonly spawn?: SpawnActions | undefined;
  /** `assign auto` (M3 6-3). */
  readonly assign?: AssignActions | undefined;
  /** Fleet mail and tasks (M3 6-2 / 6-4). */
  readonly fleet?: FleetActions | undefined;
  /** The route at the first render when there is no window (tests). */
  readonly initialRoute?: Route;
  /** The layout; overrides the window width (tests). */
  readonly initialLayout?: LayoutName;
  /** The spawn form's directory listing at the first render (tests). */
  readonly initialListing?: WorkspaceListing | null;
  /** The event feed since the page opened (DL-095). */
  readonly feed?: readonly FeedItem[];
  /** The plan library (DL-301). */
  readonly plans?: PlanActions | undefined;
  /** The automations (R5, DL-168); absent without the owner login. */
  readonly automations?: AutomationActions | undefined;
  /** The conflict radar (M3 6-7); absent in tests without a deck. */
  readonly conflicts?: ConflictLoader | undefined;
  /** The fleet card extras (DL-141); absent in tests without a deck. */
  readonly extras?: (() => Promise<FleetExtras>) | undefined;
  /** The plan library at the first render (tests). */
  readonly initialPlans?: readonly DeckPlan[] | null;
  /** The connection state, the log out button and the notices of the runtime. */
  readonly chrome?: Chrome | undefined;
  /** The terminals of the board (DL-140). */
  readonly terminals?: TerminalStore | undefined;
  /** The newest summary of one session and a way to hear about changes. */
  readonly summaryWatch?: ((key: SessionKey) => Watch) | undefined;
  /** "Return to board control" of a TUI tab. */
  readonly onReturnTerminal?: ((tab: TermTab) => Promise<Outcome>) | undefined;
  /** The race summary of the compare page (R10.5); absent: the page has no summary slot. */
  readonly race?: ((keys: readonly SessionKey[]) => Promise<RaceSummary>) | undefined;
};

const message = (e: unknown) => (e instanceof Error ? e.message : 'Request failed');

const noSubscribe = () => () => {};

/** How many terminal tabs the dock has. */
function useTabCount(store: TerminalStore | undefined): number {
  return useSyncExternalStore(
    store?.subscribe ?? noSubscribe,
    () => store?.getState().tabs.length ?? 0,
    () => 0,
  );
}

const sessionRoute = (key: SessionKey): Route => ({
  name: 'session',
  station: key.station_id,
  session: key.session_id,
  tab: 'chat',
});

/** Screens read the same runtime snapshot; filters never open another connection. */
export function App({
  sessions = [],
  stations = [],
  freshness = 'stale',
  actions,
  timelines = {},
  watch,
  sessionActions,
  filesFor,
  spawn,
  assign,
  fleet,
  initialRoute,
  initialLayout,
  initialListing = null,
  feed = [],
  plans,
  automations,
  conflicts,
  extras,
  initialPlans = null,
  chrome,
  terminals,
  summaryWatch,
  onReturnTerminal,
  race,
}: Props) {
  const [urlRoute, navigate] = useRoute(initialRoute);
  const measured = useLayout();
  const layout = initialLayout ?? measured;
  const touch = useTouch();
  const phone = layout === 'phone';
  // 'more' and 'terminals' are phone screens: elsewhere they show the home (see the effect).
  const route: Route =
    !phone && (urlRoute.name === 'more' || urlRoute.name === 'terminals')
      ? { name: 'home' }
      : urlRoute;
  const routeKey = formatHash(route);
  const tabCount = useTabCount(terminals);

  useEffect(() => {
    // The dock body is hidden while the store is closed: the terminals screen opens it.
    if (urlRoute.name === 'terminals') terminals?.setOpen(true);
    if (!phone && (urlRoute.name === 'more' || urlRoute.name === 'terminals')) {
      navigate({ name: 'home' }, { replace: true });
    }
  }, [phone, urlRoute.name, terminals, navigate]);

  // The open session; a new route object with the same ids must not reopen the timeline.
  const openStation = route.name === 'session' ? route.station : null;
  const openSession = route.name === 'session' ? route.session : null;
  useEffect(() => {
    if (openStation === null || openSession === null || watch === undefined) return undefined;
    return watch({ station_id: openStation, session_id: openSession });
  }, [openStation, openSession, watch]);
  const selected =
    route.name === 'session'
      ? sessions.find(
          (s) => s.station_id === route.station && s.summary.session_id === route.session,
        )
      : undefined;

  // The first prompt of the spawn form when a plan starts a session.
  const [planPrompt, setPlanPrompt] = useState('');
  // The plan that the spawn form starts; the plan is marked executed after a start that worked (R7.2).
  const [planId, setPlanId] = useState<string | null>(null);
  const [spawnStation, setSpawnStation] = useState<string | null>(null);
  useEffect(() => {
    if (route.name !== 'new') {
      setPlanPrompt('');
      setPlanId(null);
      setSpawnStation(null);
    }
  }, [route.name]);
  // The `c` key asks the fleet home to open its panel on Mail & tasks.
  const [panelRequest, setPanelRequest] = useState<
    { readonly tab: 'mail'; readonly seq: number } | undefined
  >(undefined);
  const starred = priority.use();
  const cleared = archived.use();

  // Overlays. The drawer belongs to the route it was opened on: navigation closes it.
  const [drawerAt, setDrawerAt] = useState<string | null>(null);
  const drawerOpen = layout === 'tablet' && drawerAt === routeKey;
  const [help, setHelp] = useState(false);
  const [palette, setPalette] = useState(false);
  // The first time the owner sees a live board, the help opens once.
  const live = chrome?.freshness === 'live';
  useEffect(() => {
    if (!live) return;
    try {
      if (localStorage.getItem('fd-help-seen') === '1') return;
      localStorage.setItem('fd-help-seen', '1');
    } catch {
      return;
    }
    setHelp(true);
  }, [live]);

  // A notice of this screen (a shell that did not open, the outcome of "Return …").
  const [local, setLocal] = useState<{ notice: Notice; at: string } | null>(null);
  const notify = (notice: Notice, at: Route) => setLocal({ notice, at: formatHash(at) });
  const notices = [
    ...(chrome?.notices ?? []),
    ...(local !== null && local.at === routeKey ? [local.notice] : []),
  ];

  // R7.1: a session that ended before it started gets a banner until the owner dismisses it.
  const [dismissed, setDismissed] = useState<ReadonlySet<string>>(() => new Set());
  const failureNow = useNow(undefined, 30_000);
  const failures = startFailures(sessions, failureNow, dismissed);

  const open = (key: SessionKey) => navigate(sessionRoute(key));
  const toTerminals = () => navigate({ name: 'terminals' });
  const order = sidebarOrder(sidebarModel(sessions, stations, '', { starred, archived: cleared }));
  const step = (by: 1 | -1) => {
    if (order.length === 0) return;
    // The URL, not the last render: a fast second key press comes before React renders the
    // route of the first one, and would step from the old session.
    const now: Route =
      typeof window === 'undefined' || initialRoute !== undefined
        ? route
        : parseHash(window.location.hash);
    const at =
      now.name === 'session'
        ? order.findIndex(
            (c) => c.station_id === now.station && c.summary.session_id === now.session,
          )
        : -1;
    const next =
      at < 0 ? (by === 1 ? 0 : order.length - 1) : (at + by + order.length) % order.length;
    const card = order[next];
    if (card !== undefined) {
      open({ station_id: card.station_id, session_id: card.summary.session_id });
    }
  };
  const toggleDock = () => {
    if (phone) toTerminals();
    else if (terminals !== undefined) terminals.setOpen(!terminals.getState().open);
  };
  // The board hotkeys are off while any overlay is open (`useOverlay`, see `keys.ts`).
  useGlobalKeys({
    next: () => step(1),
    prev: () => step(-1),
    focus: () => {
      const composer = document.getElementById('fd-composer-text');
      if (composer !== null) {
        composer.focus();
        return;
      }
      // The search box lives in the list: show the list first (phone home, tablet drawer).
      if (phone && route.name !== 'home') navigate({ name: 'home' });
      if (layout === 'tablet') setDrawerAt(routeKey);
      setTimeout(() => document.getElementById('fd-sidebar-search')?.focus(), 0);
    },
    new: () => navigate({ name: 'new' }),
    fleet: () => navigate({ name: 'fleet' }),
    compose: () => {
      navigate({ name: 'fleet' });
      setPanelRequest((r) => ({ tab: 'mail', seq: (r?.seq ?? 0) + 1 }));
    },
    terminals: toggleDock,
    help: () => setHelp(true),
    palette: () => {
      setHelp(false);
      setPalette(!palette);
    },
    // The topmost overlay closes first (UI-DESIGN "Esc and overlay order").
    escape: () => {
      if (palette) setPalette(false);
      else if (help) setHelp(false);
      else if (drawerOpen) setDrawerAt(null);
    },
  });

  const shells = actions?.shells;
  const returnTerminal = (tab: TermTab) => {
    const target: Route | null = tab.session === null ? null : sessionRoute(tab.session);
    if (onReturnTerminal === undefined) {
      notify(
        { text: 'Open the session to return it to board control.', tone: 'info' },
        target ?? route,
      );
      if (target !== null) navigate(target);
      return;
    }
    onReturnTerminal(tab).then(
      (outcome) => notify({ text: outcome.text, tone: outcome.ok ? 'info' : 'error' }, route),
      (e: unknown) => notify({ text: message(e), tone: 'error' }, route),
    );
  };
  // R1.4: "Keep this one" moves the other worktrees to the trash; the trash is owner only.
  const worktreeAction = actions?.worktreeAction;
  const compareTrash =
    worktreeAction === undefined
      ? undefined
      : (key: SessionKey) => worktreeAction(key.station_id, 'trash', key.session_id);
  const cwd = selected?.summary.cwd ?? null;
  // DL-141: a station that reports `allow_shell = false` refuses every shell; offer none.
  const shellsOff =
    selected !== undefined &&
    stations.find((st) => st.station_id === selected.station_id)?.info?.allow_shell === false;
  const newShell =
    terminals !== undefined &&
    shells !== undefined &&
    selected !== undefined &&
    cwd !== null &&
    freshness === 'live' &&
    !shellsOff
      ? () => {
          shells.open(selected.station_id, cwd, 120, 32).then(
            (shellId) => {
              terminals.add({
                id: tabId(selected.station_id, shellId),
                station: selected.station_id,
                shellId,
                kind: 'shell',
                title: basename(cwd),
                session: null,
              });
              if (phone) toTerminals();
            },
            (e: unknown) => notify({ text: `Not opened: ${message(e)}`, tone: 'error' }, route),
          );
        }
      : undefined;
  const dock =
    terminals !== undefined && shells !== undefined ? (
      <TerminalDock
        store={terminals}
        actions={shells}
        stationName={(id) => stationName(id, stations)}
        layout={phone ? 'full' : 'dock'}
        touch={touch}
        onReturn={returnTerminal}
        {...(newShell === undefined ? {} : { onNewShell: newShell })}
      />
    ) : null;

  const sidebar = (variant: 'full' | 'list') => (
    <Sidebar
      sessions={sessions}
      stations={stations}
      route={route}
      variant={variant}
      chrome={chrome}
      terminalCount={tabCount}
      onNavigate={(next) => {
        setPlanPrompt('');
        setPlanId(null);
        setSpawnStation(null);
        navigate(next);
      }}
      onOpenTerminals={() => {
        if (phone) toTerminals();
        else {
          terminals?.setOpen(true);
          setDrawerAt(null);
        }
      }}
    />
  );
  const fleetHome = (
    <FleetHome
      sessions={sessions}
      stations={stations}
      freshness={freshness}
      assign={assign}
      fleet={fleet}
      conflicts={conflicts}
      extras={extras}
      feed={feed}
      layout={layout}
      sessionActions={sessionActions}
      onOpen={open}
      onSpawn={() => navigate({ name: 'new' })}
      panelRequest={panelRequest}
      onHelp={() => setHelp(true)}
      onSpawnWith={(station, prompt) => {
        setPlanPrompt(prompt);
        setPlanId(null);
        setSpawnStation(station);
        navigate({ name: 'new' });
      }}
    />
  );

  // R7.2: a plan is `executed` only after its start returned ok; a failed start keeps it `proposed`.
  const planSpawn: SpawnActions | undefined =
    spawn === undefined || plans === undefined || planId === null
      ? spawn
      : markOnStart(spawn, plans, planId, (e) =>
          notify(
            { text: `The plan was not marked executed: ${message(e)}`, tone: 'error' },
            { name: 'plans' },
          ),
        );

  const back: Navigate = navigate;
  const content = (() => {
    switch (route.name) {
      case 'home':
        return phone ? sidebar('list') : fleetHome;
      case 'fleet':
        return fleetHome;
      case 'new':
        return (
          <div className="fd-screen">
            <SpawnForm
              key={`${planPrompt}|${spawnStation ?? ''}`}
              initialPrompt={planPrompt}
              initialStation={spawnStation ?? undefined}
              sessions={sessions}
              stations={stations}
              freshness={freshness}
              actions={planSpawn}
              initialListing={initialListing}
              onOpen={open}
            />
          </div>
        );
      case 'plans':
        return (
          <div className="fd-screen">
            <PlanLibrary
              stations={stations}
              sessions={sessions}
              actions={plans}
              live={freshness === 'live'}
              initial={initialPlans}
              onOpen={open}
              onAssign={
                fleet === undefined ? undefined : (target, text) => fleet.mail(target, text, false)
              }
              onExecute={(plan) => {
                setPlanPrompt(plan.markdown);
                setPlanId(plan.id);
                setSpawnStation(null);
                navigate({ name: 'new' });
              }}
            />
          </div>
        );
      case 'automations':
        return (
          <div className="fd-screen">
            <AutomationsPage
              stations={stations}
              actions={automations}
              live={freshness === 'live'}
            />
          </div>
        );
      case 'stations':
        return (
          <div className="fd-screen">
            <StationsPanel
              stations={stations}
              sessions={sessions}
              freshness={freshness}
              actions={actions}
            />
          </div>
        );
      case 'files':
        return <FilesPage stations={stations} sessions={sessions} filesFor={filesFor} />;
      case 'compare':
        return (
          <ComparePage
            batchId={route.batch}
            sessions={sessions}
            stations={stations}
            timelines={timelines}
            freshness={freshness}
            watch={watch}
            sessionActions={sessionActions}
            trash={compareTrash}
            race={race}
            onOpen={open}
          />
        );
      case 'more':
        return <More stationCount={stations.length} terminalCount={tabCount} chrome={chrome} />;
      case 'terminals':
        return tabCount > 0 && dock !== null ? (
          dock
        ) : (
          <p className="fd-empty fd-screen">No terminals open.</p>
        );
      case 'session': {
        if (selected === undefined) {
          return (
            <div className="fd-screen">
              <p>This session is not on the board.</p>
              <a href="#/">Back to the fleet</a>
            </div>
          );
        }
        return (
          <SessionRoute
            session={selected}
            stations={stations}
            timeline={timelines[JSON.stringify([route.station, route.session])]}
            freshness={freshness}
            actions={sessionActions?.({ station_id: route.station, session_id: route.session })}
            shells={shells}
            terminals={terminals}
            summaryWatch={summaryWatch}
            layout={layout}
            route={route}
            navigate={back}
            onTerminalOpened={phone ? toTerminals : undefined}
            onNewShell={newShell}
          />
        );
      }
    }
  })();

  return (
    <Layout
      layout={layout}
      route={route}
      sidebar={sidebar('full')}
      drawerOpen={drawerOpen}
      onDrawer={(next) => setDrawerAt(next ? routeKey : null)}
      notices={notices}
      failures={failures}
      onOpenFailure={(f) => open(f.key)}
      onDismissFailure={(f) => setDismissed((set) => new Set(set).add(f.ref))}
      dock={dock}
      onTerminalsBack={() => {
        if (typeof window !== 'undefined' && window.history.length > 1) window.history.back();
        else navigate({ name: 'home' });
      }}
      overlays={
        <>
          {help ? <HelpOverlay onClose={() => setHelp(false)} /> : null}
          {palette ? (
            <Palette
              sessions={sessions}
              stations={stations}
              onOpen={open}
              onClose={() => setPalette(false)}
              onStart={(prompt) => {
                setPlanPrompt(prompt);
                setPlanId(null);
                setSpawnStation(null);
                navigate({ name: 'new' });
              }}
            />
          ) : null}
        </>
      }
    >
      {content}
    </Layout>
  );
}
