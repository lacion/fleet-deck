import { useCallback, useEffect, useState } from 'react';

export type Tab = 'chat' | 'changes' | 'files' | 'artifacts' | 'people';

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
  | { readonly name: 'artifacts' }
  | { readonly name: 'projects' }
  | { readonly name: 'project'; readonly id: string }
  | { readonly name: 'shares' }
  | { readonly name: 'spaces' }
  | { readonly name: 'settings'; readonly tab: 'deck' }
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
    const tab = new URLSearchParams(query).get('tab');
    return {
      name: 'session',
      station,
      session,
      tab:
        tab === 'changes' || tab === 'files' || tab === 'artifacts' || tab === 'people'
          ? tab
          : 'chat',
    };
  } catch {
    return HOME;
  }
}

function formatSettings(route: Extract<Route, { name: 'settings' }>): string {
  if (route.tab === 'deck') return '#/settings';
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
      return route.tab === 'chat' ? base : `${base}?tab=${route.tab}`;
    }
    case 'compare':
      return `#/compare/${encodeURIComponent(route.batch)}`;
    case 'project':
      return `#/project/${encodeURIComponent(route.id)}`;
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
      window.history.replaceState(null, '', formatHash(next));
      setRoute(next);
    } else {
      window.location.hash = formatHash(next);
    }
  }, []);
  return [route, navigate];
}
