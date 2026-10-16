// R13 P1 (design §4): `#/wall`, the read-only command center for a wide screen. One root fills the
// viewport; a shape class picks the grid; the route holds the mode, the filter and the scale.
// Nothing here writes to a session: the keys only navigate, and no control renders.
import { type CSSProperties, useEffect, useMemo, useRef } from 'react';
import type { ArtifactActions } from '../artifacts/actions';
import type { Navigate, WallRoute } from '../app/routes';
import { sessionTitle } from '../app/sessions';
import type { DeckSession, SessionKey, StationView } from '../gen';
import type { Freshness, Timeline } from '../runtime/state';
import type { ViewsActions } from '../runtime/views';
import type { SessionActions } from '../session/commands';
import { useNow } from '../ui/useNow';
import { refOf } from '../watch/tiles';
import { MAX_TILES } from '../watch/tiles';
import { AmbientPane } from './AmbientPane';
import { ambientOffset, AMBIENT_AFTER_MIN } from './ambient';
import { ArtifactPane } from './ArtifactPane';
import { NeedsArea, SideArea, StreamArea, TilesArea } from './Areas';
import { type AutoState, initialAuto, nextAutoFocus } from './autofocus';
import { ChatPane } from './ChatPane';
import { DiffPane } from './DiffPane';
import { keyOf, timelineOf, useSize, useTimelines } from './hooks';
import { asksOf, fleetSignature, needsList, stepMode, tileSessions, wallSessions } from './model';
import { RacePane } from './RacePane';
import { ReviewPane } from './ReviewPane';
import { baseRemPx, shapeOf, tileLimit } from './shape';

type Props = {
  readonly route: WallRoute;
  readonly navigate: Navigate;
  /** `people.member === null && people.self === 'owner'`. */
  readonly owner: boolean;
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly timelines?: Readonly<Record<string, Timeline>> | undefined;
  readonly watch?: ((key: SessionKey) => () => void) | undefined;
  readonly sessionActions?: ((key: SessionKey) => SessionActions) | undefined;
  readonly views?: ViewsActions | undefined;
  readonly artifacts?: ArtifactActions | undefined;
  /** The viewport size at the first render (tests, a server render). */
  readonly initialSize?: { readonly w: number; readonly h: number };
  /** Unix ms of "now" (tests). */
  readonly now?: number;
  /** The deck origin of the QR codes (tests); the page origin by default. */
  readonly origin?: string;
};

const sessionRefOf = (route: WallRoute): string | null =>
  route.session === undefined ? null : `${route.session.station}/${route.session.id}`;

const pairOf = (ref: string): { station: string; id: string } | null => {
  const at = ref.indexOf('/');
  return at <= 0 ? null : { station: ref.slice(0, at), id: ref.slice(at + 1) };
};

/** The wall for a login that is not the owner's: a notice and no data. */
export function WallDenied() {
  return (
    <main className="fd-wall-root" data-mode="denied">
      <p className="fd-wall-denied">The wall needs the owner login.</p>
    </main>
  );
}

export function WallPage(props: Props) {
  if (!props.owner) return <WallDenied />;
  return <Wall {...props} />;
}

