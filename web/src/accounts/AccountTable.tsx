// DL-173: the connected accounts of one section: kind and method, site, user, status, and the
// actions. The columns have fixed widths, so that the tables of the tab line up (settings.css).
import type { AccountView } from '../gen';
import { KIND_LABEL } from './flow';

/** The status of an account: its glyph (`.fd-dot`) and its words (§3: a status is glyph + text). */
const STATUS: Readonly<Record<AccountView['status'], { text: string; dot: string }>> = {
  ok: { text: 'Connected', dot: 'online' },
  needs_login: { text: 'Needs login', dot: 'needs_you' },
  needs_reconnect: { text: 'Reconnect needed', dot: 'needs_you' },
  error: { text: 'Error', dot: 'lost' },
};

/** How the account signed in, in words. */
export const METHOD_LABEL: Readonly<Record<AccountView['method'], string>> = {
  cli: 'CLI login',
  oauth: 'OAuth',
  token: 'API token',
  config: 'Config file',
};

/** The site without its scheme: `https://linear.app/acme` → `linear.app/acme`. */
export const siteText = (site: string | null | undefined): string =>
  (site ?? '').replace(/^https?:\/\//i, '');

export function AccountTable({
  label,
  accounts,
  busy,
  onReconnect,
  onLogout,
  emptyText = 'No account connected.',
}: {
  readonly label: string;
  readonly accounts: readonly AccountView[];
  /** The account id whose call runs now. */
  readonly busy: string | null;
  /** Absent: the section has no "Reconnect" (a forge login starts again from "Connect"). */
  readonly onReconnect?: ((account: AccountView) => void) | undefined;
  readonly onLogout: (account: AccountView) => void;
  /** The line of an empty list; `null`: the section says it in its own words. */
  readonly emptyText?: string | null;
}) {
  if (accounts.length === 0) {
    return emptyText === null ? null : <p className="fd-note">{emptyText}</p>;
  }
  return (
    <div className="fd-tablewrap">
      <table className="fd-table fd-acc-table" aria-label={label}>
        <colgroup>
          <col className="fd-acc-col-account" />
          <col />
          <col className="fd-acc-col-user" />
          <col className="fd-acc-col-status" />
          <col className="fd-acc-col-actions" />
        </colgroup>
        <thead>
          <tr>
            <th scope="col">Account</th>
            <th scope="col">Site</th>
            <th scope="col">User</th>
            <th scope="col">Status</th>
            <th scope="col">
              <span className="fd-vh">Actions</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {accounts.map((a) => {
            const status = STATUS[a.status];
            const own = a.method !== 'config';
            return (
              <tr key={a.account_id} aria-label={`Account ${a.account_id}`}>
                <td className="fd-cell-name" data-label="Account">
                  <span className="fd-acc-name">
                    {KIND_LABEL[a.kind]} <span className="fd-badge">{METHOD_LABEL[a.method]}</span>
                  </span>
                </td>
                <td
                  className="fd-cell-path"
                  data-label="Site"
                  title={siteText(a.site) || undefined}
                >
                  {siteText(a.site)}
                </td>
                <td data-label="User" title={a.user ?? undefined}>
                  {a.user ?? ''}
                </td>
                <td data-label="Status">
                  <span className="fd-acc-status" title={a.message || undefined}>
                    <span className="fd-dot" data-status={status.dot} aria-hidden="true" />
                    {status.text}
                  </span>
                </td>
                <td className="fd-cell-actions">
                  {own && a.status === 'needs_login' && onReconnect !== undefined ? (
                    <button
                      type="button"
                      className="fd-sm"
                      disabled={busy === a.account_id}
                      onClick={() => onReconnect(a)}
                    >
                      Reconnect
                    </button>
                  ) : null}
                  {own && a.status === 'needs_reconnect' && onReconnect !== undefined ? (
                    <button
                      type="button"
                      className="fd-sm"
                      disabled={busy === a.account_id}
                      onClick={() => onReconnect(a)}
                    >
                      Connect again
                    </button>
                  ) : null}
                  {own ? (
                    <button
                      type="button"
                      className="fd-ghost fd-sm"
                      disabled={busy === a.account_id}
                      aria-label={`Log out of ${a.account_id}`}
                      onClick={() => onLogout(a)}
                    >
                      Log out
                    </button>
                  ) : (
                    <span className="fd-note">From the config file</span>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
