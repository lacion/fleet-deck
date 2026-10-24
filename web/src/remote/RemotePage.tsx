// B17 §3.3: `#/remote/<display id>`, the remote of one wall display (owner only). The mirror is the
// wall page drawn on this device from the owner's data and the state of the deck; the pad sends
// controls and navigations; the deck is the one order of both. Nothing here reads the wall screen.
import {
  type ComponentProps,
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from 'react';
import type { Navigate } from '../app/routes';
import type { ArtifactActions } from '../artifacts/actions';
import { availability, CaptureBlock } from '../desktop/CapturePanel';
import { CaptureFlow, type FlowState, IDLE, isActive } from '../desktop/flow';
import type {
  DeckSession,
  DesktopInfo,
  DisplayView,
  SessionKey,
  StationView,
  WallArea,
} from '../gen';
import type { Freshness, Timeline } from '../runtime/state';
import type { ViewsActions } from '../runtime/views';
import type { SessionActions } from '../session/commands';
import { useSettings } from '../settings/context';
import type { SettingsEnv } from '../settings/context';
import { describe } from '../settings/http';
import { usePeople } from '../sharing/context';
import { Icon, ImageIcon, RefreshCw } from '../ui/icons';
import { Loading } from '../ui/Loading';
import { ErrorNotice, Page } from '../ui/page';
import { useSize } from '../wall/hooks';
import { WallPage } from '../wall/WallPage';
import {
  GONE_TEXT,
  NO_REMOTE_TEXT,
  OFFLINE_TEXT,
  OLDER_TEXT,
  OWNER_ONLY_TEXT,
  REMOTE_FEATURE,
  RemoteLink,
  STATUS_CLEAR_MS,
  type Status,
} from './api';
import {
  activeArea,
  firstTile,
  mirrorData,
  mirrorRoute,
  mirrorScope,
  monitorSize,
  type PadButton,
  type PadContext,
  padAction,
  padKey,
  scrollAreas,
  selectableRefs,
} from './mirror';
import { KEYS_TEXT, Pad, PAD_BUTTONS } from './Pad';

export const CAPTION_TEXT =
  'A copy of what the wall shows, made on this device. It is not a picture of the screen.';
export const SHOT_TEXT = 'Take a screenshot of the wall';
export const RELOAD_TEXT = 'Reload the wall';
export const MISSION_TEXT = 'Switch to Mission view';

/* ── the setup: what the page needs before it can poll ─────────────────────────────────────── */

export type Setup =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'no_feature' }
  | { readonly kind: 'missing' }
  | {
      readonly kind: 'ready';
      readonly display: DisplayView;
      readonly info: DesktopInfo | undefined;
      /** The `/api/features` list of the deck. */
      readonly deck: readonly string[];
    };

/** The notice of a setup that is not ready; `null` when it is ready. No pad shows in any of them. */
export function SetupNotice({
  setup,
  onRetry,
}: {
  readonly setup: Setup;
  readonly onRetry?: (() => void) | undefined;
}): ReactNode {
  switch (setup.kind) {
    case 'loading':
      return <Loading text="Loading the remote…" />;
    case 'error':
      return <ErrorNotice text={setup.text} onRetry={onRetry} />;
    case 'no_feature':
      return <p className="fd-note">{NO_REMOTE_TEXT}</p>;
    case 'missing':
      return <p className="fd-note">{GONE_TEXT}</p>;
    case 'ready':
      return null;
  }
}

/**
 * Reads the feature list, then the display and the desktop of its bound station. A deck without
 * `wall_control.v1` has no remote; a display that is not in the list does not exist any more.
 */
export async function loadSetup(
  env: Pick<SettingsEnv, 'capture' | 'displays' | 'desktop'>,
  id: string,
  stations: readonly StationView[],
): Promise<Setup> {
  const deck = (await env.capture.features()) ?? [];
  if (!deck.includes(REMOTE_FEATURE)) return { kind: 'no_feature' };
  const list = await env.displays.list();
  const display = list.displays.find((d) => d.display_id === id);
  if (display === undefined) return { kind: 'missing' };
  const station = stations.find((s) => s.station_id === display.binding?.station_id);
  const info =
    station === undefined
      ? undefined
      : await env.desktop.info(station.name).catch(() => station.info?.desktop);
  return { kind: 'ready', display, info, deck };
}

