import { useCallback, useEffect, useState } from 'react';

export type Tab = 'chat' | 'changes' | 'files';

export type Route =
  | { readonly name: 'home' }
  | { readonly name: 'fleet' }
  | { readonly name: 'new' }
  | { readonly name: 'plans' }
  | { readonly name: 'stations' }
  | { readonly name: 'files' }
  | { readonly name: 'watch' }
  | { readonly name: 'activity' }
  | { readonly name: 'terminals' }
  | { readonly name: 'more' }
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
  'stations',
  'files',
  'watch',
  'activity',
  'terminals',
  'more',
] as const;

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
      tab: tab === 'changes' || tab === 'files' ? tab : 'chat',
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
