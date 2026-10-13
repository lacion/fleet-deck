// R3.2 (DL-164): the Projects page (`#/projects`): the list, "New project", "Edit", "Check" and
// "Delete". The deck keeps these routes for the owner; the page is empty without that login.
import { useState } from 'react';
import { stationName } from '../board/fleet';
import type { IntegrationsStatus, Project, StationView } from '../gen';
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
};

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

export function ProjectsPage({ stations, initialForm = null, initialChecks = {} }: Props) {
  const env = useIntegrations();
  const [form, setForm] = useState(initialForm);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [checks, setChecks] = useState<Readonly<Record<string, ProjectPreflight>>>(initialChecks);
  const [statuses, setStatuses] = useState<Readonly<Record<string, IntegrationsStatus>>>({});
  if (env === undefined || !env.actions.canAdmin) {
    return (
      <main className="fd-plans fd-int" aria-label="Projects">
        <h2>Projects</h2>
        <p className="fd-note">Projects need the owner’s login.</p>
      </main>
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
  return (
    <main className="fd-plans fd-int" aria-label="Projects">
      <h2>Projects</h2>
      <p className="fd-note">
        A project is a repository on a station with its forge and its issue trackers. The deck
        stores the bindings only. The station holds the tokens.
      </p>
      <div className="fd-plan-tools">
        <button
          type="button"
          disabled={form !== null || first === undefined}
          onClick={() => {
            setSaveError(null);
            setForm({ id: null, draft: emptyDraft(first?.station_id ?? '') });
          }}
        >
          New project
        </button>
        <button type="button" onClick={reload}>
          Refresh
        </button>
      </div>
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
          status={form.id === null ? null : (statuses[form.id] ?? null)}
          onSave={save}
          onCancel={() => setForm(null)}
        />
      )}
      {list === null ? (
        env.error === null ? (
          <p role="status">Loading projects…</p>
        ) : null
      ) : list.length === 0 ? (
        <p className="fd-note">
          No projects yet. Add one to see its issues and pull requests, and to start sessions from
          them.
        </p>
      ) : (
        <ol className="fd-plan-list">
          {list.map((p) => {
            const result = checks[p.project_id];
            return (
              <li key={p.project_id}>
                <article aria-label={`Project ${p.name}`}>
                  <h3>
                    <a href={`#/project/${encodeURIComponent(p.project_id)}`}>{p.name}</a>
                  </h3>
                  <p className="fd-plan-meta">
                    {stationName(p.station_id, stations)} · <code>{p.path}</code>
                  </p>
                  <ProjectChips project={p} />
                  {result === undefined ? null : <PreflightView result={result} />}
                  <div className="fd-plan-actions">
                    <button
                      type="button"
                      disabled={form !== null}
                      onClick={() => {
                        setSaveError(null);
                        setForm({ id: p.project_id, draft: draftOf(p) });
                      }}
                    >
                      Edit
                    </button>
                    <button type="button" disabled={busy === p.project_id} onClick={() => check(p)}>
                      {busy === p.project_id ? 'Checking…' : 'Check'}
                    </button>
                    {confirming === p.project_id ? (
                      <>
                        <span>Delete this project?</span>
                        <button
                          type="button"
                          className="fd-danger"
                          disabled={busy === p.project_id}
                          onClick={() => remove(p)}
                        >
                          Delete
                        </button>
                        <button
                          type="button"
                          className="fd-ghost"
                          onClick={() => setConfirming(null)}
                        >
                          Keep
                        </button>
                      </>
                    ) : (
                      <button
                        type="button"
                        className="fd-ghost"
                        onClick={() => setConfirming(p.project_id)}
                      >
                        Delete…
                      </button>
                    )}
                  </div>
                </article>
              </li>
            );
          })}
        </ol>
      )}
    </main>
  );
}
