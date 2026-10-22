// DL-173: the Accounts tab of the settings: GitHub and GitLab by the CLI device login, Jira and
// Linear by OAuth with an app of the owner, and the token fallback. The station holds every
// credential; the board shows metadata and sends a secret once.
import { useCallback, useEffect, useRef, useState } from 'react';
import type { AccountKind, AccountsList, AccountView, StationView } from '../gen';
import type { Navigate } from '../app/routes';
import { useLoad } from '../integrations/useLoad';
import { Section } from '../ui/page';
import type { AccountActions } from './actions';
import { AppDialog, TokenDialog } from './AccountDialogs';
import { AccountTable } from './AccountTable';
import {
  claimFinish,
  connectedText,
  finishOauth,
  redirectUrlOf,
  oauthErrorText,
  sendApp,
  sendToken,
} from './flow';
import { ForgeConnect, forgeAccounts } from './ForgeSection';
import { TrackerBlock, type TrackerKind } from './TrackerSection';
import { Field } from '../ui/page';
import type { SettingsRoute } from '../settings/SettingsPage';
import { describe } from '../settings/http';
import { NoticeIcon } from '../settings/NoticeIcon';

type Banner = { readonly tone: 'info' | 'warn' | 'error'; readonly text: string };

type Props = {
  readonly route: Extract<SettingsRoute, { tab: 'accounts' }>;
  readonly stations: readonly StationView[];
  readonly navigate: Navigate;
  readonly actions: AccountActions;
  /** The location of the board, for the callback URL of an OAuth app. */
  readonly origin?: string;
  /** The account list at the first render (tests). */
  readonly initial?: AccountsList;
};

const TRACKERS: readonly TrackerKind[] = ['jira', 'linear'];

/** The warning for token variables in the station environment. */
export const envTokenText = (names: readonly string[]): string =>
  `${names.join(', ')} in the station environment override a login. Remove them from the station’s environment to use the login here.`;

type Dialog =
  | { readonly kind: 'token'; readonly account: AccountKind; readonly site: string }
  | { readonly kind: 'app'; readonly account: TrackerKind };

