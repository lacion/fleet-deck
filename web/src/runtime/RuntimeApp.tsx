import { useEffect, useMemo, useState, useSyncExternalStore } from 'react';
import { App } from '../App';
import { artifactActions, artifactHttp } from '../artifacts/actions';
import { stationActions, worktreeActor, worktreeLoader } from '../board/pairing';
import { usageRecountCall } from '../board/usageRecountCall';
import { conflictLoader } from '../board/conflicts';
import { raceSummary } from '../compare/compare';
import { workspaceFiles } from '../session/workspace';
import { planActions } from '../board/plans';
import { automationActions } from '../automations/automations';
import { integrationActions } from '../integrations/actions';
import { integrationHttp } from '../integrations/http';
import { accountActions } from '../accounts/actions';
import { settingsActions } from '../settings/actions';
import { tailnetActions } from '../tailnet/api';
import { desktopActions, displayActions } from '../displays/actions';
import { deckHttp } from '../settings/http';
import { desktopToolsActions } from '../settings/desktopTools';
import { omarchyActions } from '../settings/omarchy';
import { captureActions } from '../desktop/capture';
import { deleteActions } from '../artifacts/DeleteArtifact';
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
import { archived } from '../ui/archive';
import { Mark } from '../ui/Mark';
import { clearAuthoringState } from './authoringState';
import {
  type Connection,
  SIGN_IN_BLOCKED_FOR_INPUT_TEXT,
  SIGNED_OUT_FOR_INPUT_TEXT,
} from './connection';
import { deviceLinkAction } from './deviceLink';
import { viewsActions } from './views';
import { ReviewProvider } from '../review/context';
import { reviewActions, reviewHttp } from '../review/api';
import { JoinPage } from '../sharing/JoinPage';
import { join, joinToken } from '../sharing/join';
import { type PeopleEnv, PeopleProvider } from '../sharing/context';
import { memberOf, writeStoredMember } from '../sharing/member';
import { useOwnerShares } from '../sharing/ownerShares';
import { peopleCalls, shareCalls } from '../sharing/people';
import { stationPalettePath } from '../ui/palette';

/** The deck waits up to 20 s for a station; the board waits a little longer. */
const ARTIFACT_WAIT_MS = 25_000;
/** The deck waits up to 75 s for the station's answer to an issue or pull request read (R3). */
const INTEGRATION_WAIT_MS = 80_000;
/** The deck waits up to 15 s for a station; the settings calls wait a little longer. */
const SETTINGS_WAIT_MS = 20_000;
// R13 P5a (design §7.1): the screenshot POST waits 45 s; the lookups and the deletions 15 s.
const CAPTURE_WAIT_MS = 45_000;
const LOOKUP_WAIT_MS = 15_000;
// Omarchy admin: the deck waits up to 75 s for an action (design §4); the board waits a little longer.
const OMARCHY_WAIT_MS = 90_000;

