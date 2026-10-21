// FG-5 (design §8.8): the "Homes" section of the project page: one row for each home, "Retry" for a
// failed home, "Remove" for a home when the project has more than one, and "Add home".
import { useEffect, useState } from 'react';
import { stationName } from '../board/fleet';
import type { Project, ProjectHome, StationView } from '../gen';
import { FolderPicker, type ListWorkspaces } from '../ui/FolderPicker';
import { Icon, Plus } from '../ui/icons';
import type { IntegrationActions } from './actions';
import { useIntegrations } from './context';
import { Dialog } from './Dialog';
import { message } from './useLoad';

/** How often the list is read again while a home is cloning. */
export const HOMES_POLL_MS = 5_000;

/**
 * The folder name of a project (mirror of `home_folder_name` in `fleetdeck-core`): every character
 * outside `[A-Za-z0-9._-]` becomes `-`, a leading `.` becomes `-`, cut to 80 characters.
 */
export function homeFolderName(name: string): string {
  const out = Array.from(name, (c) => (/^[A-Za-z0-9._-]$/.test(c) ? c : '-'))
    .slice(0, 80)
    .join('');
  return out.startsWith('.') ? `-${out.slice(1)}` : out;
}

/** The reason text of a home that is not ready: the message, else the code. */
const reasonOf = (home: ProjectHome): string => home.message ?? home.code ?? 'no reason given';

/** The text of one home row (design §8.8), with the station name first. */
export function homeText(home: ProjectHome, station: string): string {
  switch (home.state) {
    case 'ready':
      return `${station} · ${home.path ?? ''} · Ready`;
    case 'cloning':
      return `${station} · Cloning into ${home.reserved_path ?? home.path ?? ''}…`;
    case 'unverified':
      return `${station} · Not verified: ${reasonOf(home)}`;
    case 'failed':
      return `${station} · Failed: ${reasonOf(home)}`;
    default:
      return `${station} · State unknown`;
  }
}

/** The stations that can get a new home: they report Fleet Git and have no home of the project. */
export function homeCandidates(
  project: Pick<Project, 'homes'>,
  stations: readonly StationView[],
): StationView[] {
  const taken = new Set((project.homes ?? []).map((h) => h.station_id));
  return stations.filter((s) => s.info?.fleet_git !== undefined && !taken.has(s.station_id));
}

/** How the new home is made: a new clone below the clone root, or an existing folder. */
export type AddChoice = 'clone' | 'adopt';

export function AddHomeDialog({
  project,
  candidates,
  actions,
  onDone,
  onClose,
  initialChoice = 'clone',
  list,
}: {
  readonly project: Project;
  readonly candidates: readonly StationView[];
  readonly actions: IntegrationActions;
  readonly list?: ListWorkspaces | undefined;
  readonly onDone: () => void;
  readonly onClose: () => void;
  readonly initialChoice?: AddChoice;
}) {
  const [station, setStation] = useState(candidates[0]?.station_id ?? '');
  const [choice, setChoice] = useState<AddChoice>(initialChoice);
  const [path, setPath] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const chosen = candidates.find((s) => s.station_id === station);
  const root = chosen?.info?.fleet_git?.clone_root ?? '';
  const folder = path.trim();
  const adopt = choice === 'adopt';
  const ready = chosen !== undefined && (!adopt || folder.startsWith('/'));
  const submit = () => {
    if (busy || chosen === undefined || !ready) return;
    setBusy(true);
    setError(null);
    (adopt
      ? actions.addHome(project.project_id, chosen.station_id, folder)
      : actions.addHome(project.project_id, chosen.station_id)
    )
      .then(() => {
        onDone();
        onClose();
      })
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(false));
  };
  return (
    <Dialog title="Add a home on another station" onClose={onClose}>
      {candidates.length === 0 ? (
        <p className="fd-note">No station can get a home of this project.</p>
      ) : (
        <>
          <label>
            Station
            <select value={station} onChange={(e) => setStation(e.target.value)}>
              {candidates.map((s) => (
                <option key={s.station_id} value={s.station_id}>
                  {s.name}
                </option>
              ))}
            </select>
          </label>
          <div role="radiogroup" aria-label="How to add the home">
            <label>
              <input
                type="radio"
                name="add-home-choice"
                checked={!adopt}
                onChange={() => setChoice('clone')}
              />{' '}
              Clone a new copy
            </label>
            <label>
              <input
                type="radio"
                name="add-home-choice"
                checked={adopt}
                onChange={() => setChoice('adopt')}
              />{' '}
              Use an existing folder on that station
            </label>
          </div>
          {adopt ? (
            <>
              <FolderPicker
                stationId={station}
                value={path}
                onChange={setPath}
                list={list}
                label="Folder path"
                placeholder="/home/you/projects/app"
              />
              <p className="fd-note">
                The station checks that this folder is a complete clone of {project.repo ?? ''}. It
                never clones into it.
              </p>
            </>
          ) : (
            <p className="fd-note">
              The station clones {project.remote_url ?? ''} into {root}/
              {homeFolderName(project.name)}. When that folder is already a complete clone of{' '}
              {project.repo ?? ''}, the station uses it.
            </p>
          )}
        </>
      )}
      {error === null ? null : (
        <p role="alert" className="fd-formerror">
          {error}
        </p>
      )}
      <div className="fd-pane-row">
        <button type="button" className="fd-ghost" onClick={onClose}>
          Cancel
        </button>
        <button type="button" className="fd-primary" disabled={busy || !ready} onClick={submit}>
          {busy ? 'Adding…' : 'Add home'}
        </button>
      </div>
    </Dialog>
  );
}

