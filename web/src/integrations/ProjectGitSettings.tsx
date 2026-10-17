// FG-1..FG-4 (design §17.1): the Git sync section of the project form. It has its own Save button
// (`PUT /api/projects/{id}/git`) and sends the four settings of later phases unchanged.
import { useEffect, useState } from 'react';
import type { CleanupMode, GitSettings, GitSettingsPut, Project, PushMode } from '../gen';
import { Field, FormActions, FormGrid } from '../ui/page';
import { useIntegrations } from './context';
import { message } from './useLoad';

/** The fields of the form, named as the struct fields that the deck names in a 400. */
export type GitField =
  | 'sync'
  | 'push'
  | 'push_remote'
  | 'wip_remote'
  | 'default_branch'
  | 'cleanup'
  | 'cleanup_after_hours'
  | 'worktree_include'
  | 'worktree_env'
  | 'remote_url';

export type GitProblems = Partial<Record<GitField, string>>;

export type GitDraft = {
  sync: boolean;
  push: PushMode;
  pushRemote: string;
  wipRemote: string;
  defaultBranch: string;
  cleanup: CleanupMode;
  afterHours: string;
  include: string;
  env: string;
  remoteUrl: string;
};

/** The settings and the revision that the board loaded; the base of a save. */
export type GitBase = { readonly settings: GitSettings; readonly revision: number };

const FIELDS: readonly GitField[] = [
  'sync',
  'push',
  'push_remote',
  'wip_remote',
  'default_branch',
  'cleanup',
  'cleanup_after_hours',
  'worktree_include',
  'worktree_env',
  'remote_url',
];

export const gitDraftOf = (p: Pick<Project, 'git' | 'remote_url'>): GitDraft => ({
  sync: p.git.sync,
  push: p.git.push,
  pushRemote: p.git.push_remote,
  wipRemote: p.git.wip_remote ?? '',
  defaultBranch: p.git.default_branch ?? '',
  cleanup: p.git.cleanup,
  afterHours: String(p.git.cleanup_after_hours),
  include: p.git.worktree_include.join('\n'),
  env: Object.entries(p.git.worktree_env)
    .map(([name, value]) => `${name}=${value}`)
    .join('\n'),
  remoteUrl: p.remote_url ?? '',
});

/** The non-blank lines of a textarea, trimmed. */
const linesOf = (text: string): string[] =>
  text
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l !== '');

/** The body of a save, or the problems that the board finds itself (the deck checks again). */
export function buildGitPut(
  draft: GitDraft,
  base: GitBase,
  loadedRemote: string | null,
): { readonly put: GitSettingsPut } | { readonly problems: GitProblems } {
  const problems: GitProblems = {};
  if (!draft.sync && draft.push !== 'off') problems.push = 'Turn on sync first.';
  if (!draft.sync && draft.cleanup !== 'off') problems.cleanup = 'Turn on sync first.';
  let hours = base.settings.cleanup_after_hours;
  if (draft.cleanup !== 'off') {
    const n = Number(draft.afterHours.trim());
    if (draft.afterHours.trim() === '' || !Number.isInteger(n) || n < 1 || n > 720) {
      problems.cleanup_after_hours = 'Clean up after 1 to 720 hours.';
    } else {
      hours = n;
    }
  }
  const env: Record<string, string> = {};
  for (const line of linesOf(draft.env)) {
    const at = line.indexOf('=');
    if (at <= 0) {
      problems.worktree_env = 'Use NAME=value.';
      break;
    }
    env[line.slice(0, at).trim()] = line.slice(at + 1).trim();
  }
  if (Object.keys(problems).length > 0) return { problems };
  const settings: GitSettings = {
    ...base.settings,
    sync: draft.sync,
    push: draft.push,
    push_remote: draft.pushRemote.trim() === '' ? 'origin' : draft.pushRemote.trim(),
    wip_remote: draft.wipRemote.trim() === '' ? null : draft.wipRemote.trim(),
    default_branch: draft.defaultBranch.trim() === '' ? null : draft.defaultBranch.trim(),
    cleanup: draft.cleanup,
    cleanup_after_hours: hours,
    worktree_include: linesOf(draft.include),
    worktree_env: env,
  };
  const remote = draft.remoteUrl.trim();
  const changed = remote !== '' && remote !== (loadedRemote ?? '');
  return {
    put: {
      settings,
      revision: base.revision,
      ...(changed ? { remote_url: remote } : {}),
    },
  };
}

const isField = (name: string | null): name is GitField =>
  name !== null && (FIELDS as readonly string[]).includes(name);

