// DL-172: the provider logins of a station: one row for each instance (name, provider, program,
// config folder) and a line of its environment variables under it. A variable shows "set", never
// its value; a new value goes through the secret dialog (the relay), not through the save.
import { Fragment } from 'react';
import { Icon, Plus, X } from '../ui/icons';
import { type Instance, asInstances } from './model';

/** The providers of a login: the value in the file, and its name. */
const DRIVERS = [
  { value: 'claude', text: 'Claude' },
  { value: 'codex', text: 'Codex' },
] as const;

export function InstancesEditor({
  value,
  onChange,
  disabled,
  savedIds,
  secretBlocked,
  onSecret,
}: {
  readonly value: unknown;
  readonly onChange: (next: Instance[]) => void;
  readonly disabled: boolean;
  /** The ids that the station file has now: only these can take a secret. */
  readonly savedIds: readonly string[];
  /** Why a secret cannot be set now (unsaved edits of this field), or `null`. */
  readonly secretBlocked: string | null;
  /** `name` is `null` for "Add variable". */
  readonly onSecret: (instanceId: string, name: string | null) => void;
}) {
  const rows = asInstances(value);
  const patch = (i: number, next: Partial<Instance>) =>
    onChange(rows.map((r, at) => (at === i ? { ...r, ...next } : r)));
  const removeVar = (i: number, name: string) =>
    patch(i, { env: { ...(rows[i]?.env ?? {}), [name]: null } });
  const secretOff = disabled || secretBlocked !== null;
  return (
    <div className="fd-set-instances">
      {rows.length === 0 ? <p className="fd-note">No provider logins.</p> : null}
      {rows.length === 0 ? null : (
        <div className="fd-tablewrap">
          <table className="fd-table fd-set-insttable" aria-label="Provider logins">
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">Provider</th>
                <th scope="col">Program</th>
                <th scope="col">Config folder</th>
                <th scope="col">
                  <span className="fd-vh">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r, i) => {
                const saved = savedIds.includes(r.id);
                const vars = Object.entries(r.env).filter(([, v]) => v !== null);
                const who = r.id || `login ${i + 1}`;
                return (
                  <Fragment key={i}>
                    <tr aria-label={`Provider login ${r.id || i + 1}`}>
                      <td data-label="Name">
                        {saved ? (
                          <code>{r.id}</code>
                        ) : (
                          <input
                            aria-label={`Name of login ${i + 1}`}
                            value={r.id}
                            disabled={disabled}
                            maxLength={32}
                            placeholder="claude-work"
                            autoComplete="off"
                            spellCheck={false}
                            data-mono="true"
                            onChange={(e) => patch(i, { id: e.target.value })}
                          />
                        )}
                      </td>
                      <td data-label="Provider">
                        <select
                          aria-label={`Provider of ${who}`}
                          value={r.driver}
                          disabled={disabled}
                          onChange={(e) => patch(i, { driver: e.target.value })}
                        >
                          {DRIVERS.map((d) => (
                            <option key={d.value} value={d.value}>
                              {d.text}
                            </option>
                          ))}
                          {DRIVERS.some((d) => d.value === r.driver) ? null : (
                            <option value={r.driver}>{r.driver}</option>
                          )}
                        </select>
                      </td>
                      <td data-label="Program">
                        <input
                          aria-label={`Program of ${who}`}
                          value={r.binary}
                          disabled={disabled}
                          placeholder={r.driver}
                          autoComplete="off"
                          spellCheck={false}
                          data-mono="true"
                          onChange={(e) => patch(i, { binary: e.target.value })}
                        />
                      </td>
                      <td data-label="Config folder">
                        <input
                          aria-label={`Config folder of ${who}`}
                          className="fd-set-wordph"
                          value={r.config_home ?? ''}
                          disabled={disabled}
                          placeholder="Default"
                          autoComplete="off"
                          spellCheck={false}
                          data-mono="true"
                          onChange={(e) =>
                            patch(i, { config_home: e.target.value === '' ? null : e.target.value })
                          }
                        />
                      </td>
                      <td className="fd-cell-actions">
                        {/* The X icon button of every list of the page (RemoveButton). */}
                        <button
                          type="button"
                          className="fd-iconbtn fd-set-remove"
                          disabled={disabled}
                          aria-label={`Remove login ${r.id || i + 1}`}
                          title={`Remove login ${r.id || i + 1}`}
                          onClick={() => onChange(rows.filter((_, at) => at !== i))}
                        >
                          <Icon of={X} size={16} />
                        </button>
                      </td>
                    </tr>
                    <tr className="fd-set-envrow">
                      <td colSpan={5}>
                        <div
                          className="fd-set-env"
                          role="group"
                          aria-label={`Variables of ${r.id}`}
                        >
                          <span className="fd-label">Variables</span>
                          {vars.map(([name]) => (
                            <span className="fd-set-envvar" key={name}>
                              <code>{name}</code>
                              <span className="fd-badge" data-tone="ok">
                                set
                              </span>
                              <button
                                type="button"
                                className="fd-ghost fd-sm"
                                disabled={secretOff || !saved}
                                aria-label={`Replace ${name}`}
                                onClick={() => onSecret(r.id, name)}
                              >
                                Replace…
                              </button>
                              <button
                                type="button"
                                className="fd-iconbtn fd-sm"
                                disabled={disabled}
                                aria-label={`Remove ${name}`}
                                title={`Remove ${name}`}
                                onClick={() => removeVar(i, name)}
                              >
                                <Icon of={X} size={14} />
                              </button>
                            </span>
                          ))}
                          <button
                            type="button"
                            className="fd-ghost fd-sm"
                            disabled={secretOff || !saved}
                            onClick={() => onSecret(r.id, null)}
                          >
                            <Icon of={Plus} size={14} /> Add variable
                          </button>
                        </div>
                      </td>
                    </tr>
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      {secretBlocked === null ? null : <p className="fd-note">{secretBlocked}</p>}
      <div>
        <button
          type="button"
          className="fd-ghost fd-sm"
          disabled={disabled}
          onClick={() =>
            onChange([
              ...rows,
              { id: '', driver: 'claude', binary: '', config_home: null, env: {} },
            ])
          }
        >
          <Icon of={Plus} size={14} /> Add login
        </button>
      </div>
    </div>
  );
}
