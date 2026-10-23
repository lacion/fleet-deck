import { useCallback, useEffect, useState } from 'react';

export type Tab = 'chat' | 'changes' | 'files' | 'artifacts' | 'people';

/** R13 P1: the modes of the wall. */
export type WallMode = 'mission' | 'focus' | 'review' | 'race' | 'artifact' | 'ambient';
/** R13 P1: the shape classes of the wall. */
export type WallShape = 'ultra' | 'wide' | 'standard' | 'portrait';

export const WALL_MODES: readonly WallMode[] = [
  'mission',
  'focus',
  'review',
  'race',
  'artifact',
  'ambient',
];
export const WALL_SHAPES: readonly WallShape[] = ['ultra', 'wide', 'standard', 'portrait'];
export const WALL_SCALE_MIN = 0.75;
export const WALL_SCALE_MAX = 2;

export type WallRoute = {
  readonly name: 'wall';
  readonly mode: WallMode;
  /** Station ids; empty: all stations. */
  readonly stations: readonly string[];
  /** A repository name; empty: all repositories. */
  readonly repo: string;
  readonly scale: number;
  readonly shape: 'auto' | WallShape;
  readonly session?: { readonly station: string; readonly id: string };
  readonly batch?: string;
  /** `version`: a wall display shows one explicit version (a directive); absent: the latest. */
  readonly artifact?: {
    readonly station: string;
    readonly id: string;
    readonly version?: number;
  };
  /** `pin=1`: auto-focus is off. */
  readonly pin: boolean;
};

export type Route =
  | { readonly name: 'home' }
  | { readonly name: 'fleet' }
  | { readonly name: 'new' }
  | { readonly name: 'plans' }
  | { readonly name: 'automations' }
  | { readonly name: 'stations' }
  | { readonly name: 'files' }
  | { readonly name: 'watch' }
  | { readonly name: 'activity' }
  | { readonly name: 'terminals' }
  | { readonly name: 'more' }
  | { readonly name: 'compare'; readonly batch: string }
  | WallRoute
  /** B17: the remote of one wall display (owner only). */
  | { readonly name: 'remote'; readonly id: string }
  | { readonly name: 'artifacts' }
  | { readonly name: 'projects' }
  | { readonly name: 'project'; readonly id: string }
  | { readonly name: 'shares' }
  | { readonly name: 'spaces' }
  | { readonly name: 'settings'; readonly tab: 'deck' }
  | { readonly name: 'settings'; readonly tab: 'displays' }
  | { readonly name: 'settings'; readonly tab: 'station'; readonly station: string }
  | {
      readonly name: 'settings';
      readonly tab: 'accounts';
      /** `?station=<id>`; `null`: the first station. */
      readonly station: string | null;
      /** `?finish=<transaction>`: the OAuth callback came back to the board. */
      readonly finish: string | null;
      /** `?oauth_error=<code>`: the provider or the deck refused the login. */
      readonly oauthError: string | null;
    }
  | {
      readonly name: 'artifact';
      readonly station: string;
      readonly id: string;
      /** `null`: the latest version. */
      readonly version: number | null;
    }
  | {
      readonly name: 'session';
      readonly station: string;
      readonly session: string;
      readonly tab: Tab;
      /** B6: `?agent=<node id>`: the subagent that the chat tab shows; only on the chat tab. */
      readonly agent?: string;
    };

const HOME: Route = { name: 'home' };

const PLAIN = [
  'fleet',
  'new',
  'plans',
  'automations',
  'stations',
  'files',
  'watch',
  'activity',
  'terminals',
  'more',
  'artifacts',
  'projects',
  'shares',
  'spaces',
] as const;

/** `#/artifact/<station>/<id>[/<version>]`; a bad part is the home. */
function parseArtifact(parts: readonly string[]): Route {
  try {
    const station = decodeURIComponent(parts[2] ?? '');
    const id = decodeURIComponent(parts[3] ?? '');
    if (station === '' || id === '') return HOME;
    if (parts.length === 4) return { name: 'artifact', station, id, version: null };
    const version = Number(parts[4]);
    return Number.isInteger(version) && version >= 1
      ? { name: 'artifact', station, id, version }
      : HOME;
  } catch {
    return HOME;
  }
}

