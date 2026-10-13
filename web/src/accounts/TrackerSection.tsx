// DL-173: Jira and Linear. The owner registers an OAuth app once, then connects with it. An API
// token is the fallback.
import type { AccountView, AppView } from '../gen';
import { AccountTable } from './AccountTable';
import { KIND_LABEL } from './flow';

export type TrackerKind = 'jira' | 'linear';

export function AppStatus({ app }: { readonly app: AppView | undefined }) {
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
  return (
    <div className="fd-set-tracker" role="group" aria-label={label}>
      <h3 className="fd-set-trackerhead">{label}</h3>
      <AppStatus app={app} />
      <div className="fd-set-trackeractions">
        <button type="button" onClick={onSetup}>
          Set up the OAuth app…
        </button>
        <button
          type="button"
          className="fd-primary"
          disabled={app === undefined}
          onClick={onConnect}
        >
          Connect {label}
        </button>
        <button type="button" className="fd-ghost" onClick={onToken}>
          Use an API token instead
        </button>
      </div>
      {app === undefined ? (
        <p className="fd-note">Set up the OAuth app first. Then you can connect.</p>
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
