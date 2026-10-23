// DL-173: Jira and Linear. The owner registers an OAuth app once, then connects with it. An API
// token is the fallback. The section has the structure of GitHub and GitLab: one table with a row
// for each account and a "Not connected" row for a provider without one (it says what the account
// gives, or which app it connects with, and has the buttons to connect), then one row of "Connect
// another … account…" folds.
import { useState } from 'react';
import type { AccountView, AppView } from '../gen';
import { Icon, Plus } from '../ui/icons';
import { type AbsentRow, AccountTable } from './AccountTable';
import { KIND_LABEL } from './flow';

export type TrackerKind = 'jira' | 'linear';

const TRACKERS: readonly TrackerKind[] = ['jira', 'linear'];

/** DL-185: the built-in Linear app works from these two board origins only. */
const BUILTIN_ORIGINS: readonly string[] = ['https://127.0.0.1:7420', 'https://localhost:7420'];

/** The text of the station code `builtin_needs_localhost`. */
export const BUILTIN_LOCALHOST_TEXT =
  'The built-in Linear app works only from the board on the deck machine at https://127.0.0.1:7420 or https://localhost:7420. Open the board there, or set up your own Linear app.';

/**
 * The line of a provider without an account and without an app: what the account gives. The two
 * ways to connect are the buttons of the row; the line does not repeat them.
 */
export const noAppText = (label: string): string =>
  `Connect ${label} to start sessions from its issues.`;
/** The words of the built-in Linear app. */
export const BUILTIN_TEXT = 'Uses the built-in Fleet Deck app.';
/** The button that opens the app dialog; with the built-in app, "your own". */
const SETUP_TEXT = 'Set up OAuth app…';
const SETUP_OWN_TEXT = 'Set up your own OAuth app…';
const TOKEN_TEXT = 'Use an API token…';

/** The OAuth app of the owner: its client id, and whether the secret is set. */
export function AppStatus({ app }: { readonly app: AppView }) {
  return (
    <span className="fd-acc-app">
      OAuth app <code>{app.client_id}</code>
      {app.has_secret ? (
        <span className="fd-badge" data-tone="ok">
          secret set
        </span>
      ) : null}
    </span>
  );
}

/** What one tracker offers: its app, the built-in app (Linear), and so whether Connect works. */
type Ways = {
  readonly kind: TrackerKind;
  readonly label: string;
  readonly app: AppView | undefined;
  readonly builtin: boolean;
};

type Handlers = {
  readonly onSetup: (kind: TrackerKind) => void;
  readonly onConnect: (kind: TrackerKind) => void;
  readonly onToken: (kind: TrackerKind) => void;
};

/**
 * The note of a "Not connected" row: what the account gives, or, with an app, which app it connects
 * with and the setup of the owner's own app.
 */
function absentNote(w: Ways, on: Handlers) {
  if (w.app === undefined && !w.builtin) return noAppText(w.label);
  return (
    <span className="fd-acc-ways">
      {w.app === undefined ? BUILTIN_TEXT : <AppStatus app={w.app} />}
      <button type="button" className="fd-ghost fd-sm" onClick={() => on.onSetup(w.kind)}>
        {w.builtin ? SETUP_OWN_TEXT : SETUP_TEXT}
      </button>
    </span>
  );
}

/**
 * The buttons of a "Not connected" row: Connect (with an app) or the app setup, then the token. They
 * are two ways to do one step: two plain buttons, one look.
 */
function absentActions(w: Ways, on: Handlers) {
  const canConnect = w.app !== undefined || w.builtin;
  return (
    <>
      {canConnect ? (
        <button type="button" className="fd-sm" onClick={() => on.onConnect(w.kind)}>
          Connect {w.label}
        </button>
      ) : (
        <button type="button" className="fd-sm" onClick={() => on.onSetup(w.kind)}>
          {SETUP_TEXT}
        </button>
      )}
      <button type="button" className="fd-sm" onClick={() => on.onToken(w.kind)}>
        {TOKEN_TEXT}
      </button>
    </>
  );
}