function useSetup(env: SettingsEnv, id: string, stations: readonly StationView[]) {
  const [setup, setSetup] = useState<Setup>({ kind: 'loading' });
  const [attempt, setAttempt] = useState(0);
  const bound = useRef(stations);
  bound.current = stations;
  useEffect(() => {
    let live = true;
    setSetup({ kind: 'loading' });
    loadSetup(env, id, bound.current).then(
      (next) => live && setSetup(next),
      (e: unknown) => live && setSetup({ kind: 'error', text: describe(e) }),
    );
    return () => {
      live = false;
    };
  }, [env, id, attempt]);
  return { setup, retry: () => setAttempt((n) => n + 1) };
}

/* ── the link line ─────────────────────────────────────────────────────────────────────────── */

/** The link of the wall page: connected, not connected, or an older board with "Reload the wall". */
export function LinkLine({
  link,
  onReload,
}: {
  readonly link: 'offline' | 'older' | 'ready';
  /** Present when the station's Desktop has this wall: the built `wall_reload` action. */
  readonly onReload: (() => void) | null;
}) {
  if (link === 'ready') {
    return (
      <p className="fd-remote-link">
        <span className="fd-dot" data-status="online" aria-hidden="true" /> Connected
      </p>
    );
  }
  return (
    <p className="fd-remote-link">
      <span className="fd-dot" data-status="idle" aria-hidden="true" />{' '}
      {link === 'offline' ? OFFLINE_TEXT : OLDER_TEXT}
      {link === 'older' && onReload !== null ? (
        <>
          {' '}
          <button type="button" className="fd-sm" onClick={onReload}>
            <Icon of={RefreshCw} size={14} /> {RELOAD_TEXT}
          </button>
        </>
      ) : null}
    </p>
  );
}

/* ── the footer and the screenshot ─────────────────────────────────────────────────────────── */

/**
 * The monitor that "Take a screenshot of the wall" captures: the bound monitor, only when the
 * display is bound, the station reports a desktop, and the deck and the station allow the capture of
 * that monitor. `null`: no button.
 */
export function screenshotMonitor(
  display: Pick<DisplayView, 'binding'>,
  info: DesktopInfo | undefined,
  deck: readonly string[] | null,
): string | null {
  const monitor = display.binding?.monitor;
  if (monitor === undefined || info === undefined) return null;
  return availability(deck, info, monitor, false).enabled ? monitor : null;
}

export function Footer({
  onMission,
  shot,
}: {
  readonly onMission: () => void;
  /** The screenshot control, or `null` when the wall has none. */
  readonly shot: ReactNode;
}) {
  return (
    <div className="fd-remote-foot">
      <div className="fd-remote-row">
        <button type="button" onClick={onMission}>
          {MISSION_TEXT}
        </button>
        {shot}
      </div>
      <details className="fd-remote-keys">
        <summary>Keys</summary>
        <p className="fd-note">{KEYS_TEXT}</p>
      </details>
    </div>
  );
}

const IDLE_SNAPSHOT = () => IDLE;

const storageOrNull = (): Storage | null => {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
};

/** The owner screenshot of the bound monitor (the built flow); it opens the artifact when it is done. */
function Shot({
  env,
  station,
  monitor,
}: {
  readonly env: SettingsEnv;
  readonly station: StationView;
  readonly monitor: string;
}) {
  const flow = useMemo(
    () =>
      new CaptureFlow({
        stationName: station.name,
        stationId: station.station_id,
        actions: env.capture,
        storage: storageOrNull(),
      }),
    [env.capture, station.name, station.station_id],
  );
  useEffect(() => {
    flow.attach();
    return () => flow.detach();
  }, [flow]);
  const state: FlowState = useSyncExternalStore(flow.subscribe, flow.getSnapshot, IDLE_SNAPSHOT);
  const startedHere = useRef(false);
  useEffect(() => {
    if (state.kind !== 'committed' || !startedHere.current) return;
    startedHere.current = false;
    const { artifact } = state.published;
    window.location.hash = `#/artifact/${encodeURIComponent(artifact.station_id)}/${encodeURIComponent(artifact.artifact_id)}`;
  }, [state]);
  return (
    <>
      <button
        type="button"
        disabled={isActive(state)}
        onClick={() => {
          startedHere.current = true;
          void flow.start(monitor);
        }}
      >
        <Icon of={ImageIcon} size={14} /> {SHOT_TEXT}
      </button>
      <CaptureBlock
        state={state}
        onCheck={() => void flow.check()}
        onDismiss={() => flow.dismiss()}
      />
    </>
  );
}

