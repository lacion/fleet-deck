// B17 §3.3 (design `docs/v2/design/wall-control.md`): the pure parts of the Remote page. The mirror
// state follows the polls of `GET /api/displays/{id}/remote`; the pad turns a button into a control
// or a navigation; the scope of the mirror is the display ceiling narrowed by the directive filter.
// Nothing here calls the deck or touches the document.
import { directiveRoute } from '../display/api';
import type { WallMode, WallRoute } from '../app/routes';
import type {
  DesktopInfo,
  DeckSession,
  DirectiveBody,
  DirectiveFilter,
  ScrollStep,
  SessionRef,
  StationView,
  WallArea,
  WallControl,
  WallRemoteView,
} from '../gen';
import { MAX_TILES, refOf } from '../watch/tiles';
import { siblings } from '../compare/compare';
import {
  applyFrame,
  type ControlEffect,
  type ControlState,
  initialControl,
  selectable,
  stepSelection,
} from '../wall/control';
import { needsList, stepMode, tileSessions, wallSessions } from '../wall/model';
import { RACE_COLUMNS } from '../wall/RacePane';

/* ── the mirror state (§3.3.3) ─────────────────────────────────────────────────────────────── */

export type MirrorState = {
  /** The relay of the last view (`null` before the first). */
  readonly incarnation: string | null;
  /** The `seq` of the last view: the next poll asks for the frames above it. */
  readonly cursor: number;
  /** Unix ms until which auto-focus and the ambient timer wait (0: no hold). */
  readonly holdUntil: number;
  /** The relative scroll effects that the last view gave, in `seq` order. */
  readonly effects: readonly ControlEffect[];
};

const NO_EFFECTS: readonly ControlEffect[] = [];

export const initialMirror: MirrorState = {
  incarnation: null,
  cursor: 0,
  holdUntil: 0,
  effects: NO_EFFECTS,
};

/** A rebase: the relative scroll areas go back to their default position (the chat at its end). */
export const REBASE_EFFECTS: readonly ControlEffect[] = [
  { kind: 'scroll', area: 'needs', step: 'top' },
  { kind: 'scroll', area: 'stream', step: 'top' },
  { kind: 'scroll', area: 'changes', step: 'top' },
  { kind: 'scroll', area: 'artifact', step: 'top' },
  { kind: 'scroll', area: 'chat', step: 'bottom' },
];

/**
 * The mirror state after one `WallRemoteView`. The view, the selection and the long-frame position
 * come from the view itself (the deck's checkpoint); this state holds only what a view adds: the
 * cursor, the hold and the relative scroll effects.
 *
 * 1. Rebase when it is the first view, when the incarnation differs (the deck restarted) or when
 *    the view is not complete (a gap): the cursor is `seq`, the relative areas return to their
 *    default position, and no frame gives an effect.
 * 2. Else each frame above the cursor, in `seq` order, gives its effect by the rule of the wall
 *    (`applyFrame`, §3.1.5): a `control` frame of the shown generation that scrolls a relative area.
 */
export function applyView(state: MirrorState, view: WallRemoteView, now: number): MirrorState {
  const holdUntil = view.hold_left_ms > 0 ? now + view.hold_left_ms : 0;
  const generation = view.directive.generation;
  if (state.incarnation === null || view.incarnation !== state.incarnation || !view.complete) {
    return { incarnation: view.incarnation, cursor: view.seq, holdUntil, effects: REBASE_EFFECTS };
  }
  let control: ControlState = {
    ...initialControl,
    incarnation: state.incarnation,
    seq: state.cursor,
    generation,
    remote: view.state,
  };
  const effects: ControlEffect[] = [];
  for (const frame of [...view.controls].sort((a, b) => a.seq - b.seq)) {
    if (frame.seq <= state.cursor) continue;
    const out = applyFrame(control, frame, { shown: generation, now });
    control = out.state;
    if (out.effect.kind === 'scroll') effects.push(out.effect);
  }
  return {
    incarnation: view.incarnation,
    cursor: view.seq,
    holdUntil,
    effects: effects.length === 0 ? NO_EFFECTS : effects,
  };
}

