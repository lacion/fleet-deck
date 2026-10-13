// R3.2 (DL-164): the Projects page (`#/projects`): the list, "New project", "Edit", "Check" and
// "Delete". The deck keeps these routes for the owner; the page is empty without that login.
import { Fragment, useState } from 'react';
import { stationName } from '../board/fleet';
import type { DeckSession, IntegrationsStatus, Project, StationView } from '../gen';
import type { ListWorkspaces } from '../ui/FolderPicker';
import { FolderGit2, Icon, Plus, RefreshCw } from '../ui/icons';
import { EmptyState, Page } from '../ui/page';
import { useIntegrations } from './context';
import { draftOf, emptyDraft, forgeChip, inputOf, type ProjectDraft, trackerChip } from './model';
import { PreflightView } from './Preflight';
import { ProjectForm } from './ProjectForm';
import { message } from './useLoad';
import type { ProjectPreflight } from '../gen';

type Props = {
  readonly stations: readonly StationView[];
  /** The form at the first render (tests). */
  readonly initialForm?: { readonly id: string | null; readonly draft: ProjectDraft } | null;
  /** The "Check" results at the first render (tests), by project id. */
  readonly initialChecks?: Readonly<Record<string, ProjectPreflight>>;
  /** Reads a folder of a station, for the folder picker of the form. */
  readonly list?: ListWorkspaces | undefined;
  readonly sessions?: readonly DeckSession[] | undefined;
};

/** The tracker accounts that a station reported in the checks of its projects. */
function statusOfStation(
  station: string,
  projects: readonly Project[],
  statuses: Readonly<Record<string, IntegrationsStatus>>,
): IntegrationsStatus | null {
  for (const p of projects) {
    const status = statuses[p.project_id];
    if (p.station_id === station && status !== undefined) return status;
  }
  return null;
}

/** The chips of a project: its forge and its trackers. */
export function ProjectChips({ project }: { readonly project: Project }) {
  return (
    <span className="fd-int-chips">
      {project.forge === null ? null : (
        <span className="fd-badge" data-chip="forge">
          {forgeChip(project.forge)}
        </span>
      )}
      {project.trackers.map((t) => (
        <span key={`${t.tracker}/${t.key}`} className="fd-badge" data-chip="tracker">
          {trackerChip(t)}
        </span>
      ))}
    </span>
  );
}

