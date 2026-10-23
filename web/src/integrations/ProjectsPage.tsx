// R3.2 (DL-164): the Projects page (`#/projects`): the list, "New project", "Edit", "Check" and
// "Delete". The deck keeps these routes for the owner; the page is empty without that login.
import { Fragment, useEffect, useState } from 'react';
import { stationName } from '../board/fleet';
import type { AccountsList, DeckSession, Project, StationView } from '../gen';
import { useSettings } from '../settings/context';
import type { ListWorkspaces } from '../ui/FolderPicker';
import { PathTail } from '../chat/PathTail';
import { homeShort } from '../ui/format';
import {
  Ellipsis,
  FolderGit2,
  Icon,
  Pencil,
  Plus,
  RefreshCw,
  ShieldCheck,
  Trash2,
} from '../ui/icons';
import { Menu } from '../ui/Menu';
import { Loading } from '../ui/Loading';
import { EmptyState, ErrorNotice, Page } from '../ui/page';
import { useIntegrations } from './context';
import { draftOf, emptyDraft, inputOf, type ProjectDraft } from './model';
import { ForgeBadge, TrackerBadge } from './ProjectBadges';
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

/**
 * One group for each non-null `remote_dest` that two or more projects share, in the order of the
 * first project of the group. `repo` is the `repo` of that first project, else the `remote_dest`.
 */