/* ── the scope and the route of the mirror (§3.3.2 item 2) ─────────────────────────────────── */

export type MirrorScope = {
  /** The stations that the wall may show: the ceiling narrowed by the filter. */
  readonly stations: readonly string[];
  /** A repository name; empty: all repositories. */
  readonly repo: string;
};

/**
 * The display ceiling narrowed by the filter of the directive (the same rule as
 * `EffectiveScope::narrowed` of the deck): a filter with stations keeps those of the ceiling, and
 * the repository comes from the filter.
 */
export function mirrorScope(ceiling: readonly string[], body: DirectiveBody): MirrorScope {
  const filter = body.filter;
  const stations =
    filter === null || filter.stations.length === 0
      ? [...ceiling]
      : ceiling.filter((s) => filter.stations.includes(s));
  return { stations, repo: filter?.repo ?? '' };
}

/** The route of the mirror: the route of the directive, with the scope of the display. */
export function mirrorRoute(ceiling: readonly string[], body: DirectiveBody): WallRoute {
  const scope = mirrorScope(ceiling, body);
  return { ...directiveRoute(body), stations: scope.stations, repo: scope.repo };
}

/**
 * The data that the mirror draws: the owner's sessions and stations inside the scope. An empty
 * station list in a route means "all stations", so the data is cut here, and a narrowed scope with
 * no station shows nothing.
 */
export function mirrorData(
  sessions: readonly DeckSession[],
  stations: readonly StationView[],
  scope: MirrorScope,
): { readonly sessions: readonly DeckSession[]; readonly stations: readonly StationView[] } {
  const allowed = new Set(scope.stations);
  return {
    sessions: wallSessions(
      sessions.filter((c) => allowed.has(c.station_id)),
      { stations: [], repo: scope.repo },
    ),
    stations: stations.filter((s) => allowed.has(s.station_id)),
  };
}

/** The logical size of a monitor: pixels × 100 / `scale_pct`. No binding or no monitor: 1920×1080. */
export function monitorSize(
  info: Pick<DesktopInfo, 'monitors'> | undefined,
  monitor: string | null,
): { readonly w: number; readonly h: number } {
  const found = monitor === null ? undefined : info?.monitors.find((m) => m.name === monitor);
  if (found === undefined || found.scale_pct <= 0 || found.width <= 0 || found.height <= 0) {
    return { w: 1920, h: 1080 };
  }
  return {
    w: Math.round((found.width * 100) / found.scale_pct),
    h: Math.round((found.height * 100) / found.scale_pct),
  };
}

/* ── the pad (§3.3.2 item 3) ───────────────────────────────────────────────────────────────── */

export type PadButton =
  | 'previous_view'
  | 'next_view'
  | 'previous_item'
  | 'next_item'
  | 'open'
  | 'back'
  | 'line_up'
  | 'line_down'
  | 'scroll_up'
  | 'scroll_down'
  | 'top'
  | 'bottom';

/** What a button sends: one control, or one navigation (an owner directive). */
export type PadAction =
  | { readonly kind: 'control'; readonly control: WallControl }
  | { readonly kind: 'navigate'; readonly directive: DirectiveBody };

export type PadContext = {
  /** The directive of the last `WallRemoteView`; a new directive keeps its `pin` and `filter`. */
  readonly current: DirectiveBody;
  /** The selected item of the deck's checkpoint. */
  readonly selected: SessionRef | null;
  /** The selectable refs (`station/session`) in reading order (`selectableRefs`). */
  readonly items: readonly string[];
  /** The first tile of the wall, the fallback target of Focus and Review. */
  readonly first: { readonly station: string; readonly id: string } | null;
  /** The scroll area that the segmented control chose; `null`: the mode has none. */
  readonly area: WallArea | null;
};

