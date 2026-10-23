// DL-173: the accounts of one section: kind and method, site, user, status, and the actions. The
// table sits on the canvas (§4.7) and runs the width of the column; its columns have fixed widths,
// so that the tables of the tab line up (settings.css). A provider without an account can have a
// row of its own ("Not connected" and the ways to connect), so that every provider of a section
// has the same structure.
import type { ReactNode } from 'react';
import type { AccountKind, AccountView } from '../gen';
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

/** A provider of the section without an account: what it needs, and the buttons to connect. */
export type AbsentRow = {
  readonly kind: AccountKind;
  /** The sentence in the Site and User columns: how this provider connects. */
  readonly note: ReactNode;
  readonly actions: ReactNode;
};

/** The order of the providers in a table. */
const KINDS: readonly AccountKind[] = ['github', 'gitlab', 'jira', 'linear'];

type Row =
  | { readonly kind: AccountKind; readonly account: AccountView }
  | { readonly kind: AccountKind; readonly absent: AbsentRow };

function AccountRow({
  a,
  busy,
  onReconnect,
  onLogout,
}: {
  readonly a: AccountView;
  readonly busy: string | null;
  readonly onReconnect?: ((account: AccountView) => void) | undefined;
  readonly onLogout: (account: AccountView) => void;
}) {
  const status = STATUS[a.status];
  const own = a.method !== 'config';
  return (
    <tr aria-label={`Account ${a.account_id}`}>
      <td className="fd-cell-name" data-label="Account">
        <span className="fd-acc-name">
          {KIND_LABEL[a.kind]} <span className="fd-badge">{METHOD_LABEL[a.method]}</span>
        </span>
      </td>
      <td className="fd-cell-path" data-label="Site" title={siteText(a.site) || undefined}>
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
          // Log out removes the login of the station: the danger look, as Revoke and Remove.
          <button
            type="button"
            className="fd-danger fd-sm"
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
}

/** The row of a provider without an account: the hollow idle glyph and "Not connected". */
function AbsentAccountRow({ row }: { readonly row: AbsentRow }) {
  return (
    <tr aria-label={`${KIND_LABEL[row.kind]} not connected`} data-absent="true">
      <td className="fd-cell-name" data-label="Account">
        <span className="fd-acc-name">{KIND_LABEL[row.kind]}</span>
      </td>
      <td className="fd-acc-desc" colSpan={2}>
        {row.note}
      </td>
      <td data-label="Status">
        <span className="fd-acc-status">
          <span className="fd-dot" data-status="idle" aria-hidden="true" />
          Not connected
        </span>
      </td>
      <td className="fd-cell-actions">{row.actions}</td>
    </tr>
  );
}

export function AccountTable({
  label,
  accounts,
  absent = [],
  busy,
  onReconnect,
  onLogout,
  emptyText = 'No account connected.',
}: {
  readonly label: string;
  readonly accounts: readonly AccountView[];
  /** The providers of the section that have no account: a row each, in the provider order. */
  readonly absent?: readonly AbsentRow[];
  /** The account id whose call runs now. */
  readonly busy: string | null;
  /** Absent: the section has no "Reconnect" (a forge login starts again from "Connect"). */
  readonly onReconnect?: ((account: AccountView) => void) | undefined;
  readonly onLogout: (account: AccountView) => void;
  /** The line of an empty list; `null`: the section says it in its own words. */
  readonly emptyText?: string | null;
}) {
  const rows: Row[] = KINDS.flatMap((kind): Row[] => [
    ...accounts.filter((a) => a.kind === kind).map((account) => ({ kind, account })),
    ...absent.filter((r) => r.kind === kind).map((row) => ({ kind, absent: row })),
  ]);
  if (rows.length === 0) {
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
          {rows.map((row) =>
            'account' in row ? (
              <AccountRow
                key={row.account.account_id}
                a={row.account}
                busy={busy}
                onReconnect={onReconnect}
                onLogout={onLogout}
              />
            ) : (
              <AbsentAccountRow key={`absent:${row.kind}`} row={row.absent} />
            ),
          )}
        </tbody>
      </table>
    </div>
  );
}