/** The Homes section; it shows nothing when the deck sent no `homes` (an older deck). */
export function ProjectHomes({
  project,
  stations,
  list,
}: {
  readonly project: Project;
  readonly stations: readonly StationView[];
  /** Reads the folders of a station ("Browse…" of an existing folder). */
  readonly list?: ListWorkspaces | undefined;
}) {
  const env = useIntegrations();
  const [adding, setAdding] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const homes = project.homes;
  const cloning = Array.isArray(homes) && homes.some((h) => h.state === 'cloning');
  const reload = env?.reload;
  useEffect(() => {
    if (!cloning || reload === undefined) return undefined;
    const timer = setInterval(reload, HOMES_POLL_MS);
    return () => clearInterval(timer);
  }, [cloning, reload]);
  if (env === undefined || !Array.isArray(homes) || homes.length === 0) return null;
  const { actions } = env;
  const admin = actions.canAdmin;
  const rows = [...homes].sort((a, b) => a.position - b.position);
  const run = (key: string, call: Promise<unknown>) => {
    setBusy(key);
    setError(null);
    call
      .then(() => env.reload())
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(null));
  };
  const hasRemote = project.remote_url !== null && project.remote_url !== undefined;
  const slugs = Array.isArray(project.slugs) ? project.slugs : [];
  return (
    <section className="fd-panel fd-int-homes" aria-label="Homes">
      <h3 className="fd-label">Homes</h3>
      <ul className="fd-int-list">
        {rows.map((home) => (
          <li key={home.station_id} data-state={home.state}>
            <span>{homeText(home, stationName(home.station_id, stations))}</span>
            {admin && home.state === 'failed' ? (
              <button
                type="button"
                className="fd-ghost fd-sm"
                disabled={busy !== null}
                aria-label={`Retry the home on ${stationName(home.station_id, stations)}`}
                onClick={() =>
                  run(
                    `retry:${home.station_id}`,
                    actions.retryHome(project.project_id, home.station_id),
                  )
                }
              >
                Retry
              </button>
            ) : null}
            {admin && rows.length > 1 ? (
              <button
                type="button"
                className="fd-ghost fd-sm"
                disabled={busy !== null}
                aria-label={`Remove the home on ${stationName(home.station_id, stations)}`}
                onClick={() =>
                  run(
                    `remove:${home.station_id}`,
                    actions.removeHome(project.project_id, home.station_id),
                  )
                }
              >
                Remove
              </button>
            ) : null}
          </li>
        ))}
      </ul>
      {error === null ? null : (
        <p role="alert" className="fd-formerror">
          {error}
        </p>
      )}
      {slugs.length === 0 ? null : (
        <p className="fd-plan-meta">
          Git names:{' '}
          {slugs.map((s) => `${s.slug} → ${stationName(s.station_id, stations)}`).join(', ')}
        </p>
      )}
      {admin ? (
        <div className="fd-pane-row">
          <button
            type="button"
            className="fd-ghost"
            disabled={!hasRemote}
            onClick={() => setAdding(true)}
          >
            <Icon of={Plus} size={14} /> Add home
          </button>
          {hasRemote ? null : (
            <span className="fd-note">Set the remote URL of the project first.</span>
          )}
        </div>
      ) : null}
      {adding ? (
        <AddHomeDialog
          project={project}
          candidates={homeCandidates(project, stations)}
          actions={actions}
          list={list}
          onDone={env.reload}
          onClose={() => setAdding(false)}
        />
      ) : null}
    </section>
  );
}
