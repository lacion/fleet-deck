import { useMemo, useState, useSyncExternalStore } from 'react';
import { App } from '../App';
import { stationActions, worktreeLoader } from '../board/pairing';
import { conflictLoader } from '../board/conflicts';
import { workspaceFiles } from '../session/workspace';
import { planActions } from '../board/plans';
import type { FleetExtras, SessionKey } from '../gen';
import { sessionActions } from '../session/commands';
import { shellActions } from '../shell/shell';
import { spawnActions } from '../spawn/spawn';
import { assignActions } from '../assign/assign';
import { fleetActions } from '../fleet/fleet';
import type { Chrome, Notice } from '../app/chrome';
import { createTerminalStore, type TermTab } from '../terminal/store';
import { handoverFlow } from '../session/handoverFlow';
import type { Outcome } from '../session/commands';
import type { Watch } from '../terminal/handover';
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
      // The deck's shell routes are owner-only: no admin scope, no terminals.
      shells: canAdmin
        ? shellActions(
            (path) => connection.get(path),
            (path, body) => connection.post(path, body),
          )
        : undefined,
      // The worktree inventory is owner-only as well.
      worktrees: canAdmin ? worktreeLoader((path) => connection.get(path, 65_000)) : undefined,
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
  const filesFor = useMemo(
    () => (station: string, workspace: string) =>
      workspaceFiles((path) => connection.get(path), station, workspace),
    [connection],
  );
  const extras = useMemo(
    () => async (): Promise<FleetExtras> => {
      const v = await connection.get('/api/fleet/extras');
      if (typeof v !== 'object' || v === null || !('sessions' in v) || !Array.isArray(v.sessions)) {
        throw new Error('The deck sent invalid fleet extras.');
      }
      return v as FleetExtras;
    },
    [connection],
  );
  const watch = useMemo(() => (key: SessionKey) => connection.watch(key), [connection]);
  // The terminals survive a reload through sessionStorage.
  const terminals = useMemo(() => {
    let storage: Storage | null = null;
    try {
      storage = window.sessionStorage;
    } catch {
      // blocked: the tabs live in memory only
    }
    return createTerminalStore(storage);
  }, []);
  const summaryWatch = useMemo(
    () =>
      (key: SessionKey): Watch => ({
        get: () =>
          connection
            .getSnapshot()
            .sessions.find(
              (s) => s.station_id === key.station_id && s.summary.session_id === key.session_id,
            )?.summary ?? null,
        subscribe: connection.subscribe,
      }),
    [connection],
  );
  const returnTerminal = useMemo(
    () =>
      async (tab: TermTab): Promise<Outcome> => {
        if (tab.session === null || actions.shells === undefined)
          return { ok: false, text: 'This terminal holds no session.' };
        return handoverFlow({
          key: tab.session,
          title: tab.title,
          watch: summaryWatch(tab.session),
          actions: commands(tab.session),
          shells: actions.shells,
          terminals,
        }).back();
      },
    [actions, commands, summaryWatch, terminals],
  );
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
  const notices: Notice[] = [
    ...(state.freshness === 'stale'
      ? [{ text: 'Cached data is read-only until the deck reconnects.', tone: 'info' as const }]
      : []),
    ...(state.error ? [{ text: state.error, tone: 'error' as const }] : []),
    ...(logoutError ? [{ text: logoutError, tone: 'error' as const }] : []),
  ];
  const chrome: Chrome = {
    label,
    freshness: state.freshness,
    onLogout: () => {
      setPassword('');
      void connection.logout().catch(() => {
        setLogoutError(
          'The deck did not confirm logout. Close the page or reconnect to try again.',
        );
      });
    },
    notices,
    version: state.login.deck_version,
  };
  return (
    <App
      sessions={state.sessions}
      stations={state.stations}
      freshness={state.freshness}
      actions={actions}
      timelines={state.timelines}
      watch={watch}
      sessionActions={commands}
      filesFor={filesFor}
      spawn={spawn}
      assign={assign}
      fleet={fleet}
      feed={state.feed}
      plans={plans}
      conflicts={conflicts}
      extras={extras}
      chrome={chrome}
      terminals={terminals}
      summaryWatch={summaryWatch}
      onReturnTerminal={returnTerminal}
    />
  );
}