/** The Git sync section of the form of an existing project (owner only). */
export function ProjectGitSettings({
  projectId,
  initialDraft,
}: {
  readonly projectId: string;
  /** The draft at the first render (tests). */
  readonly initialDraft?: GitDraft;
}) {
  const env = useIntegrations();
  const project = env?.projects?.find((p) => p.project_id === projectId);
  const [base, setBase] = useState<GitBase | null>(
    project === undefined ? null : { settings: project.git, revision: project.git_revision },
  );
  const [draft, setDraft] = useState<GitDraft | null>(
    initialDraft ?? (project === undefined ? null : gitDraftOf(project)),
  );
  const [problems, setProblems] = useState<GitProblems>({});
  const [failure, setFailure] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const [saved, setSaved] = useState(false);
  const [saving, setSaving] = useState(false);
  // The list read again (a save, Reload) brings a new revision: the form starts from it.
  useEffect(() => {
    if (project === undefined || base?.revision === project.git_revision) return;
    setBase({ settings: project.git, revision: project.git_revision });
    setDraft(gitDraftOf(project));
    setProblems({});
    setFailure(null);
    setStale(false);
  }, [project, base?.revision]);
  if (env === undefined || !env.actions.canAdmin || project === undefined) return null;
  if (base === null || draft === null) return null;
  const { actions } = env;
  const set = (patch: Partial<GitDraft>, field: GitField) => {
    setDraft({ ...draft, ...patch });
    setProblems(({ [field]: _gone, ...rest }) => rest);
    setSaved(false);
  };
  const save = () => {
    if (saving) return;
    const built = buildGitPut(draft, base, project.remote_url);
    setSaved(false);
    setFailure(null);
    setStale(false);
    if ('problems' in built) {
      setProblems(built.problems);
      return;
    }
    setProblems({});
    setSaving(true);
    actions
      .saveGit(projectId, built.put)
      .then((result) => {
        switch (result.kind) {
          case 'saved':
            setBase({ settings: result.answer.git, revision: result.answer.git_revision });
            setSaved(true);
            env.reload();
            break;
          case 'invalid':
            if (isField(result.field)) setProblems({ [result.field]: result.message });
            else setFailure(result.message);
            break;
          case 'stale':
            setFailure(result.message);
            setStale(true);
            break;
          case 'rebind':
            setProblems({ remote_url: result.message });
            break;
        }
      })
      .catch((e: unknown) => setFailure(message(e)))
      .finally(() => setSaving(false));
  };
  const off = draft.cleanup === 'off';
  return (
    <form
      aria-label="Git sync"
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <div className="fd-formsec" role="group" aria-label="Git sync">
        <h3 className="fd-label">Git sync</h3>
        <FormGrid>
          <Field
            label="Sync this project across stations"
            hint="Branches get the station name. Fleet Deck fetches before each new worktree."
            span={2}
            error={problems.sync}
          >
            <input
              type="checkbox"
              role="switch"
              className="fd-switch"
              aria-checked={draft.sync}
              checked={draft.sync}
              onChange={(e) => set({ sync: e.target.checked }, 'sync')}
            />
          </Field>
          <Field
            label="Remote URL (optional)"
            hint="An https, ssh or user@host:path URL without a password."
            span={2}
            error={problems.remote_url}
          >
            <input
              value={draft.remoteUrl}
              autoComplete="off"
              onChange={(e) => set({ remoteUrl: e.target.value }, 'remote_url')}
            />
          </Field>
          <Field
            label="Push branches"
            hint="Fleet Deck pushes only fd/<station>/… branches that it made."
            error={problems.push}
          >
            <select
              value={draft.push}
              onChange={(e) => set({ push: e.target.value as PushMode }, 'push')}
            >
              <option value="off">Off</option>
              <option value="on_turn">After each turn</option>
              <option value="on_exit">When the session ends</option>
              {draft.push === 'unknown' ? <option value="unknown">Unknown value</option> : null}
            </select>
          </Field>
          <Field label="Remote" error={problems.push_remote}>
            <input
              value={draft.pushRemote}
              autoComplete="off"
              placeholder="origin"
              onChange={(e) => set({ pushRemote: e.target.value }, 'push_remote')}
            />
          </Field>
          <Field
            label="WIP remote URL (optional)"
            hint="Pushes go here instead of the remote, for example a bare repository on an always-on server."
            span={2}
            error={problems.wip_remote}
          >
            <input
              value={draft.wipRemote}
              autoComplete="off"
              onChange={(e) => set({ wipRemote: e.target.value }, 'wip_remote')}
            />
          </Field>
          <Field
            label="Default branch (optional)"
            hint="Empty: the branch that the remote names."
            error={problems.default_branch}
          >
            <input
              value={draft.defaultBranch}
              autoComplete="off"
              onChange={(e) => set({ defaultBranch: e.target.value }, 'default_branch')}
            />
          </Field>
          <Field
            label="Clean up merged worktrees"
            hint="Moves the worktree to the trash. It never deletes a branch."
            error={problems.cleanup}
          >
            <select
              value={draft.cleanup}
              onChange={(e) => set({ cleanup: e.target.value as CleanupMode }, 'cleanup')}
            >
              <option value="off">Off</option>
              <option value="merged">When merged</option>
              {draft.cleanup === 'unknown' ? <option value="unknown">Unknown value</option> : null}
            </select>
          </Field>
          <Field label="After (hours)" error={problems.cleanup_after_hours}>
            <input
              type="number"
              min={1}
              max={720}
              value={draft.afterHours}
              disabled={off}
              onChange={(e) => set({ afterHours: e.target.value }, 'cleanup_after_hours')}
            />
          </Field>
          <Field
            label="Copy ignored files into new worktrees"
            hint="For example .env. Each file at most 1 MiB."
            span={2}
            error={problems.worktree_include}
          >
            <textarea
              rows={3}
              value={draft.include}
              onChange={(e) => set({ include: e.target.value }, 'worktree_include')}
            />
          </Field>
          <Field
            label="Environment for agents and setup"
            hint="PATH, HOME, LD_*, DYLD_* and provider keys are not allowed."
            span={2}
            error={problems.worktree_env}
          >
            <textarea
              rows={3}
              value={draft.env}
              onChange={(e) => set({ env: e.target.value }, 'worktree_env')}
            />
          </Field>
        </FormGrid>
        <FormActions>
          {failure === null ? null : (
            <span className="fd-formerror" role="alert">
              {failure}
            </span>
          )}
          {saved ? <span role="status">Saved.</span> : null}
          {stale ? (
            <button type="button" className="fd-ghost" onClick={env.reload}>
              Reload
            </button>
          ) : null}
          <button type="submit" className="fd-primary" disabled={saving}>
            {saving ? 'Saving…' : 'Save Git settings'}
          </button>
        </FormActions>
      </div>
    </form>
  );
}
