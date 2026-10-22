// DL-173: Jira and Linear. The owner registers an OAuth app once, then connects with it. An API
// token is the fallback. A block shows its accounts first; without an account it says "Not
// connected." and shows the ways to connect; with one, the ways to connect fold under "Connect
// another account…".
import { useState } from 'react';
import type { AccountView, AppView } from '../gen';
import { AccountTable } from './AccountTable';
import { KIND_LABEL } from './flow';

export type TrackerKind = 'jira' | 'linear';

/** DL-185: the built-in Linear app works from these two board origins only. */
const BUILTIN_ORIGINS: readonly string[] = ['https://127.0.0.1:7420', 'https://localhost:7420'];

/** The text of the station code `builtin_needs_localhost`. */
export const BUILTIN_LOCALHOST_TEXT =
  'The built-in Linear app works only from the board on the deck machine at https://127.0.0.1:7420 or https://localhost:7420. Open the board there, or set up your own Linear app.';

/** The line of a block without an account and without an app. */
export const NO_APP_TEXT = 'Set up an OAuth app, or use an API token.';
/** The words of the built-in Linear app. */
export const BUILTIN_TEXT = 'Uses the built-in Fleet Deck app.';

/** The OAuth app of the owner: its client id, and whether the secret is set. */
export function AppStatus({ app }: { readonly app: AppView }) {
  return (
    <p className="fd-note">
      OAuth app: client id <code>{app.client_id}</code>
      {app.has_secret ? (
        <>
          {' '}
          <span className="fd-badge" data-tone="ok">
            secret set
          </span>
        </>
      ) : null}
    </p>
  );
}

/** The ways to connect: the app line, then [Set up…] [Connect] (with an app) [token]. */
function ConnectWays({
  label,
  app,
  builtin,
  origin,
  onSetup,
  onConnect,
  onToken,
}: {
  readonly label: string;
  readonly app: AppView | undefined;
  readonly builtin: boolean;
  readonly origin: string;
  readonly onSetup: () => void;
  readonly onConnect: () => void;
  readonly onToken: () => void;
}) {
  const canConnect = app !== undefined || builtin;
  return (
    <>
      {app === undefined ? null : <AppStatus app={app} />}
      <div className="fd-set-trackeractions">
        <button type="button" onClick={onSetup}>
          {builtin ? 'Set up your own OAuth app…' : 'Set up the OAuth app…'}
        </button>
        {canConnect ? (
          <button type="button" onClick={onConnect}>
            Connect {label}
          </button>
        ) : null}
        <button type="button" className="fd-ghost" onClick={onToken}>
          Use an API token instead
        </button>
      </div>
      {builtin && !BUILTIN_ORIGINS.includes(origin) ? (
        <p className="fd-note">{BUILTIN_LOCALHOST_TEXT}</p>
      ) : null}
    </>
  );
}

export function TrackerBlock({
  kind,
  app,
  accounts,
  linearBuiltin,
  origin,
  busy,
  waiting,
  authorizeUrl,
  onSetup,
  onConnect,
  onToken,
  onReconnect,
  onLogout,
}: {
  readonly kind: TrackerKind;
  readonly app: AppView | undefined;
  readonly accounts: readonly AccountView[];
  /** The station offers the built-in Linear app (`accounts.list` `linear_builtin`). */
  readonly linearBuiltin: boolean;
  /** The location of the board (`window.location.origin`). */
  readonly origin: string;
  readonly busy: string | null;
  /** The login runs in the other tab. */
  readonly waiting: boolean;
  /** The address that was opened, for a browser that blocked the new tab. */
  readonly authorizeUrl: string | null;
  readonly onSetup: () => void;
  readonly onConnect: () => void;
  readonly onToken: () => void;
  readonly onReconnect: (account: AccountView) => void;
  readonly onLogout: (account: AccountView) => void;
}) {
  const label = KIND_LABEL[kind];
  const [more, setMore] = useState(false);
  // Linear without an owner app uses the built-in app when the station offers
  // it. Jira has none.
  const builtin = kind === 'linear' && app === undefined && linearBuiltin;
  const ways = (
    <ConnectWays
      label={label}
      app={app}
      builtin={builtin}
      origin={origin}
      onSetup={onSetup}
      onConnect={onConnect}
      onToken={onToken}
    />
  );
  const connected = accounts.length > 0;
  return (
    <div className="fd-set-tracker" role="group" aria-label={label}>
      <h3 className="fd-set-trackerhead">{label}</h3>
      {connected ? (
        <>
          <AccountTable
            label={`${label} accounts`}
            accounts={accounts}
            busy={busy}
            onReconnect={onReconnect}
            onLogout={onLogout}
            emptyText={null}
          />
          <div>
            <button
              type="button"
              className="fd-ghost fd-sm"
              aria-expanded={more}
              onClick={() => setMore(!more)}
            >
              Connect another account…
            </button>
          </div>
          {more ? (
            <>
              {builtin ? <p className="fd-note">{BUILTIN_TEXT}</p> : null}
              {ways}
            </>
          ) : null}
        </>
      ) : (
        <>
          <p className="fd-note">
            {builtin
              ? `Not connected. ${BUILTIN_TEXT}`
              : app === undefined
                ? `Not connected. ${NO_APP_TEXT}`
                : 'Not connected.'}
          </p>
          {ways}
        </>
      )}
      {waiting ? (
        <p className="fd-note" role="status">
          Finish the login in the new tab.
          {authorizeUrl === null ? null : (
            <>
              {' '}
              <a href={authorizeUrl} target="_blank" rel="noopener noreferrer">
                Open it again
              </a>
            </>
          )}
        </p>
      ) : null}
    </div>
  );
}