export function sameRepoGroups(projects: readonly Project[]): { repo: string; count: number }[] {
  const groups = new Map<string, { repo: string; count: number }>();
  for (const p of projects) {
    if (typeof p.remote_dest !== 'string') continue;
    const group = groups.get(p.remote_dest);
    if (group === undefined) groups.set(p.remote_dest, { repo: p.repo ?? p.remote_dest, count: 1 });
    else group.count += 1;
  }
  return [...groups.values()].filter((g) => g.count >= 2);
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
  // DL-173: the form's account and container pickers read the accounts of the project's station.
  const settings = useSettings();
  const formStation = form?.draft.station ?? null;
  const [accounts, setAccounts] = useState<{
    readonly station: string;
    readonly list: AccountsList;
  } | null>(null);
  useEffect(() => {
    if (formStation === null || settings === undefined) return undefined;
    let live = true;
    settings.accounts.list(formStation).then(
      (list) => live && setAccounts({ station: formStation, list }),
      () => live && setAccounts(null),
    );
    return () => {
      live = false;
    };
  }, [formStation, settings]);
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
  // No project has a tracker: the column is absent, not a header over empty cells (§1 rule 7).
  const anyTrackers = (list ?? []).some((p) => p.trackers.length > 0);
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
  // The one general primary of the screen (§2.2): in the header, or in the empty state while the
  // list is empty (one rule for every list page).
  const empty = list !== null && list.length === 0;
  const newProject = (
    <button
      type="button"
      className="fd-primary"
      disabled={first === undefined}
      onClick={() => openForm(null, emptyDraft(first?.station_id ?? ''))}
    >
      <Icon of={Plus} /> New project
    </button>
  );
  return (
    <Page
      title="Projects"
      // The title row says how many projects there are (§4.3), only while there are some.
      count={list === null || list.length === 0 ? undefined : list.length}
      description="Repositories with their forge and trackers. Forge tokens stay on the station."
      wide
      actions={
        <>
          {form === null && !empty ? newProject : null}
          {/* The board is live: Refresh is a quiet icon button after the primary. */}
          <button
            type="button"
            className="fd-iconbtn"
            aria-label="Refresh"
            title="Refresh"
            onClick={reload}
          >
            <Icon of={RefreshCw} />
          </button>
        </>
      }
    >
      {env.error === null ? null : <ErrorNotice text={env.error} />}
      {error === null ? null : <ErrorNotice text={error} />}
      {sameRepoGroups(list ?? []).map((g) => (
        <p key={g.repo} className="fd-note" role="note">
          {g.count} projects use {g.repo}. Open one of them to merge them.
        </p>
      ))}
      {form === null ? null : (
        <ProjectForm
          key={form.id ?? 'new'}
          stations={stations}
          draft={form.draft}
          onChange={(draft) => setForm({ id: form.id, draft })}
          editing={form.id !== null}
          projectId={form.id ?? undefined}
          saving={saving}
          error={saveError}
          accounts={accounts?.station === form.draft.station ? accounts.list : null}
          loadContainers={settings?.accounts.containers}
          onSave={save}
          onCancel={() => setForm(null)}
          list={listFolders}
          sessions={sessions}
        />
      )}
      {list === null ? (
        env.error === null ? (
          <Loading text="Loading projects…" />
        ) : null
      ) : list.length === 0 ? (
        form === null ? (
          <EmptyState
            icon={FolderGit2}
            title="No projects yet"
            body="Add a project to see its issues and pull requests, and to start sessions from them."
            action={newProject}
          />
        ) : null
      ) : (
        // The table sits on the canvas, the same as the Artifacts and Activity lists (no box).
        <div className="fd-tablewrap">
          <table className="fd-table fd-int-projects" aria-label="Projects">
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">Station</th>
                <th scope="col">Folder</th>
                <th scope="col">Forge</th>
                {anyTrackers ? <th scope="col">Trackers</th> : null}
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
                        {/* The name link covers the row (integrations.css): the row opens it. */}
                        <a
                          className="fd-tap"
                          href={`#/project/${encodeURIComponent(p.project_id)}`}
                        >
                          {p.name}
                        </a>
                      </td>
                      <td data-label="Station">{stationName(p.station_id, stations)}</td>
                      <td className="fd-cell-path" data-label="Folder">
                        <code className="fd-int-path" title={p.path}>
                          <PathTail path={homeShort(p.path)} />
                        </code>
                      </td>
                      <td data-label="Forge">
                        {p.forge === null ? null : <ForgeBadge forge={p.forge} />}
                      </td>
                      {anyTrackers ? (
                        <td data-label="Trackers">
                          {/* No trackers: an empty cell (the phone card hides it, §1 rule 7). */}
                          {p.trackers.length === 0 ? null : (
                            <span className="fd-cellbadges">
                              {p.trackers.map((t) => (
                                <TrackerBadge key={`${t.tracker}/${t.key}`} tracker={t} />
                              ))}
                            </span>
                          )}
                        </td>
                      ) : null}
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
                            {busy === p.project_id ? <span role="status">Checking…</span> : null}
                            {/* One row menu (the Stations cards' form): the row stays data. */}
                            <Menu
                              summary={<Icon of={Ellipsis} />}
                              summaryClass="fd-iconbtn fd-sm"
                              label={`Actions for ${p.name}`}
                              title="Actions"
                            >
                              {(close) => (
                                <>
                                  <button
                                    type="button"
                                    disabled={form !== null}
                                    onClick={() => {
                                      close();
                                      openForm(p.project_id, draftOf(p));
                                    }}
                                  >
                                    <Icon of={Pencil} /> Edit
                                  </button>
                                  <button
                                    type="button"
                                    disabled={busy === p.project_id}
                                    onClick={() => {
                                      close();
                                      check(p);
                                    }}
                                  >
                                    <Icon of={ShieldCheck} /> Check access
                                  </button>
                                  <div className="fd-menu-sep" />
                                  <button
                                    type="button"
                                    className="fd-danger"
                                    onClick={() => {
                                      close();
                                      setConfirming(p.project_id);
                                    }}
                                  >
                                    <Icon of={Trash2} /> Delete…
                                  </button>
                                </>
                              )}
                            </Menu>
                          </>
                        )}
                      </td>
                    </tr>
                    {result === undefined ? null : (
                      <tr>
                        <td colSpan={anyTrackers ? 6 : 5}>
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
      )}
    </Page>
  );
}
