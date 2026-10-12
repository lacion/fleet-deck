import { useMemo, useState, useSyncExternalStore } from 'react';
import { App } from '../App';
import { stationActions } from '../board/pairing';
import { conflictLoader } from '../board/conflicts';
import { planActions } from '../board/plans';
import type { SessionKey } from '../gen';
import { sessionActions } from '../session/commands';
import { shellActions } from '../shell/shell';
import { spawnActions } from '../spawn/spawn';
import { assignActions } from '../assign/assign';
import { fleetActions } from '../fleet/fleet';
import type { Connection } from './connection';

export function RuntimeApp({ connection }: { readonly connection: Connection }) {
  const state = useSyncExternalStore(
    connection.subscribe,
    connection.getSnapshot,
    connection.getSnapshot,
  );
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [logoutError, setLogoutError] = useState<string | null>(null);
  const canAdmin = state.login?.scopes.includes('admin') === true;
  const actions = useMemo(
    () => ({
      ...stationActions((path, body) => connection.post(path, body), canAdmin),
      shells: shellActions(
        (path) => connection.get(path),
        (path, body) => connection.post(path, body),
      ),
    }),
    [connection, canAdmin],
  );
  // A new login object with the same scopes keeps the same actions.
  const scopeText = (state.login?.scopes ?? []).join(' ');
  const commands = useMemo(() => {
    const scopes = scopeText.split(' ');
    return (key: SessionKey) =>
      sessionActions(
        (path, body) => connection.post(path, body),
        key,
        scopes,
        undefined,
        (path) => connection.get(path),
      );
  }, [connection, scopeText]);
  const spawn = useMemo(
    () =>
      spawnActions(
        (path) => connection.get(path),
        (path, body) => connection.post(path, body),
        scopeText.split(' '),
      ),
    [connection, scopeText],
  );
  const assign = useMemo(
    () =>
      scopeText.split(' ').includes('drive')
        ? assignActions((path, body) => connection.post(path, body))
        : undefined,
    [connection, scopeText],
  );
  const fleet = useMemo(
    () =>
      scopeText.split(' ').includes('drive')
        ? fleetActions(
            (path) => connection.get(path),
            (path, body) => connection.post(path, body),
          )
        : undefined,
    [connection, scopeText],
  );
  const plans = useMemo(
    () =>
      planActions(
        (path) => connection.get(path),
        (path, body) => connection.post(path, body),
        scopeText.split(' '),
      ),
    [connection, scopeText],
  );
  const conflicts = useMemo(() => conflictLoader((path) => connection.get(path)), [connection]);
  const watch = useMemo(() => (key: SessionKey) => connection.watch(key), [connection]);
  if (state.login === null) {
    return (
      <main className="fd-login">
        <h1>FLEET DECK</h1>
        <p>Log in to your deck. Login is required on loopback, LAN, and the tailnet.</p>
        {state.auth === 'checking' ? <p role="status">Connecting to the deck…</p> : null}
        <form
          onSubmit={(event) => {
            event.preventDefault();
            setBusy(true);
            const value = password;
            setPassword('');
            void connection.login(value).finally(() => setBusy(false));
          }}
        >
          <label htmlFor="deck-password">Owner password</label>
          <input
            id="deck-password"
            type="password"
            autoComplete="current-password"
            required
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
          <button type="submit" disabled={busy || state.auth === 'checking'}>
            {busy ? 'Logging in…' : 'Log in'}
          </button>
        </form>
        <p>
          Or run <code>fleetdeck open</code> on the deck machine for a one-time login link.
        </p>
        {state.error || logoutError ? <p role="alert">{state.error ?? logoutError}</p> : null}
      </main>
    );
  }
  const label =
    state.transport === 'connected'
      ? state.freshness === 'live'
        ? 'Connected · Live fleet'
        : 'Connected · Stale fleet'
      : 'Reconnecting · Stale fleet';
  return (
    <>
      <div className="fd-connection" data-freshness={state.freshness}>
        <span role="status">{label}</span>
        <button
          type="button"
          onClick={() => {
            setPassword('');
            void connection.logout().catch(() => {
              setLogoutError(
                'The deck did not confirm logout. Close the page or reconnect to try again.',
              );
            });
          }}
        >
          Log out
        </button>
      </div>
      {state.freshness === 'stale' ? (
        <p className="fd-stale" role="status">
          Cached data is read-only until the deck reconnects.
        </p>
      ) : null}
      {state.error ? (
        <p className="fd-stale" role="alert">
          {state.error}
        </p>
      ) : null}
      <App
        sessions={state.sessions}
        stations={state.stations}
        freshness={state.freshness}
        actions={actions}
        timelines={state.timelines}
        watch={watch}
        sessionActions={commands}
        spawn={spawn}
        assign={assign}
        fleet={fleet}
        feed={state.feed}
        plans={plans}
        conflicts={conflicts}
      />
    </>
  );
}
