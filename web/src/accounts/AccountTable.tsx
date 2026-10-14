// DL-173: the connected accounts of one section: kind, site, user, status, and the actions.
import type { AccountView } from '../gen';
import { KIND_LABEL } from './flow';

const STATUS: Readonly<Record<AccountView['status'], { text: string; tone: string }>> = {
  ok: { text: 'connected', tone: 'ok' },
  needs_login: { text: 'needs login', tone: 'act' },
  needs_reconnect: { text: 'reconnect', tone: 'act' },
  error: { text: 'error', tone: 'hazard' },
};

export function AccountTable({
  label,
  accounts,
  busy,
  onReconnect,
  onLogout,
}: {
  readonly label: string;
  readonly accounts: readonly AccountView[];
  /** The account id whose call runs now. */
  readonly busy: string | null;
  /** Absent: the section has no "Reconnect" (a forge login starts again from "Connect"). */
  readonly onReconnect?: ((account: AccountView) => void) | undefined;
  readonly onLogout: (account: AccountView) => void;
}) {
  if (accounts.length === 0) return <p className="fd-note">No account connected.</p>;
  return (
    <div className="fd-tablewrap">
      <table className="fd-table" aria-label={label}>
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
                  {KIND_LABEL[a.kind]} <span className="fd-badge">{a.method}</span>
                </td>
                <td className="fd-cell-path" data-label="Site">
                  {a.site ?? ''}
                </td>
                <td data-label="User">{a.user ?? ''}</td>
                <td data-label="Status">
                  <span className="fd-badge" data-tone={status.tone} title={a.message || undefined}>
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