function Wall({
  route,
  navigate,
  sessions,
  stations,
  freshness,
  timelines,
  watch,
  sessionActions,
  views,
  artifacts,
  initialSize,
  now: givenNow,
  origin,
}: Props) {
  const root = useRef<HTMLDivElement>(null);
  const tilesArea = useRef<HTMLElement>(null);
  const clock = useNow(givenNow, 1000);
  const rootSize = useSize(root);
  const size =
    rootSize ??
    initialSize ??
    (typeof window === 'undefined'
      ? { w: 3440, h: 1440 }
      : { w: window.innerWidth, h: window.innerHeight });
  const shape = shapeOf(size.w, size.h, route.shape);
  const mode = route.mode;
  const filtered = useMemo(() => wallSessions(sessions, route), [sessions, route]);
  const needs = useMemo(() => needsList(filtered), [filtered]);
  const shownStations =
    route.stations.length === 0
      ? stations
      : stations.filter((s) => route.stations.includes(s.station_id));

  // The tile count follows the size of the tiles area (decision 6).
  const area = useSize(tilesArea, mode === 'mission');
  const limit =
    area === null ? MAX_TILES : tileLimit(area.w, area.h, baseRemPx(size.h), route.scale);
  const tiles = useMemo(() => tileSessions(filtered).slice(0, limit), [filtered, limit]);
  const focusRef = sessionRefOf(route);
  const focusCard = focusRef === null ? undefined : sessions.find((c) => refOf(c) === focusRef);
  // The timelines that the mode needs: the tiles in mission, the one session in focus.
  const watched = mode === 'mission' ? tiles : mode === 'focus' && focusCard ? [focusCard] : [];
  const failed = useTimelines(watched, watch);

  // The latest values, for the key handler and the timer.
  const latest = useRef({ route, sessions: filtered, tiles });
  latest.current = { route, sessions: filtered, tiles };

  // Idle: after `AMBIENT_AFTER_MIN` with no change of the fleet and no key, the wall goes to
  // ambient; a change or a key returns it. Auto-focus follows the open requests (decision 10).
  const sig = fleetSignature(sessions);
  const asks = useMemo(() => asksOf(needs), [needs]);
  const lastSig = useRef(sig);
  const lastChange = useRef(Date.now());
  const idleBack = useRef<WallRoute | null>(null);
  const previousAsks = useRef(asks);
  // The requests that are open when the board first goes live are not new.
  const armed = useRef(false);
  const liveNow = freshness === 'live';
  const auto = useRef<AutoState>(initialAuto(route.mode, focusRef));
  // `clock` ticks every second; `sig` changes with the fleet.
  useEffect(() => {
    let r = latest.current.route;
    const t = Date.now();
    const changed = sig !== lastSig.current;
    if (changed) {
      lastSig.current = sig;
      lastChange.current = t;
    }
    if (r.mode === 'ambient' && idleBack.current !== null && changed) {
      r = idleBack.current;
      idleBack.current = null;
    } else if (r.mode !== 'ambient' && t - lastChange.current >= AMBIENT_AFTER_MIN * 60_000) {
      idleBack.current = r;
      r = { ...r, mode: 'ambient' };
    }
    if (!liveNow || !armed.current) {
      previousAsks.current = asks;
      armed.current = liveNow;
      if (r !== latest.current.route) navigate(r, { replace: true });
      return;
    }
    const before = initialAuto(r.mode, sessionRefOf(r));
    const state: AutoState = { ...before, hold: auto.current.hold, queue: auto.current.queue };
    const next = nextAutoFocus(previousAsks.current, asks, t, state, r.pin);
    previousAsks.current = asks;
    auto.current = next;
    if (next.mode !== state.mode || next.session !== state.session) {
      const { session: _drop, ...rest } = r;
      const pair = next.session === null ? null : pairOf(next.session);
      r = { ...rest, mode: next.mode, ...(pair === null ? {} : { session: pair }) };
    }
    if (r !== latest.current.route) navigate(r, { replace: true });
  }, [sig, clock]);

  // The keys of the wall only navigate. This listener runs first (capture) and stops the key, so
  // the board hotkeys do not act on the wall.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      event.stopPropagation();
      lastChange.current = Date.now();
      const { route: r, tiles: shownTiles } = latest.current;
      if (r.mode === 'ambient' && idleBack.current !== null) {
        const back = idleBack.current;
        idleBack.current = null;
        navigate(back, { replace: true });
        return;
      }
      if (event.ctrlKey || event.metaKey || event.altKey) return;
      if (event.key === 'Escape') {
        if (r.mode !== 'mission') navigate({ ...r, mode: 'mission' }, { replace: true });
      } else if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') {
        event.preventDefault();
        const active = document.activeElement?.closest<HTMLElement>('[data-ref]')?.dataset['ref'];
        const first = shownTiles[0];
        const fallbackRef = active ?? (first === undefined ? null : refOf(first));
        const next = stepMode(
          r,
          event.key === 'ArrowRight' ? 1 : -1,
          fallbackRef === null ? null : pairOf(fallbackRef),
        );
        if (next !== r) navigate(next, { replace: true });
      } else if (event.key === 'Enter') {
        const ref = (event.target as HTMLElement | null)?.closest<HTMLElement>('[data-ref]')
          ?.dataset['ref'];
        const pair = ref === undefined ? null : pairOf(ref);
        if (pair !== null) {
          event.preventDefault();
          navigate({ ...r, mode: 'focus', session: pair }, { replace: true });
        }
      }
    };
    document.addEventListener('keydown', onKey, true);
    return () => document.removeEventListener('keydown', onKey, true);
  }, [navigate]);

  const live = freshness === 'live';
  const minute = Math.floor(clock / 60_000);
  const offset = ambientOffset(minute);
  const style = {
    '--fd-wall-scale': route.scale,
    fontSize: 'calc(clamp(14px, 100dvh / 80, 28px) * var(--fd-wall-scale))',
  } as CSSProperties;
  const pageOrigin = origin ?? (typeof window === 'undefined' ? '' : window.location.origin);

  const body = (() => {
    switch (mode) {
      case 'mission':
        return (
          <div className="fd-wall-grid">
            <NeedsArea needs={needs} stations={stations} origin={pageOrigin} now={clock} />
            <TilesArea
              cards={tiles}
              stations={stations}
              timelines={timelines}
              freshness={freshness}
              failed={failed}
              areaRef={tilesArea}
            />
            {shape === 'portrait' ? null : (
              <SideArea sessions={filtered} stations={shownStations} now={clock} />
            )}
            {shape === 'standard' ? null : <StreamArea views={views} live={live} now={clock} />}
          </div>
        );
      case 'focus':
      case 'review': {
        if (focusCard === undefined) {
          return <p className="fd-wall-note">This session is not on the board.</p>;
        }
        const actions = sessionActions?.(keyOf(focusCard));
        if (mode === 'review') {
          return (
            <div className="fd-wall-single">
              <h2 className="fd-wall-h">{sessionTitle(focusCard)}</h2>
              <ReviewPane card={focusCard} actions={actions} live={live} />
            </div>
          );
        }
        const timeline = timelineOf(timelines, keyOf(focusCard));
        return (
          <div className="fd-wall-focus" data-session={focusCard.summary.session_id}>
            <div className="fd-wall-focus-chat">
              <h2 className="fd-wall-h">{sessionTitle(focusCard)}</h2>
              <ChatPane
                events={timeline?.events ?? []}
                ready={timeline?.ready === true}
                unavailable={failed.has(refOf(focusCard)) || timeline?.error != null}
              />
            </div>
            <DiffPane card={focusCard} actions={actions} title="Changes" />
          </div>
        );
      }
      case 'race':
        return route.batch === undefined ? null : (
          <RacePane
            batch={route.batch}
            sessions={sessions}
            stations={stations}
            timelines={timelines}
            watch={watch}
            sessionActions={sessionActions}
          />
        );
      case 'artifact':
        return route.artifact === undefined ? null : (
          <ArtifactPane
            station={route.artifact.station}
            id={route.artifact.id}
            artifacts={artifacts}
          />
        );
      case 'ambient':
        return (
          <div
            className="fd-wall-ambient"
            style={{ transform: `translate(${offset.x}px, ${offset.y}px)` }}
          >
            <AmbientPane sessions={filtered} stations={stations} needs={needs} now={clock} />
          </div>
        );
    }
  })();

  return (
    <div
      ref={root}
      className="fd-wall-root"
      data-shape={shape}
      data-mode={mode}
      data-testid="wall"
      style={style}
    >
      <p className="fd-wall-corner">
        <span>Fleet Deck wall</span>{' '}
        <span role="status" data-state={live ? 'live' : 'reconnecting'}>
          {live ? 'Live' : 'Reconnecting'}
        </span>
      </p>
      {body}
    </div>
  );
}
