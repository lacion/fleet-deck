// DL-173: Jira and Linear. The owner registers an OAuth app once, then connects with it. An API
// token is the fallback.
import type { AccountView, AppView } from '../gen';
import { AccountTable } from './AccountTable';
import { KIND_LABEL } from './flow';

export type TrackerKind = 'jira' | 'linear';

/** DL-185: the built-in Linear app works from these two board origins only. */
const BUILTIN_ORIGINS: readonly string[] = ['https://127.0.0.1:7420', 'https://localhost:7420'];

/** The text of the station code `builtin_needs_localhost`. */
export const BUILTIN_LOCALHOST_TEXT =
  'The built-in Linear app works only from the board on the deck machine at https://127.0.0.1:7420 or https://localhost:7420. Open the board there, or set up your own Linear app.';

export function AppStatus({
  app,
  builtin = false,
}: {
  readonly app: AppView | undefined;
  /** The tracker has a built-in app (Linear) that is used when the owner set none. */
  readonly builtin?: boolean;
}) {
  if (app === undefined && builtin) {
    return <p className="fd-note">Uses the built-in Fleet Deck app.</p>;
  }
  return app === undefined ? (
    <p className="fd-note">No OAuth app yet.</p>
  ) : (
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
  // Linear without an owner app uses the built-in app when the station offers
  // it. Jira has none.
  const builtin = kind === 'linear' && app === undefined && linearBuiltin;
  return (
    <div className="fd-set-tracker" role="group" aria-label={label}>
      <h3 className="fd-set-trackerhead">{label}</h3>
      <AppStatus app={app} builtin={builtin} />
      <div className="fd-set-trackeractions">
        <button type="button" onClick={onSetup}>
          {builtin ? 'Set up your own OAuth app…' : 'Set up the OAuth app…'}
        </button>
        <button
          type="button"
          className="fd-primary"
          disabled={app === undefined && !builtin}
          onClick={onConnect}
        >
          Connect {label}
        </button>
        <button type="button" className="fd-ghost" onClick={onToken}>
          Use an API token instead
        </button>
      </div>
      {app === undefined && !builtin ? (
        <p className="fd-note">Set up the OAuth app first. Then you can connect.</p>
      ) : null}
      {builtin && !BUILTIN_ORIGINS.includes(origin) ? (
        <p className="fd-note">{BUILTIN_LOCALHOST_TEXT}</p>
      ) : null}
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
      <AccountTable
        label={`${label} accounts`}
        accounts={accounts}
        busy={busy}
        onReconnect={onReconnect}
        onLogout={onLogout}
      />
    </div>
  );
}