/* ── the mirror ────────────────────────────────────────────────────────────────────────────── */

const noNavigate: Navigate = () => {};

/** The owner's data and calls that the mirror draws (the same as the props of `WallPage`). */
export type Board = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly timelines?: Readonly<Record<string, Timeline>> | undefined;
  readonly watch?: ((key: SessionKey) => () => void) | undefined;
  readonly sessionActions?: ((key: SessionKey) => SessionActions) | undefined;
  readonly views?: ViewsActions | undefined;
  readonly artifacts?: ArtifactActions | undefined;
};

/**
 * The wall page at the logical size of the bound monitor, scaled down to the width of the box,
 * `inert`, with no key listener and no QR code (`mirror`). The route never changes here: the
 * directive of the deck decides it.
 */
function Mirror({
  size,
  wall,
}: {
  readonly size: { readonly w: number; readonly h: number };
  readonly wall: ComponentProps<typeof WallPage>;
}) {
  const box = useRef<HTMLDivElement>(null);
  const measured = useSize(box);
  const scale = measured === null ? 0.25 : measured.w / size.w;
  const style = {
    width: size.w,
    height: size.h,
    transform: `scale(${scale})`,
    '--fd-remote-h': `${size.h}px`,
  } as CSSProperties;
  return (
    <figure className="fd-remote-mirror">
      <div ref={box} className="fd-remote-box" style={{ aspectRatio: `${size.w} / ${size.h}` }}>
        <div className="fd-remote-screen" style={style} inert>
          <WallPage {...wall} initialSize={size} />
        </div>
      </div>
      <figcaption className="fd-note">{CAPTION_TEXT}</figcaption>
    </figure>
  );
}

/* ── the page ──────────────────────────────────────────────────────────────────────────────── */

type Props = {
  /** The display id of the route. */
  readonly id: string;
  readonly freshness: Freshness;
  readonly board: Board;
};

/** The remote of a display. A login that is not the owner's sees one line and no data. */
export function RemotePage(props: Props) {
  const env = useSettings();
  const people = usePeople();
  if (env === undefined || people.member !== null || people.self !== 'owner') {
    return (
      <Page title="Remote">
        <p className="fd-note">{OWNER_ONLY_TEXT}</p>
      </Page>
    );
  }
  return <Remote {...props} env={env} />;
}

function Remote({ env, ...props }: Props & { readonly env: SettingsEnv }) {
  const { setup, retry } = useSetup(env, props.id, props.board.stations);
  if (setup.kind !== 'ready') {
    return (
      <Page title="Remote" wide>
        <SetupNotice setup={setup} onRetry={retry} />
      </Page>
    );
  }
  return (
    <Connected {...props} env={env} display={setup.display} info={setup.info} deck={setup.deck} />
  );
}