/** The inverse of `directiveRoute`; `null` when the route lacks the target of its mode. */
export function routeDirective(
  route: WallRoute,
  pin: boolean,
  filter: DirectiveFilter | null,
): DirectiveBody | null {
  switch (route.mode) {
    case 'mission':
    case 'ambient':
      return { mode: route.mode, pin, filter };
    case 'focus':
    case 'review':
      return route.session === undefined
        ? null
        : {
            mode: route.mode,
            session: { station_id: route.session.station, session_id: route.session.id },
            pin,
            filter,
          };
    case 'race':
      return route.batch === undefined
        ? null
        : { mode: 'race', batch_id: route.batch, pin, filter };
    case 'artifact':
      return route.artifact?.version === undefined
        ? null
        : {
            mode: 'artifact',
            artifact: {
              station_id: route.artifact.station,
              artifact_id: route.artifact.id,
              version: route.artifact.version,
            },
            app: false,
            pin,
            filter,
          };
    case 'page':
      return route.page === undefined
        ? null
        : { mode: 'page', page: { page_id: route.page.id }, pin, filter };
  }
}

/** A selectable ref `station/session` as a session reference. */
function refPair(ref: string): SessionRef | null {
  const at = ref.indexOf('/');
  return at <= 0 ? null : { station_id: ref.slice(0, at), session_id: ref.slice(at + 1) };
}

/** The two routes show the same thing: the same mode and the same target. */
const sameRoute = (a: WallRoute, b: WallRoute): boolean =>
  a.mode === b.mode &&
  JSON.stringify([a.session, a.batch, a.artifact, a.page]) ===
    JSON.stringify([b.session, b.batch, b.artifact, b.page]);

const STEP_OF: Readonly<Record<string, ScrollStep>> = {
  line_up: 'line_up',
  line_down: 'line_down',
  scroll_up: 'page_up',
  scroll_down: 'page_down',
  top: 'top',
  bottom: 'bottom',
};

/**
 * What a button sends, or `null` when it is disabled (the table of §3.3.2):
 *
 * - "Previous view", "Next view": a navigation to the stepped route (`stepMode`, with the selected
 *   item or the first tile as the target); disabled when the stepped route is the route.
 * - "Open": a `focus` navigation to the selected item; only in Mission and Race, with a selection.
 * - "Back": in Mission the control `select { item: null }` (disabled without a selection); in any
 *   other mode a `mission` navigation.
 * - "Previous item", "Next item": the control `select` of the stepped item; disabled without items.
 * - The scroll buttons: the control `scroll { area, step }`; disabled when the mode has no area.
 */
export function padAction(button: PadButton, ctx: PadContext): PadAction | null {
  const { current, selected } = ctx;
  const route = directiveRoute(current);
  const selectedPair =
    selected === null ? null : { station: selected.station_id, id: selected.session_id };
  const selectedRef = selected === null ? null : `${selected.station_id}/${selected.session_id}`;
  switch (button) {
    case 'previous_view':
    case 'next_view': {
      const stepped = stepMode(route, button === 'next_view' ? 1 : -1, selectedPair ?? ctx.first);
      if (sameRoute(stepped, route)) return null;
      const directive = routeDirective(stepped, current.pin, current.filter);
      return directive === null ? null : { kind: 'navigate', directive };
    }
    case 'open': {
      if ((route.mode !== 'mission' && route.mode !== 'race') || selectedPair === null) return null;
      const directive = routeDirective(
        { ...route, mode: 'focus', session: selectedPair },
        current.pin,
        current.filter,
      );
      return directive === null ? null : { kind: 'navigate', directive };
    }
    case 'back':
      if (route.mode === 'mission') {
        return selected === null
          ? null
          : { kind: 'control', control: { op: 'select', item: null } };
      }
      return {
        kind: 'navigate',
        directive: { mode: 'mission', pin: current.pin, filter: current.filter },
      };
    case 'previous_item':
    case 'next_item': {
      const ref = stepSelection(
        ctx.items,
        selectedRef,
        button === 'next_item' ? 'next' : 'previous',
      );
      const item = ref === null ? null : refPair(ref);
      return item === null ? null : { kind: 'control', control: { op: 'select', item } };
    }
    default: {
      const step = STEP_OF[button];
      return ctx.area === null || step === undefined
        ? null
        : { kind: 'control', control: { op: 'scroll', area: ctx.area, step } };
    }
  }
}