/** "Connect another … account…" of a tracker with an account, and the ways when it is open. */
function ConnectAnother({ w, on }: { readonly w: Ways; readonly on: Handlers }) {
  const [open, setOpen] = useState(false);
  const canConnect = w.app !== undefined || w.builtin;
  return (
    <div className="fd-set-connect" role="group" aria-label={`Connect ${w.label}`}>
      <button type="button" className="fd-sm" aria-expanded={open} onClick={() => setOpen(!open)}>
        <Icon of={Plus} size={14} />
        Connect another {w.label} account…
      </button>
      {open ? (
        <>
          {w.app === undefined ? (
            w.builtin ? (
              <p className="fd-note">{BUILTIN_TEXT}</p>
            ) : null
          ) : (
            <p className="fd-note">
              <AppStatus app={w.app} />
            </p>
          )}
          <div className="fd-set-trackeractions">
            {canConnect ? (
              <button type="button" className="fd-sm" onClick={() => on.onConnect(w.kind)}>
                Connect {w.label}
              </button>
            ) : null}
            <button type="button" className="fd-sm" onClick={() => on.onSetup(w.kind)}>
              {w.builtin ? SETUP_OWN_TEXT : SETUP_TEXT}
            </button>
            <button type="button" className="fd-ghost fd-sm" onClick={() => on.onToken(w.kind)}>
              {TOKEN_TEXT}
            </button>
          </div>
        </>
      ) : null}
    </div>
  );
}

export function TrackerBlock({
  apps,
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
}: Handlers & {
  readonly apps: readonly AppView[];
  /** The Jira and Linear accounts of the station. */
  readonly accounts: readonly AccountView[];
  /** The station offers the built-in Linear app (`accounts.list` `linear_builtin`). */
  readonly linearBuiltin: boolean;
  /** The location of the board (`window.location.origin`). */
  readonly origin: string;
  readonly busy: string | null;
  /** The tracker whose login runs in the other tab. */
  readonly waiting: TrackerKind | null;
  /** The address that was opened, for a browser that blocked the new tab. */
  readonly authorizeUrl: string | null;
  readonly onReconnect: (account: AccountView) => void;
  readonly onLogout: (account: AccountView) => void;
}) {
  const on: Handlers = { onSetup, onConnect, onToken };
  const ways = TRACKERS.map((kind): Ways => {
    const app = apps.find((a) => a.kind === kind);
    // Linear without an owner app uses the built-in app when the station offers it. Jira has none.
    return {
      kind,
      label: KIND_LABEL[kind],
      app,
      builtin: kind === 'linear' && app === undefined && linearBuiltin,
    };
  });
  const connected = (kind: TrackerKind) => accounts.some((a) => a.kind === kind);
  const absent: AbsentRow[] = ways
    .filter((w) => !connected(w.kind))
    .map((w) => ({ kind: w.kind, note: absentNote(w, on), actions: absentActions(w, on) }));
  const more = ways.filter((w) => connected(w.kind));
  // The built-in app answers only on the deck machine: a board elsewhere says so once.
  const localOnly = ways.some((w) => w.builtin) && !BUILTIN_ORIGINS.includes(origin);
  return (
    <>
      <AccountTable
        label="Jira and Linear accounts"
        accounts={accounts}
        absent={absent}
        busy={busy}
        onReconnect={onReconnect}
        onLogout={onLogout}
      />
      {localOnly ? <p className="fd-note">{BUILTIN_LOCALHOST_TEXT}</p> : null}
      {waiting === null ? null : (
        <p className="fd-note" role="status">
          Finish the {KIND_LABEL[waiting]} login in the new tab.
          {authorizeUrl === null ? null : (
            <>
              {' '}
              <a href={authorizeUrl} target="_blank" rel="noopener noreferrer">
                Open it again
              </a>
            </>
          )}
        </p>
      )}
      {more.length === 0 ? null : (
        <div className="fd-set-connectrow">
          {more.map((w) => (
            <ConnectAnother key={w.kind} w={w} on={on} />
          ))}
        </div>
      )}
    </>
  );
}