export function AccountsTab({ route, stations, navigate, actions, origin, initial }: Props) {
  const station = route.station ?? stations[0]?.station_id ?? null;
  const [banner, setBanner] = useState<Banner | null>(
    route.oauthError === null ? null : { tone: 'error', text: oauthErrorText(route.oauthError) },
  );
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [dialogBusy, setDialogBusy] = useState(false);
  const [dialogError, setDialogError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [waiting, setWaiting] = useState<{ kind: TrackerKind; url: string } | null>(null);
  const load = useCallback((id: string) => actions.list(id), [actions]);
  const { state, reload } = useLoad<AccountsList>(load, station ?? '', initial);
  const here = useRef(route);
  here.current = route;

  // `?finish=<transaction>`: the provider came back to the board. Post it once, show the answer,
  // and take the query out of the address.
  const finish = route.finish;
  useEffect(() => {
    if (finish === null || !claimFinish(finish)) return;
    const ids = stations.map((s) => s.station_id);
    void finishOauth(actions, finish).then(async (out) => {
      if (out.ok) {
        setBanner({ tone: 'info', text: connectedText(out.account) });
        // The callback does not say which station started the login: find the one that holds it.
        const found = await findStation(actions, ids, out.account.account_id);
        navigate(
          {
            name: 'settings',
            tab: 'accounts',
            station: found ?? here.current.station,
            finish: null,
            oauthError: null,
          },
          { replace: true },
        );
        reload();
      } else {
        setBanner({ tone: 'error', text: out.text });
        navigate(
          {
            name: 'settings',
            tab: 'accounts',
            station: here.current.station,
            finish: null,
            oauthError: null,
          },
          { replace: true },
        );
      }
    });
  }, [finish, actions, stations, navigate, reload]);
  // `?oauth_error=<code>` is shown once and taken out of the address.
  const oauthError = route.oauthError;
  useEffect(() => {
    if (oauthError === null) return;
    navigate(
      {
        name: 'settings',
        tab: 'accounts',
        station: here.current.station,
        finish: null,
        oauthError: null,
      },
      { replace: true },
    );
  }, [oauthError, navigate]);

  if (station === null) {
    return <p className="fd-note">Add a station first. Accounts belong to a station.</p>;
  }
  const list = state.kind === 'ok' ? state.value : null;
  const appOf = (kind: TrackerKind) => list?.apps.find((a) => a.kind === kind);
  const boardOrigin = origin ?? (typeof window === 'undefined' ? '' : window.location.origin);

  const logout = async (a: AccountView) => {
    setBusy(a.account_id);
    setBanner(null);
    try {
      await actions.logout(station, a.account_id);
      reload();
    } catch (e) {
      setBanner({ tone: 'error', text: describe(e) });
    } finally {
      setBusy(null);
    }
  };
  const connect = async (kind: TrackerKind) => {
    setBanner(null);
    try {
      const { authorize_url } = await actions.oauthStart(station, kind);
      setWaiting({ kind, url: authorize_url });
      window.open(authorize_url, '_blank', 'noopener');
    } catch (e) {
      setBanner({ tone: 'error', text: describe(e) });
    }
  };
  const reconnect = (a: AccountView) => {
    if (a.kind === 'jira' || a.kind === 'linear') {
      if (a.method === 'token') setDialog({ kind: 'token', account: a.kind, site: a.site ?? '' });
      else void connect(a.kind);
    }
  };
  const closeDialog = () => {
    setDialog(null);
    setDialogBusy(false);
    setDialogError(null);
  };

  return (
    <div className="fd-set-tab fd-acc">
      {stations.length > 1 ? (
        <div className="fd-set-stationpick">
          <Field label="Station">
            <select
              value={station}
              onChange={(e) =>
                navigate({
                  name: 'settings',
                  tab: 'accounts',
                  station: e.target.value,
                  finish: null,
                  oauthError: null,
                })
              }
            >
              {stations.map((s) => (
                <option key={s.station_id} value={s.station_id}>
                  {s.name}
                </option>
              ))}
            </select>
          </Field>
        </div>
      ) : null}
      {banner === null ? null : (
        <div
          className="fd-set-notice"
          data-tone={banner.tone}
          role={banner.tone === 'error' ? 'alert' : 'status'}
        >
          <NoticeIcon tone={banner.tone} />
          <span>{banner.text}</span>
          <button type="button" className="fd-ghost fd-sm" onClick={() => setBanner(null)}>
            Dismiss
          </button>
        </div>
      )}
      {state.kind === 'loading' ? <p role="status">Loading accounts…</p> : null}
      {state.kind === 'error' ? (
        <div className="fd-set-notice" data-tone="error" role="alert">
          <NoticeIcon tone="error" />
          <span>{state.text}</span>
          <button type="button" className="fd-sm" onClick={reload}>
            Try again
          </button>
        </div>
      ) : null}
      {list === null ? null : (
        <>
          {list.env_tokens.length === 0 ? null : (
            <div className="fd-set-notice" data-tone="warn" role="status">
              <NoticeIcon tone="warn" />
              <span>{envTokenText(list.env_tokens)}</span>
            </div>
          )}
          <Section label="GitHub and GitLab">
            <div className="fd-set-forges">
              <AccountTable
                label="GitHub and GitLab accounts"
                accounts={forgeAccounts(list.accounts)}
                busy={busy}
                onLogout={(a) => void logout(a)}
              />
              <div className="fd-set-connectrow">
                {(['github', 'gitlab'] as const).map((kind) => (
                  <ForgeConnect
                    key={kind}
                    kind={kind}
                    station={station}
                    actions={actions}
                    onConnected={reload}
                    onToken={(account, host) => setDialog({ kind: 'token', account, site: host })}
                  />
                ))}
              </div>
            </div>
          </Section>
          <Section label="Jira and Linear">
            <div className="fd-set-trackerlist">
              {TRACKERS.map((kind) => (
                <TrackerBlock
                  key={kind}
                  kind={kind}
                  app={appOf(kind)}
                  accounts={list.accounts.filter((a) => a.kind === kind)}
                  linearBuiltin={list.linear_builtin === true}
                  origin={boardOrigin}
                  busy={busy}
                  waiting={waiting?.kind === kind}
                  authorizeUrl={waiting?.kind === kind ? waiting.url : null}
                  onSetup={() => setDialog({ kind: 'app', account: kind })}
                  onConnect={() => void connect(kind)}
                  onToken={() => setDialog({ kind: 'token', account: kind, site: '' })}
                  onReconnect={reconnect}
                  onLogout={(a) => void logout(a)}
                />
              ))}
            </div>
          </Section>
        </>
      )}
      {dialog?.kind === 'token' ? (
        <TokenDialog
          kind={dialog.account}
          initialSite={dialog.site}
          busy={dialogBusy}
          error={dialogError}
          onCancel={closeDialog}
          onSubmit={(form, secret) => {
            setDialogBusy(true);
            setDialogError(null);
            void sendToken(
              actions,
              station,
              { kind: dialog.account, site: form.site, email: form.email },
              secret,
            ).then((out) => {
              if (out.ok) {
                closeDialog();
                setBanner({ tone: 'info', text: connectedText(out.value) });
                reload();
              } else {
                setDialogBusy(false);
                setDialogError(out.text);
              }
            });
          }}
        />
      ) : null}
      {dialog?.kind === 'app' ? (
        <AppDialog
          kind={dialog.account}
          redirectUrl={redirectUrlOf(boardOrigin)}
          initialClientId={appOf(dialog.account)?.client_id ?? ''}
          busy={dialogBusy}
          error={dialogError}
          onCancel={closeDialog}
          onSubmit={(clientId, secret) => {
            setDialogBusy(true);
            setDialogError(null);
            void sendApp(actions, station, dialog.account, clientId, secret).then((out) => {
              if (out.ok) {
                closeDialog();
                reload();
              } else {
                setDialogBusy(false);
                setDialogError(out.text);
              }
            });
          }}
        />
      ) : null}
    </div>
  );
}

/** The station that holds an account: the callback does not say which station started the login. */
async function findStation(
  actions: AccountActions,
  ids: readonly string[],
  accountId: string,
): Promise<string | null> {
  for (const id of ids) {
    try {
      const list = await actions.list(id);
      if (list.accounts.some((a) => a.account_id === accountId)) return id;
    } catch {
      // A station that does not answer is not the one.
    }
  }
  return null;
}