export function RuntimeApp({ connection }: { readonly connection: Connection }) {
  const state = useSyncExternalStore(
    connection.subscribe,
    connection.getSnapshot,
    connection.getSnapshot,
  );
  // R11: a member is a guest of the owner's deck; the board hides what the deck refuses.
  const member = useMemo(() => memberOf(state.login, sessionStorageOrNull()), [state.login]);
  const guest = member !== null;
  // DL-149: the archived sessions live on the deck; attach once logged in,
  // and again after a new login.
  const loggedIn = state.login !== null && !guest;
  useEffect(() => {
    if (!loggedIn) return undefined;
    // The detach of the archive is the cleanup.
    return archived.attach({
      get: (p) => connection.get(p),
      post: (p, b) => connection.post(p, b),
    });
  }, [connection, loggedIn]);
  const [password, setPassword] = useState('');
  const [joinHash, setJoinHash] = useState<string | null>(() =>
    typeof window === 'undefined' ? null : joinToken(window.location.hash),
  );
  const [busy, setBusy] = useState(false);
  const [logoutError, setLogoutError] = useState<string | null>(null);
  const canAdmin = state.login?.scopes.includes('admin') === true;
  const owner = state.login?.actor === 'owner';
  const sharing = useMemo(
    () =>
      owner && canAdmin
        ? shareCalls(
            (path) => connection.get(path),
            (path, body) => connection.post(path, body),
          )
        : undefined,
    [connection, owner, canAdmin],
  );
  const ownerShares = useOwnerShares(sharing);
  // R13 P4: the palette of a station is read with the owner's login only; the function is stable.
  const getPalette = useMemo(
    () =>
      owner && canAdmin && !guest
        ? (stationId: string) => connection.get(stationPalettePath(stationId))
        : undefined,
    [connection, owner, canAdmin, guest],
  );
  const calls = useMemo(
    () =>
      peopleCalls(
        (path) => connection.get(path),
        (path, body) => connection.post(path, body),
      ),
    [connection],
  );
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
      worktreeAction: canAdmin
        ? worktreeActor((path, body) => connection.post(path, body))
        : undefined,
      // The recount of old usage counts is the owner's login only (B20).
      usageRecount:
        owner && canAdmin && !guest
          ? usageRecountCall(
              (path) => connection.get(path),
              (path, body) => connection.post(path, body),
            )
          : undefined,
    }),
    [connection, canAdmin, owner, guest],
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
      guest
        ? undefined
        : spawnActions(
            // B7: the deck waits up to 15 s for a station's session list or preview.
            (path) => connection.get(path, path.includes('/native-sessions') ? 20_000 : undefined),
            // The deck waits up to 30 s for a station's Git access check (DL-144).
            (path, body) =>
              connection.post(
                path,
                body,
                path.endsWith('/workspace/preflight')
                  ? 35_000
                  : path.includes('/native-sessions')
                    ? 20_000
                    : undefined,
              ),
            scopeText.split(' '),
          ),
    [connection, scopeText, guest],
  );
  const assign = useMemo(
    () =>
      !guest && scopeText.split(' ').includes('drive')
        ? assignActions((path, body) => connection.post(path, body))
        : undefined,
    [connection, scopeText, guest],
  );
  const fleet = useMemo(
    () =>
      !guest && scopeText.split(' ').includes('drive')
        ? fleetActions(
            (path) => connection.get(path),
            (path, body, headers) =>
              headers === undefined
                ? connection.post(path, body)
                : deckHttp(
                    (p, init) => fetch(p, init),
                    () => connection.getSnapshot().login?.csrf ?? null,
                    SETTINGS_WAIT_MS,
                  ).post(path, body, headers),
          )
        : undefined,
    [connection, scopeText, guest],
  );
  const plans = useMemo(
    () =>
      guest
        ? undefined
        : planActions(
            (path) => connection.get(path),
            (path, body) => connection.post(path, body),
            scopeText.split(' '),
          ),
    [connection, scopeText, guest],
  );
  // The deck's automation routes are owner-only: no admin scope, no automations.
  const automations = useMemo(
    () =>
      canAdmin
        ? automationActions(
            (path) => connection.get(path),
            (path, body) => connection.post(path, body),
          )
        : undefined,
    [connection, canAdmin],
  );
  const artifacts = useMemo(
    () =>
      guest
        ? undefined
        : artifactActions(
            {
              get: (path) => connection.get(path, ARTIFACT_WAIT_MS),
              post: (path, body) => connection.post(path, body, ARTIFACT_WAIT_MS),
              ...artifactHttp(
                (path, init) => fetch(path, init),
                () => connection.getSnapshot().login?.csrf ?? null,
                ARTIFACT_WAIT_MS,
              ),
            },
            scopeText.split(' '),
          ),
    [connection, scopeText, guest],
  );
  // DL-179: review comments. The deck decides who may write, for every kind of login.
  const review = useMemo(
    () =>
      reviewActions({
        get: (path, timeoutMs) => connection.get(path, timeoutMs),
        post: (path, body) => connection.post(path, body),
        ...reviewHttp(
          (path, init) => fetch(path, init),
          () => connection.getSnapshot().login?.csrf ?? null,
          10_000,
        ),
      }),
    [connection],
  );
  const conflicts = useMemo(
    () => (guest ? undefined : conflictLoader((path) => connection.get(path))),
    [connection, guest],
  );
  const integrations = useMemo(
    () =>
      integrationActions(
        {
          get: (path) => connection.get(path, INTEGRATION_WAIT_MS),
          post: (path, body) => connection.post(path, body, INTEGRATION_WAIT_MS),
          ...integrationHttp(
            (path, init) => fetch(path, init),
            () => connection.getSnapshot().login?.csrf ?? null,
            INTEGRATION_WAIT_MS,
          ),
        },
        scopeText.split(' '),
      ),
    [connection, scopeText],
  );
  // The settings and accounts are the owner's login only; the elevation lives in these actions.
  const settings = useMemo(() => {
    if (!owner || !canAdmin || guest) return undefined;
    const io = deckHttp(
      (path, init) => fetch(path, init),
      () => connection.getSnapshot().login?.csrf ?? null,
      SETTINGS_WAIT_MS,
    );
    const csrf = () => connection.getSnapshot().login?.csrf ?? null;
    const captureIo = deckHttp((path, init) => fetch(path, init), csrf, CAPTURE_WAIT_MS);
    const lookupIo = deckHttp((path, init) => fetch(path, init), csrf, LOOKUP_WAIT_MS);
    return {
      settings: settingsActions(io),
      accounts: accountActions(io),
      tailnet: tailnetActions(io),
      displays: displayActions(io),
      desktop: desktopActions(io),
      // A recording POST waits `seconds + 45` s (R13 P5b §10): its own transport.
      capture: captureActions(captureIo, lookupIo, (ms) =>
        deckHttp((path, init) => fetch(path, init), csrf, ms),
      ),
      deletion: deleteActions(lookupIo),
      omarchy: omarchyActions(deckHttp((path, init) => fetch(path, init), csrf, OMARCHY_WAIT_MS)),
      desktopTools: desktopToolsActions(
        deckHttp((path, init) => fetch(path, init), csrf, OMARCHY_WAIT_MS),
      ),
    };
  }, [connection, owner, canAdmin, guest]);
  const filesFor = useMemo(
    () =>
      guest
        ? undefined
        : (station: string, workspace: string) =>
            workspaceFiles((path) => connection.get(path), station, workspace),
    [connection, guest],
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
  const views = useMemo(
    () =>
      guest
        ? undefined
        : viewsActions(
            (path) => connection.get(path),
            (path, body) => connection.post(path, body),
          ),
    [connection, guest],
  );
  const watch = useMemo(() => (key: SessionKey) => connection.watch(key), [connection]);
  // R10.5: a model call takes up to 60 s on the station.
  const race = useMemo(
    () => (keys: readonly SessionKey[]) =>
      raceSummary((path, body) => connection.post(path, body, 70_000), keys),
    [connection],
  );
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
  const self = state.login?.actor ?? 'owner';
  const people = useMemo<PeopleEnv>(
    () => ({
      self,
      member,
      shares: sharing,
      calls,
      shared: ownerShares.shared,
      mob: ownerShares.mob,
      presence: state.presence,
      chat: state.chat,
      driver: state.driver,
      driveRequests: state.driveRequests,
      heartbeat: connection.presence,
      leave: () => {
        writeStoredMember(sessionStorageOrNull(), null);
        void connection.logout().catch(() => {
          setLogoutError(
            'The deck did not confirm logout. Close the page or reconnect to try again.',
          );
        });
      },
    }),
    [
      self,
      member,
      sharing,
      calls,
      ownerShares,
      state.presence,
      state.chat,
      state.driver,
      state.driveRequests,
      connection,
    ],
  );
  if (state.login === null && joinHash !== null) {
    // R11.4: an invite link comes before the login page.
    return (
      <JoinPage
        onJoin={(name) => join((path, init) => fetch(path, init), joinHash, name)}
        onJoined={(result) => {
          writeStoredMember(sessionStorageOrNull(), result.joined);
          window.history.replaceState(
            null,
            '',
            `${window.location.pathname}${window.location.search}#/`,
          );
          setJoinHash(null);
          // NV-05: a join starts a new account.
          clearAuthoringState();
          // The cookie is set: the connection asks the deck who this is.
          connection.stop();
          connection.start();
        }}
      />
    );
  }
  if (state.login === null) {
    // R13 P7 §7.6: the sign-out lines of agent input show above the form.
    const inputLine =
      state.error === SIGNED_OUT_FOR_INPUT_TEXT || state.error === SIGN_IN_BLOCKED_FOR_INPUT_TEXT
        ? state.error
        : null;
    const belowLine = inputLine === null ? state.error || logoutError : logoutError;
    return (
      <main className="fd-login">
        <Mark size={32} />
        <h1>Fleet Deck</h1>
        <p className="fd-login-lead">
          Log in to your deck. Login is required on loopback, LAN, and the tailnet.
        </p>
        {inputLine === null ? null : <p role="alert">{inputLine}</p>}
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
          <button type="submit" className="fd-primary" disabled={busy || state.auth === 'checking'}>
            {busy ? 'Logging in…' : 'Log in'}
          </button>
        </form>
        <p className="fd-note">
          Or run <code>fleetdeck open</code> on the deck machine for a one-time login link.
        </p>
        {belowLine ? <p role="alert">{belowLine}</p> : null}
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
  const addDevice = deviceLinkAction((path, body) => connection.post(path, body));
  const chrome: Chrome = {
    label,
    freshness: state.freshness,
    onLogout: () => {
      setPassword('');
      writeStoredMember(sessionStorageOrNull(), null);
      void connection.logout().catch(() => {
        setLogoutError(
          'The deck did not confirm logout. Close the page or reconnect to try again.',
        );
      });
    },
    notices,
    version: state.login.deck_version,
    // DL-146: the deck answers 403 `owner_only` for a token login.
    addDevice: canAdmin ? addDevice : undefined,
    // R13 P4: the Omarchy palette of a station is for the owner's login only.
    getPalette,
  };
  return (
    <PeopleProvider value={people}>
      <ReviewProvider value={review}>
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
          artifacts={artifacts}
          integrations={integrations}
          plans={plans}
          automations={automations}
          conflicts={conflicts}
          extras={extras}
          chrome={chrome}
          terminals={terminals}
          summaryWatch={summaryWatch}
          onReturnTerminal={returnTerminal}
          race={race}
          views={views}
          settings={settings}
        />
      </ReviewProvider>
    </PeopleProvider>
  );
}

/** `sessionStorage`, or `null` when the browser blocks it. */
function sessionStorageOrNull(): Storage | null {
  try {
    return typeof window === 'undefined' ? null : window.sessionStorage;
  } catch {
    return null;
  }
}
