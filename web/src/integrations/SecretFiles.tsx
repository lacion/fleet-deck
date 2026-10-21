// FS-3 (design §3.6, §3.7): the "Secret files" section of the project page: the list of paths, the
// "Sync now" button and the table of the last report of each file.
import { Fragment, useEffect, useState } from 'react';
import { stationName } from '../board/fleet';
import type {
  Project,
  SecretFileReport,
  SecretFilesView,
  SecretStationReport,
  StationView,
} from '../gen';
import { ApiError } from '../runtime/api';
import { useIntegrations } from './context';
import { message } from './useLoad';

/** The paths of the text area: one for each line, trimmed, without the empty lines. */
export function parsePaths(text: string): string[] {
  return text
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line !== '');
}

const REFUSED: Readonly<Record<string, string>> = {
  not_ignored: 'Not ignored by Git',
  symlink: 'A link',
  too_large: 'Larger than 512 KiB',
  not_regular: 'Not a regular file',
  invalid_path: 'Invalid path',
  private_file: 'A private file of the station',
};

/** The text of one cell of the table: the state of one file on one station. */
export function cellText(r: SecretStationReport): string {
  switch (r.state) {
    case 'same':
      return 'Same';
    case 'written':
      return 'Copied';
    case 'missing':
      return 'Missing';
    case 'refused':
      return (r.code === undefined ? undefined : REFUSED[r.code]) ?? 'Refused';
    case 'offline':
      return 'Offline';
    case 'failed':
      return `Failed: ${r.message ?? r.code ?? 'unknown'}`;
    default:
      return 'Unknown';
  }
}

/** The note under the row of one file: the parts are joined by one space. */
export function fileNote(f: SecretFileReport, stations: readonly StationView[]): string {
  const parts: string[] = [];
  const source = f.source === undefined ? undefined : stationName(f.source, stations);
  if (f.synced_at !== undefined) {
    const time = new Date(f.synced_at).toLocaleString();
    parts.push(source === undefined ? `Last sync ${time}` : `Last sync ${time} from ${source}`);
  }
  if (f.outcome === 'not_run') parts.push('Not synced yet.');
  if (f.conflict) {
    parts.push(
      `Two homes changed this file. ${source ?? 'One home'} had the newest copy; the other copies are in the station backups.`,
    );
  }
  if (f.outcome === 'partial') parts.push('Some homes did not get the file.');
  return parts.join(' ');
}

type Props = {
  readonly project: Project;
  readonly stations: readonly StationView[];
  readonly view: SecretFilesView;
  readonly draft: string;
  readonly busy: 'save' | 'sync' | null;
  readonly error: string | null;
  readonly fieldError: string | null;
  readonly onDraft: (text: string) => void;
  readonly onSave: () => void;
  readonly onSync: () => void;
};

/** Everything below the heading text: the text area, the buttons, the table and the alerts. */
export function SecretFilesBody({
  project,
  stations,
  view,
  draft,
  busy,
  error,
  fieldError,
  onDraft,
  onSave,
  onSync,
}: Props) {
  const homes = [...(project.homes ?? [])].sort((a, b) => a.position - b.position);
  const ready = homes.filter((h) => h.state === 'ready').length;
  const cannotSync = view.paths.length === 0 || ready < 2 || busy !== null;
  return (
    <>
      <textarea
        className="fd-int-secret-paths"
        aria-label="Secret file paths"
        rows={4}
        value={draft}
        spellCheck={false}
        autoComplete="off"
        onChange={(e) => onDraft(e.target.value)}
      />
      {fieldError === null ? null : (
        <p role="alert" className="fd-formerror">
          {fieldError}
        </p>
      )}
      <div className="fd-pane-row">
        <button type="button" className="fd-ghost" disabled={busy !== null} onClick={onSave}>
          Save
        </button>
        <button type="button" className="fd-ghost" disabled={cannotSync} onClick={onSync}>
          {busy === 'sync' ? 'Syncing…' : 'Sync now'}
        </button>
      </div>
      {error === null ? null : (
        <p role="alert" className="fd-formerror">
          {error}
        </p>
      )}
      {view.files.length === 0 ? null : (
        <div className="fd-int-secret-scroll">
          <table className="fd-int-secret-table">
            <thead>
              <tr>
                <th scope="col">File</th>
                {homes.map((h) => (
                  <th key={h.station_id} scope="col">
                    {stationName(h.station_id, stations)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {view.files.map((f) => {
                const note = fileNote(f, stations);
                return (
                  <Fragment key={f.path}>
                    <tr>
                      <th scope="row">
                        <code>{f.path}</code>
                      </th>
                      {homes.map((h) => {
                        const report = f.stations.find((s) => s.station_id === h.station_id);
                        return (
                          <td key={h.station_id}>
                            {report === undefined ? '—' : cellText(report)}
                          </td>
                        );
                      })}
                    </tr>
                    {note === '' ? null : (
                      <tr>
                        <td className="fd-note" colSpan={homes.length + 1}>
                          {note}
                        </td>
                      </tr>
                    )}
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}

/** The section; it shows nothing without the admin login. */
export function SecretFiles({
  project,
  stations,
}: {
  readonly project: Project;
  readonly stations: readonly StationView[];
}) {
  const env = useIntegrations();
  const actions = env?.actions;
  const id = project.project_id;
  const [view, setView] = useState<SecretFilesView | null>(null);
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState<'save' | 'sync' | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [fieldError, setFieldError] = useState<string | null>(null);
  const admin = actions?.canAdmin === true;
  useEffect(() => {
    if (actions === undefined || !admin) return undefined;
    let live = true;
    actions.secretFiles(id).then(
      (v) => {
        if (!live) return;
        setView(v);
        setDraft(v.paths.join('\n'));
        setLoadError(null);
      },
      (e: unknown) => live && setLoadError(message(e)),
    );
    return () => {
      live = false;
    };
  }, [actions, admin, id]);
  if (actions === undefined || !admin) return null;
  const save = () => {
    if (busy !== null) return;
    setBusy('save');
    setError(null);
    setFieldError(null);
    actions
      .setSecretFiles(id, parsePaths(draft))
      .then((v) => {
        setView(v);
        setDraft(v.paths.join('\n'));
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.field === 'paths') setFieldError(e.message);
        else setError(message(e));
      })
      .finally(() => setBusy(null));
  };
  const sync = () => {
    if (busy !== null) return;
    setBusy('sync');
    setError(null);
    actions
      .syncSecretFiles(id)
      .then((report) => setView((cur) => (cur === null ? cur : { ...cur, files: report.files })))
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(null));
  };
  return (
    <section className="fd-panel fd-int-secrets" aria-label="Secret files">
      <h3 className="fd-label">Secret files</h3>
      <p className="fd-note">
        Files that Git ignores, such as .env, copied between the homes of this project. The newest
        copy wins; the replaced copy is kept on its station.
      </p>
      {view === null ? (
        loadError === null ? (
          <p role="status" className="fd-note">
            Loading…
          </p>
        ) : (
          <p role="alert" className="fd-formerror">
            {loadError}
          </p>
        )
      ) : (
        <SecretFilesBody
          project={project}
          stations={stations}
          view={view}
          draft={draft}
          busy={busy}
          error={error}
          fieldError={fieldError}
          onDraft={setDraft}
          onSave={save}
          onSync={sync}
        />
      )}
    </section>
  );
}
