// FS-2 (design §2.1, §3.7): the "Same repository" panel of the project page. It lists the other
// projects with the same `remote_dest` and offers "Merge into this project".
import { useState } from 'react';
import { stationName } from '../board/fleet';
import type { Project, StationView } from '../gen';
import { useIntegrations } from './context';
import { Dialog } from './Dialog';
import { message } from './useLoad';

/** The other projects with the same non-null `remote_dest`, in the order of `projects`. */
export function sameRepository(project: Project, projects: readonly Project[]): Project[] {
  return projects.filter(
    (p) =>
      p.project_id !== project.project_id &&
      typeof p.remote_dest === 'string' &&
      p.remote_dest === project.remote_dest,
  );
}

/** The text and the buttons of the merge dialog. */
export function MergeDialogBody({
  from,
  target,
  busy,
  error,
  onMerge,
  onCancel,
}: {
  readonly from: Project;
  readonly target: Project;
  readonly busy: boolean;
  readonly error: string | null;
  readonly onMerge: () => void;
  readonly onCancel: () => void;
}) {
  return (
    <>
      <p className="fd-note">
        The homes of {from.name} become homes of {target.name}, with their folders. Sessions and
        schedules move to {target.name}. The settings, prompts, trackers and secret file list of{' '}
        {from.name} are deleted.
      </p>
      {error === null ? null : (
        <p role="alert" className="fd-formerror">
          {error}
        </p>
      )}
      <div className="fd-pane-row">
        <button type="button" className="fd-ghost" onClick={onCancel}>
          Cancel
        </button>
        <button type="button" className="fd-primary" disabled={busy} onClick={onMerge}>
          {busy ? 'Merging…' : 'Merge'}
        </button>
      </div>
    </>
  );
}

/** The panel; it shows nothing without the admin login or without another project of the repository. */
export function SameRepository({
  project,
  stations,
}: {
  readonly project: Project;
  readonly stations: readonly StationView[];
}) {
  const env = useIntegrations();
  const [merging, setMerging] = useState<Project | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (env === undefined || !env.actions.canAdmin) return null;
  const others = sameRepository(project, env.projects ?? []);
  if (others.length === 0) return null;
  const close = () => {
    setMerging(null);
    setError(null);
  };
  const merge = (from: Project) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    env.actions
      .mergeProject(project.project_id, from.project_id)
      .then(() => {
        close();
        env.reload();
      })
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(false));
  };
  return (
    <section className="fd-panel fd-int-same" aria-label="Same repository">
      <h3 className="fd-label">Same repository</h3>
      <p className="fd-note">
        These projects use the same repository. Merge them to work with one project on all stations.
      </p>
      <ul className="fd-int-list">
        {others.map((from) => (
          <li key={from.project_id}>
            <span>
              {from.name} · {stationName(from.station_id, stations)} · {from.path}
            </span>
            <button
              type="button"
              className="fd-ghost fd-sm"
              aria-label={`Merge ${from.name} into this project`}
              onClick={() => {
                setError(null);
                setMerging(from);
              }}
            >
              Merge into this project
            </button>
          </li>
        ))}
      </ul>
      {merging === null ? null : (
        <Dialog title={`Merge ${merging.name} into ${project.name}?`} onClose={close}>
          <MergeDialogBody
            from={merging}
            target={project}
            busy={busy}
            error={error}
            onMerge={() => merge(merging)}
            onCancel={close}
          />
        </Dialog>
      )}
    </section>
  );
}