export function ProjectsPage({
  stations,
  initialForm = null,
  initialChecks = {},
  list: listFolders,
  sessions,
}: Props) {
  const env = useIntegrations();
  const [form, setForm] = useState(initialForm);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [checks, setChecks] = useState<Readonly<Record<string, ProjectPreflight>>>(initialChecks);
  const [statuses, setStatuses] = useState<Readonly<Record<string, IntegrationsStatus>>>(() => {
    const out: Record<string, IntegrationsStatus> = {};
    for (const [id, result] of Object.entries(initialChecks)) {
      if (result.status !== null) out[id] = result.status;
    }
    return out;
  });
  if (env === undefined || !env.actions.canAdmin) {
    return (
      <Page title="Projects">
        <p className="fd-note">Projects need the owner’s login.</p>
      </Page>
    );
  }
  const { actions, projects, reload } = env;
  const first = stations[0];
  const list = projects;
  const save = () => {
    if (form === null) return;
    setSaving(true);
    setSaveError(null);
    const body = inputOf(form.draft);
    const call =
      form.id === null ? actions.createProject(body) : actions.updateProject(form.id, body);
    call
      .then(() => {
        setForm(null);
        reload();
      })
      .catch((e: unknown) => setSaveError(message(e)))
      .finally(() => setSaving(false));
  };
  const check = (p: Project) => {
    setBusy(p.project_id);
    setError(null);
    actions
      .preflight(p.project_id)
      .then((result) => {
        setChecks((cur) => ({ ...cur, [p.project_id]: result }));
        const status = result.status;
        if (status !== null) setStatuses((cur) => ({ ...cur, [p.project_id]: status }));
      })
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(null));
  };
  const remove = (p: Project) => {
    setConfirming(null);
    setBusy(p.project_id);
    setError(null);
    actions
      .deleteProject(p.project_id)
      .then(reload)
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(null));
  };
  const openForm = (id: string | null, draft: ProjectDraft) => {
    setSaveError(null);
    setForm({ id, draft });
  };
  return (
    <Page
      title="Projects"
      description="A project is a repository on a station with its forge and its issue trackers. The deck stores the bindings only. The station holds the tokens."
      wide
      actions={
        <>
          {form === null ? (
            <button
              type="button"
              className="fd-primary"
              disabled={first === undefined}
              onClick={() => openForm(null, emptyDraft(first?.station_id ?? ''))}
            >
              <Icon of={Plus} /> New project
            </button>
          ) : null}
          <button type="button" className="fd-ghost" onClick={reload}>
            <Icon of={RefreshCw} /> Refresh
          </button>
        </>
      }
    >
      {env.error === null ? null : <p role="alert">{env.error}</p>}
      {error === null ? null : <p role="alert">{error}</p>}
      {form === null ? null : (
        <ProjectForm
          key={form.id ?? 'new'}
          stations={stations}
          draft={form.draft}
          onChange={(draft) => setForm({ id: form.id, draft })}
          editing={form.id !== null}
          saving={saving}
          error={saveError}
          status={statusOfStation(form.draft.station, list ?? [], statuses)}
          onSave={save}
          onCancel={() => setForm(null)}
          list={listFolders}
          sessions={sessions}
        />
      )}
      {list === null ? (
        env.error === null ? (
          <p role="status">Loading projects…</p>
        ) : null
      ) : list.length === 0 ? (
        <EmptyState
          icon={FolderGit2}
          title="No projects yet"
          body="Add a project to see its issues and pull requests, and to start sessions from them."
        />
      ) : (
        <div className="fd-panel fd-tablepanel">
          <div className="fd-tablewrap">
            <table className="fd-table" aria-label="Projects">
              <thead>
                <tr>
                  <th scope="col">Name</th>
                  <th scope="col">Station</th>
                  <th scope="col">Folder</th>
                  <th scope="col">Forge</th>
                  <th scope="col">Trackers</th>
                  <th scope="col">
                    <span className="fd-vh">Actions</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {list.map((p) => {
                  const result = checks[p.project_id];
                  return (
                    <Fragment key={p.project_id}>
                      <tr aria-label={`Project ${p.name}`}>
                        <td className="fd-cell-name">
                          <a href={`#/project/${encodeURIComponent(p.project_id)}`}>{p.name}</a>
                        </td>
                        <td data-label="Station">{stationName(p.station_id, stations)}</td>
                        <td className="fd-cell-path" data-label="Folder">
                          <code>{p.path}</code>
                        </td>
                        <td data-label="Forge">
                          {p.forge === null ? null : (
                            <span className="fd-badge" data-chip="forge">
                              {forgeChip(p.forge)}
                            </span>
                          )}
                        </td>
                        <td data-label="Trackers">
                          <span className="fd-cellbadges">
                            {p.trackers.map((t) => (
                              <span
                                key={`${t.tracker}/${t.key}`}
                                className="fd-badge"
                                data-chip="tracker"
                              >
                                {trackerChip(t)}
                              </span>
                            ))}
                          </span>
                        </td>
                        <td className="fd-cell-actions">
                          {confirming === p.project_id ? (
                            <>
                              <span>Delete this project?</span>
                              <button
                                type="button"
                                className="fd-danger fd-sm"
                                disabled={busy === p.project_id}
                                onClick={() => remove(p)}
                              >
                                Delete
                              </button>
                              <button
                                type="button"
                                className="fd-ghost fd-sm"
                                onClick={() => setConfirming(null)}
                              >
                                Keep
                              </button>
                            </>
                          ) : (
                            <>
                              <button
                                type="button"
                                className="fd-sm"
                                disabled={form !== null}
                                onClick={() => openForm(p.project_id, draftOf(p))}
                              >
                                Edit
                              </button>
                              <button
                                type="button"
                                className="fd-sm"
                                disabled={busy === p.project_id}
                                onClick={() => check(p)}
                              >
                                {busy === p.project_id ? 'Checking…' : 'Check'}
                              </button>
                              <button
                                type="button"
                                className="fd-ghost fd-sm"
                                onClick={() => setConfirming(p.project_id)}
                              >
                                Delete…
                              </button>
                            </>
                          )}
                        </td>
                      </tr>
                      {result === undefined ? null : (
                        <tr>
                          <td colSpan={6}>
                            <PreflightView result={result} />
                          </td>
                        </tr>
                      )}
                    </Fragment>
                  );
                })}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </Page>
  );
}