/**
 * The selectable refs of the mirror, in reading order: Mission lists the needs items and then the
 * tiles (no repeats), Race its columns, every other mode none. `sessions` is the data of the
 * mirror (already inside the scope).
 */
export function selectableRefs(
  route: WallRoute,
  sessions: readonly DeckSession[],
): readonly string[] {
  const filtered = wallSessions(sessions, route);
  const needs = needsList(filtered).map((n) => refOf(n.card));
  const tiles = tileSessions(filtered).slice(0, MAX_TILES).map(refOf);
  const columns =
    route.batch === undefined
      ? []
      : siblings(sessions, route.batch).slice(0, RACE_COLUMNS).map(refOf);
  return selectable(route, needs, tiles, columns);
}

/** The first tile of the wall (the target of Focus and Review when nothing is selected). */
export function firstTile(
  route: WallRoute,
  sessions: readonly DeckSession[],
): { readonly station: string; readonly id: string } | null {
  const first = tileSessions(wallSessions(sessions, route))[0];
  return first === undefined ? null : { station: first.station_id, id: first.summary.session_id };
}

/* ── the scroll areas and the keys ─────────────────────────────────────────────────────────── */

export type AreaChoice = { readonly area: WallArea; readonly label: string };

/** The scroll areas that a mode has, with the words of the segmented control. */
export function scrollAreas(mode: WallMode): readonly AreaChoice[] {
  switch (mode) {
    case 'mission':
      return [
        { area: 'needs', label: 'Needs you' },
        { area: 'stream', label: 'Activity' },
      ];
    case 'focus':
      return [
        { area: 'chat', label: 'Chat' },
        { area: 'changes', label: 'Changes' },
      ];
    case 'review':
      return [{ area: 'changes', label: 'Changes' }];
    case 'artifact':
      return [{ area: 'artifact', label: 'Artifact' }];
    case 'page':
      return [{ area: 'page', label: 'Page' }];
    case 'race':
    case 'ambient':
      return [];
  }
}

/** The area in use: the chosen one when the mode has it, else the first of the mode, else `null`. */
export function activeArea(mode: WallMode, chosen: WallArea | null): WallArea | null {
  const areas = scrollAreas(mode);
  return areas.find((a) => a.area === chosen)?.area ?? areas[0]?.area ?? null;
}

/** The key of a pad: a button, or a move of the scroll area. */
export type PadKey = PadButton | 'previous_area' | 'next_area';

/**
 * The keys of the Remote page, read only while the focus is inside the pad (the pad owns the
 * handler; the document has none). Each pair keeps the order of its words: the first key is the
 * previous one or the upward one.
 */
const KEY_OF: Readonly<Record<string, PadKey>> = {
  ArrowLeft: 'previous_view',
  ArrowRight: 'next_view',
  j: 'previous_item',
  k: 'next_item',
  Enter: 'open',
  Escape: 'back',
  ArrowUp: 'line_up',
  ArrowDown: 'line_down',
  PageUp: 'scroll_up',
  PageDown: 'scroll_down',
  Home: 'top',
  End: 'bottom',
  '[': 'previous_area',
  ']': 'next_area',
};

/** What a key event needs. */
export type KeyLike = {
  readonly key: string;
  readonly ctrlKey: boolean;
  readonly metaKey: boolean;
  readonly altKey: boolean;
  /** The tag name of the event target, upper case. */
  readonly tag: string;
};

/**
 * The pad key of an event, or `null`. A modifier key does nothing. A field keeps its own keys, and a
 * button, a link or a summary keeps Enter (it presses itself).
 */
export function padKey(event: KeyLike): PadKey | null {
  if (event.ctrlKey || event.metaKey || event.altKey) return null;
  if (event.tag === 'INPUT' || event.tag === 'SELECT' || event.tag === 'TEXTAREA') return null;
  if (
    event.key === 'Enter' &&
    (event.tag === 'BUTTON' || event.tag === 'A' || event.tag === 'SUMMARY')
  ) {
    return null;
  }
  return KEY_OF[event.key.length === 1 ? event.key.toLowerCase() : event.key] ?? null;
}
