// R13 P2c: the runtime of the wall display document (`/display`). One `Connection` (with the
// display header on every request) owns the login, the socket and the timelines. The page shows:
// the pair page without a login, and the wall with a display login. It has no control; a
// directive from the deck moves the wall (`useDirective`), and the control frames of the deck scroll
// it and select an item (B17).
import { useEffect, useMemo, useSyncExternalStore } from 'react';
import { type Navigate, useRoute, wallDefaults } from '../app/routes';
import type { SessionKey } from '../gen';
import type { Connection } from '../runtime/connection';
import { viewsActions } from '../runtime/views';
import type { DisplayEnv } from '../wall/displayChanges';
import { WallPage } from '../wall/WallPage';
import { expectedLaunch, withDisplayHeader } from './api';
import { PairPage } from './PairPage';
import { useDirective } from './useDirective';
import { usePalette } from './usePalette';

/** The actor of a display login is `display:<name>`. */
export const isDisplayActor = (actor: string): boolean => actor.startsWith('display:');

const refused = async (): Promise<never> => {
  throw new Error('A wall display can only read.');
};

function Wall({ connection }: { readonly connection: Connection }) {
  const state = useSyncExternalStore(
    connection.subscribe,
    connection.getSnapshot,
    connection.getSnapshot,
  );
  const [route, navigate] = useRoute();
  // The wall is the only page of a display: any other hash goes to it.
  useEffect(() => {
    if (route.name !== 'wall') (navigate as Navigate)(wallDefaults, { replace: true });
  }, [route.name, navigate]);
  const get = useMemo(() => (path: string) => connection.get(path), [connection]);
  const { generation, reread } = useDirective(get, route, navigate, () => {
    // The login ended or the display was revoked: ask again who this is (the pair page shows).
    connection.stop();
    connection.start();
  });
  // R13 P4: the palette of the bound station; a 401 ends the login the same way.
  usePalette(get, () => {
    connection.stop();
    connection.start();
  });
  const env = useMemo<DisplayEnv>(
    () => ({ get, reread, ...(generation === null ? {} : { generation }) }),
    [get, reread, generation],
  );
  // B17: the frames of the deck. `WallPage` keeps the control state and calls `reread` (the
  // `navigate` frame, or a frame for a newer generation) through the env.
  const controls = useMemo(() => connection.onControl, [connection]);
  const views = useMemo(() => viewsActions(get, refused), [get]);
  const watch = useMemo(() => (key: SessionKey) => connection.watch(key), [connection]);
  if (route.name !== 'wall') return null;
  return (
    <WallPage
      route={route}
      navigate={navigate}
      owner={false}
      display={env}
      sessions={state.sessions}
      stations={state.stations}
      freshness={state.freshness}
      timelines={state.timelines}
      watch={watch}
      views={views}
      controls={controls}
    />
  );
}

export function DisplayRuntime({ connection }: { readonly connection: Connection }) {
  const state = useSyncExternalStore(
    connection.subscribe,
    connection.getSnapshot,
    connection.getSnapshot,
  );
  // The launch values are read once, before the hash changes.
  const launch = useMemo(() => {
    if (typeof window === 'undefined') return null;
    const hash = window.location.hash;
    return hash.startsWith('#/display-pair?')
      ? expectedLaunch(hash.slice(hash.indexOf('?') + 1))
      : null;
  }, []);
  if (state.login === null) {
    if (state.auth === 'checking') {
      return (
        <main className="fd-wall-root fd-pair" data-mode="pair">
          <p className="fd-pair-line" role="status">
            Connecting to the deck…
          </p>
        </main>
      );
    }
    return (
      <PairPage
        fetcher={withDisplayHeader((path, init) => fetch(path, init))}
        launch={launch}
        onClaimed={() => {
          window.history.replaceState(null, '', '/display#/wall');
          connection.stop();
          connection.start();
        }}
      />
    );
  }
  if (!isDisplayActor(state.login.actor)) {
    return (
      <main className="fd-wall-root" data-mode="denied">
        <p className="fd-wall-denied">This page is for a wall display. Open the board at /.</p>
      </main>
    );
  }
  return <Wall connection={connection} />;
}