function Connected({
  env,
  display,
  info,
  deck,
  board,
  freshness,
}: Props & {
  readonly env: SettingsEnv;
  readonly display: DisplayView;
  readonly info: DesktopInfo | undefined;
  readonly deck: readonly string[];
}) {
  const link = useMemo(
    () => new RemoteLink({ id: display.display_id, calls: env.displays }),
    [env.displays, display.display_id],
  );
  useEffect(() => {
    const visible = () => link.setVisible(document.visibilityState !== 'hidden');
    visible();
    link.start();
    document.addEventListener('visibilitychange', visible);
    return () => {
      document.removeEventListener('visibilitychange', visible);
      link.stop();
    };
  }, [link]);
  const snap = useSyncExternalStore(link.subscribe, link.getSnapshot, link.getSnapshot);
  const [chosen, setChosen] = useState<WallArea | null>(null);
  const [local, setLocal] = useState<Status | null>(null);
  useEffect(() => {
    if (local === null) return undefined;
    const timer = setTimeout(() => setLocal(null), STATUS_CLEAR_MS);
    return () => clearTimeout(timer);
  }, [local]);

  const station = board.stations.find((s) => s.station_id === display.binding?.station_id);
  const walls = info?.walls ?? [];
  const reload =
    station === undefined || !walls.some((w) => w.display_id === display.display_id)
      ? null
      : () =>
          env.desktop
            .act(station.name, { action: 'wall_reload', display_id: display.display_id })
            .then(
              () => setLocal({ text: 'Reloading the wall.', tone: 'ok' }),
              (e: unknown) => setLocal({ text: describe(e), tone: 'error' }),
            );
  const monitor = screenshotMonitor(display, info, deck);
  const view = snap.view;
  const header = (children: ReactNode) => (
    <Page
      title="Remote"
      wide
      description={<span className="fd-wide">{display.name}</span>}
      className="fd-remote"
    >
      {children}
    </Page>
  );
  if (view === null) {
    return header(
      snap.problem === null ? (
        <Loading text="Loading the remote…" />
      ) : (
        <ErrorNotice text={snap.problem} />
      ),
    );
  }

  const current = view.directive.directive;
  const route = mirrorRoute(display.ceiling, current);
  const scope = mirrorScope(display.ceiling, current);
  const data = mirrorData(board.sessions, board.stations, scope);
  const area = activeArea(route.mode, chosen);
  const ctx: PadContext = {
    current,
    selected: view.state.selected,
    items: selectableRefs(route, data.sessions),
    first: firstTile(route, data.sessions),
    area,
  };
  const off = new Set<PadButton>(PAD_BUTTONS.filter((b) => padAction(b, ctx) === null));
  const act = (button: PadButton) => {
    const action = padAction(button, ctx);
    if (action !== null) void link.press(action);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    const key = padKey({
      key: event.key,
      ctrlKey: event.ctrlKey,
      metaKey: event.metaKey,
      altKey: event.altKey,
      tag: (event.target as Element).tagName,
    });
    if (key === null) return;
    event.preventDefault();
    if (key === 'previous_area' || key === 'next_area') {
      const areas = scrollAreas(route.mode);
      const at = areas.findIndex((a) => a.area === area);
      const next = areas[at + (key === 'next_area' ? 1 : -1)];
      if (next !== undefined) setChosen(next.area);
    } else {
      act(key);
    }
  };
  const status = snap.status ?? local;
  return header(
    <>
      <LinkLine link={view.link} onReload={reload} />
      {snap.problem === null ? null : <ErrorNotice text={snap.problem} />}
      <div className="fd-remote-body">
        <Mirror
          size={monitorSize(info, display.binding?.monitor ?? null)}
          wall={{
            route,
            navigate: noNavigate,
            owner: true,
            mirror: true,
            sessions: data.sessions,
            stations: data.stations,
            freshness,
            timelines: board.timelines,
            watch: board.watch,
            sessionActions: board.sessionActions,
            views: board.views,
            artifacts: board.artifacts,
            remote: {
              state: view.state,
              holdUntil: snap.mirror.holdUntil,
              effects: snap.mirror.effects,
            },
          }}
        />
        <div className="fd-remote-side">
          <Pad
            mode={route.mode}
            off={off}
            area={area}
            onPress={act}
            onArea={setChosen}
            onKeyDown={onKeyDown}
          />
          <p
            role="status"
            className={status?.tone === 'error' ? 'fd-fielderror' : 'fd-note'}
            data-tone={status?.tone}
          >
            {status?.text ?? ''}
          </p>
          <Footer
            onMission={() =>
              void link.press({
                kind: 'switch',
                directive: { mode: 'mission', pin: false, filter: null },
              })
            }
            shot={
              monitor === null || station === undefined ? null : (
                <Shot env={env} station={station} monitor={monitor} />
              )
            }
          />
        </div>
      </div>
    </>,
  );
}
