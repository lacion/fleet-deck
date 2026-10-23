// DL-172: the `[[trackers]]` entries of station.toml, read-only.
const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const text = (v: unknown): string => (typeof v === 'string' ? v : '');

/** Where a tracker token comes from: `env NAME` or `file /path`. The token itself is never shown. */
function tokenSource(v: unknown): string {
  if (!isRecord(v)) return '';
  if (typeof v['env'] === 'string') return `env ${v['env']}`;
  if (typeof v['file'] === 'string') return `file ${v['file']}`;
  return '';
}

export function TrackersTable({ value }: { readonly value: unknown }) {
  const rows = Array.isArray(value) ? value.filter(isRecord) : [];
  return (
    <div className="fd-set-trackers">
      {rows.length === 0 ? (
        <p className="fd-note">No trackers in the config file.</p>
      ) : (
        <div className="fd-tablewrap">
          <table className="fd-table" aria-label="Trackers from the config file">
            <thead>
              <tr>
                <th scope="col">Id</th>
                <th scope="col">Kind</th>
                <th scope="col">Site</th>
                <th scope="col">Token from</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r, i) => (
                <tr key={`${text(r['id'])}/${i}`}>
                  <td className="fd-cell-name" data-label="Id">
                    {text(r['id'])}
                  </td>
                  <td data-label="Kind">{text(r['kind'])}</td>
                  <td data-label="Site">{text(r['site'])}</td>
                  <td className="fd-cell-path" data-label="Token from">
                    <code>{tokenSource(r['token'])}</code>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <p className="fd-note">
        From the config file of this station. Connect accounts in the Accounts tab.
      </p>
    </div>
  );
}
