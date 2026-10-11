import { useState, useSyncExternalStore } from 'react';
import { App } from '../App';
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
      <App sessions={state.sessions.map((session) => session.summary)} />
    </>
  );
}