/** `#/settings`, `#/settings/station/<id>` and `#/settings/accounts[?station=&finish=&oauth_error=]` (DL-172, DL-173). */
function parseSettings(parts: readonly string[], query: string): Route {
  if (parts.length === 2) return { name: 'settings', tab: 'deck' };
  if (parts.length === 3 && parts[2] === 'accounts') {
    const q = new URLSearchParams(query);
    const text = (name: string) => {
      const v = q.get(name);
      return v === null || v === '' ? null : v;
    };
    return {
      name: 'settings',
      tab: 'accounts',
      station: text('station'),
      finish: text('finish'),
      oauthError: text('oauth_error'),
    };
  }
  if (parts.length === 3 && parts[2] === 'displays') return { name: 'settings', tab: 'displays' };
  if (parts.length === 4 && parts[2] === 'station') {
    try {
      const station = decodeURIComponent(parts[3] ?? '');
      return station === '' ? HOME : { name: 'settings', tab: 'station', station };
    } catch {
      return HOME;
    }
  }
  return HOME;
}

/** `station/id` as a pair; `undefined` for a bad text. */
function refPair(text: string | null): { station: string; id: string } | undefined {
  if (text === null) return undefined;
  const at = text.indexOf('/');
  if (at <= 0 || at === text.length - 1) return undefined;
  return { station: text.slice(0, at), id: text.slice(at + 1) };
}

/** A wall route with its defaults (R13 P1). */
export const wallDefaults: WallRoute = {
  name: 'wall',
  mode: 'mission',
  stations: [],
  repo: '',
  scale: 1,
  shape: 'auto',
  pin: false,
};

/** `#/wall?mode=&stations=&repo=&scale=&shape=&session=&batch=&artifact=&pin=` (R13 P1). */
function parseWall(query: string): WallRoute {
  const q = new URLSearchParams(query);
  const mode = q.get('mode');
  const shape = q.get('shape');
  const scaleText = q.get('scale');
  const scaleNumber = scaleText === null || scaleText === '' ? Number.NaN : Number(scaleText);
  const scale = Number.isFinite(scaleNumber)
    ? Math.min(WALL_SCALE_MAX, Math.max(WALL_SCALE_MIN, scaleNumber))
    : 1;
  const session = refPair(q.get('session'));
  const artifactRef = refPair(q.get('artifact'));
  const versionNumber = Number(q.get('version'));
  const artifact =
    artifactRef === undefined
      ? undefined
      : Number.isInteger(versionNumber) && versionNumber >= 1
        ? { ...artifactRef, version: versionNumber }
        : artifactRef;
  const batchText = q.get('batch');
  const batch = batchText === null || batchText === '' ? undefined : batchText;
  const wanted = WALL_MODES.find((m) => m === mode) ?? 'mission';
  const mode2 =
    ((wanted === 'focus' || wanted === 'review') && session === undefined) ||
    (wanted === 'race' && batch === undefined) ||
    (wanted === 'artifact' && artifact === undefined)
      ? 'mission'
      : wanted;
  return {
    name: 'wall',
    mode: mode2,
    stations: (q.get('stations') ?? '').split(',').filter((v) => v !== ''),
    repo: q.get('repo') ?? '',
    scale,
    shape: WALL_SHAPES.find((v) => v === shape) ?? 'auto',
    ...(session === undefined ? {} : { session }),
    ...(batch === undefined ? {} : { batch }),
    ...(artifact === undefined ? {} : { artifact }),
    pin: q.get('pin') === '1',
  };
}

function formatWall(route: WallRoute): string {
  const q = new URLSearchParams();
  q.set('mode', route.mode);
  if (route.stations.length > 0) q.set('stations', route.stations.join(','));
  if (route.repo !== '') q.set('repo', route.repo);
  q.set('scale', String(route.scale));
  q.set('shape', route.shape);
  if (route.session !== undefined) q.set('session', `${route.session.station}/${route.session.id}`);
  if (route.batch !== undefined) q.set('batch', route.batch);
  if (route.artifact !== undefined) {
    q.set('artifact', `${route.artifact.station}/${route.artifact.id}`);
    if (route.artifact.version !== undefined) q.set('version', String(route.artifact.version));
  }
  if (route.pin) q.set('pin', '1');
  return `#/wall?${q.toString()}`;
}

