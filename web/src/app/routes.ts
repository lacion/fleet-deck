import { useCallback, useEffect, useState } from 'react';

export type Tab = 'chat' | 'changes' | 'files' | 'artifacts';

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
      tab: tab === 'changes' || tab === 'files' || tab === 'artifacts' ? tab : 'chat',
    };
  } catch {
    return HOME;
  }
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