/** Hash routes (DL-140): the browser's back button works on a phone. Anything unknown is home. */
export function parseHash(hash: string): Route {
  const text = hash.startsWith('#') ? hash.slice(1) : hash;
  if (text === '' || text === '/') return HOME;
  const cut = text.indexOf('?');
  const path = cut < 0 ? text : text.slice(0, cut);
  const query = cut < 0 ? '' : text.slice(cut + 1);
  for (const name of PLAIN) {
    if (text === `/${name}`) return { name };
  }
  const parts = path.split('/');
  if (path === '/wall') return parseWall(query);
  if (parts[0] === '' && parts[1] === 'settings') return parseSettings(parts, query);
  // R1.4: `#/compare/<batch id>`.
  if (parts.length === 3 && parts[0] === '' && parts[1] === 'compare') {
    try {
      const batch = decodeURIComponent(parts[2] ?? '');
      return batch === '' ? HOME : { name: 'compare', batch };
    } catch {
      return HOME;
    }
  }
  // B17: `#/remote/<display id>`.
  if (parts.length === 3 && parts[0] === '' && parts[1] === 'remote') {
    try {
      const id = decodeURIComponent(parts[2] ?? '');
      return id === '' ? HOME : { name: 'remote', id };
    } catch {
      return HOME;
    }
  }
  // R3: `#/project/<id>`.
  if (parts.length === 3 && parts[0] === '' && parts[1] === 'project') {
    try {
      const id = decodeURIComponent(parts[2] ?? '');
      return id === '' ? HOME : { name: 'project', id };
    } catch {
      return HOME;
    }
  }
  if (parts[0] === '' && parts[1] === 'artifact' && (parts.length === 4 || parts.length === 5)) {
    return parseArtifact(parts);
  }
  if (parts.length !== 4 || parts[0] !== '' || parts[1] !== 's') return HOME;
  try {
    const station = decodeURIComponent(parts[2] ?? '');
    const session = decodeURIComponent(parts[3] ?? '');
    if (station === '' || session === '') return HOME;
    const q = new URLSearchParams(query);
    const tab = q.get('tab');
    const chosen: Tab =
      tab === 'changes' || tab === 'files' || tab === 'artifacts' || tab === 'people'
        ? tab
        : 'chat';
    const agent = q.get('agent');
    return {
      name: 'session',
      station,
      session,
      tab: chosen,
      ...(chosen === 'chat' && agent !== null && agent !== '' ? { agent } : {}),
    };
  } catch {
    return HOME;
  }
}

function formatSettings(route: Extract<Route, { name: 'settings' }>): string {
  if (route.tab === 'deck') return '#/settings';
  if (route.tab === 'displays') return '#/settings/displays';
  if (route.tab === 'station') return `#/settings/station/${encodeURIComponent(route.station)}`;
  const q = new URLSearchParams();
  if (route.station !== null) q.set('station', route.station);
  if (route.finish !== null) q.set('finish', route.finish);
  if (route.oauthError !== null) q.set('oauth_error', route.oauthError);
  const text = q.toString();
  return text === '' ? '#/settings/accounts' : `#/settings/accounts?${text}`;
}

/** The inverse of `parseHash`. */
export function formatHash(route: Route): string {
  switch (route.name) {
    case 'home':
      return '#/';
    case 'session': {
      const base = `#/s/${encodeURIComponent(route.station)}/${encodeURIComponent(route.session)}`;
      if (route.tab !== 'chat') return `${base}?tab=${route.tab}`;
      return route.agent === undefined || route.agent === ''
        ? base
        : `${base}?tab=chat&agent=${encodeURIComponent(route.agent)}`;
    }
    case 'wall':
      return formatWall(route);
    case 'compare':
      return `#/compare/${encodeURIComponent(route.batch)}`;
    case 'project':
      return `#/project/${encodeURIComponent(route.id)}`;
    case 'remote':
      return `#/remote/${encodeURIComponent(route.id)}`;
    case 'settings':
      return formatSettings(route);
    case 'artifact': {
      const base = `#/artifact/${encodeURIComponent(route.station)}/${encodeURIComponent(route.id)}`;
      return route.version === null ? base : `${base}/${route.version}`;
    }
    default:
      return `#/${route.name}`;
  }
}

export type Navigate = (route: Route, opts?: { readonly replace?: boolean }) => void;

/** The route from `window.location.hash`; `initial` (or home) when there is no window. */
export function useRoute(initial?: Route): [Route, Navigate] {
  const [route, setRoute] = useState<Route>(() =>
    typeof window === 'undefined' ? (initial ?? HOME) : parseHash(window.location.hash),
  );
  useEffect(() => {
    const sync = () => setRoute(parseHash(window.location.hash));
    window.addEventListener('hashchange', sync);
    return () => window.removeEventListener('hashchange', sync);
  }, []);
  const navigate = useCallback<Navigate>((next, opts) => {
    if (typeof window === 'undefined') return;
    if (opts?.replace === true) {
      window.history.replaceState(window.history.state, '', formatHash(next));
      setRoute(next);
    } else {
      window.location.hash = formatHash(next);
    }
  }, []);
  return [route, navigate];
}
