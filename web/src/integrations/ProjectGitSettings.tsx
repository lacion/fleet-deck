// FG-1..FG-4 and FG-5, FG-12 (design §17.1, §8.8): the Git sync section of the project form. It has
// its own Save button. A changed remote URL goes first to `PUT /api/projects/{id}/remote` (a rebind
// asks the owner), then the settings go to `PUT /api/projects/{id}/git`. FG-7, FG-8: the form also
// shows `land`, `check_cmd` and `radar_home`.
import { useEffect, useState } from 'react';
import { stationName } from '../board/fleet';
import type {
  CleanupMode,
  GitSettings,
  GitSettingsPut,
  LandMode,
  Project,
  ProjectHome,
  PushMode,
  StationView,
} from '../gen';
import { Field, FormActions, FormGrid } from '../ui/page';
import { useIntegrations } from './context';
import { Dialog } from './Dialog';
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
  | 'land'
  | 'check_cmd'
  | 'artifact_share'
  | 'radar_home'
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
  /** `''` = the default (null); else `pr`, `merge` or a value of a newer deck. */
  land: string;
  checkCmd: string;
  artifactShare: string;
  /** `''` = the default (null); else a station id. */
  radarHome: string;
  remoteUrl: string;
};

/** The longest check command that the board accepts. */
export const CHECK_CMD_MAX = 4096;

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
  'land',
  'check_cmd',
  'artifact_share',
  'radar_home',
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
  land: p.git.land ?? '',
  checkCmd: p.git.check_cmd ?? '',
  artifactShare: p.git.artifact_share ?? '',
  radarHome: p.git.radar_home ?? '',
  remoteUrl: p.remote_url ?? '',
});

/** The non-blank lines of a textarea, trimmed. */
const linesOf = (text: string): string[] =>
  text
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l !== '');

/** The homes of a project in position order. */
const homesByPosition = (homes: readonly ProjectHome[]): ProjectHome[] =>
  [...homes].sort((a, b) => a.position - b.position);

/**
 * The home of the default radar station (design §10.1): the first home, by position, on a station
 * that reports the label `always_on`, else the first home. `null` without homes.
 */
export function defaultRadarHome(
  homes: readonly ProjectHome[],
  stations: readonly StationView[],
): ProjectHome | null {
  const ordered = homesByPosition(homes);
  const always = ordered.find((h) =>
    stations.find((s) => s.station_id === h.station_id)?.info?.traits?.labels.includes('always_on'),
  );
  return always ?? ordered[0] ?? null;
}

/** The new remote URL of a draft, or `null` when it is empty or equals the loaded one. */
export function remoteChange(draft: GitDraft, loadedRemote: string | null): string | null {
  const remote = draft.remoteUrl.trim();
  return remote !== '' && remote !== (loadedRemote ?? '') ? remote : null;
}

/**
 * The machine of an `artifact_share` that no Fleet Deck station exports, or `null` (design §15.5
 * `not_a_station`): at least one station reports Taildrive, and none has this machine.
 */
export function foreignMachine(
  share: string,
  stations: readonly Pick<StationView, 'info'>[],
): string | null {
  const machine = share.trim().split('/')[0]?.trim() ?? '';
  if (machine === '' || !share.includes('/')) return null;
  const reporting = stations.filter((s) => s.info?.taildrive !== undefined);
  if (reporting.length === 0) return null;
  const known = reporting.some(
    (s) => s.info?.taildrive?.machine?.toLowerCase() === machine.toLowerCase(),
  );
  return known ? null : machine;
}

/** The body of a save, or the problems that the board finds itself (the deck checks again). */
export function buildGitPut(
  draft: GitDraft,
  base: GitBase,
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
  const check = draft.checkCmd.trim();
  if (Array.from(check).length > CHECK_CMD_MAX) problems.check_cmd = 'At most 4096 characters.';
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
    land: draft.land === '' ? null : (draft.land as LandMode),
    check_cmd: check === '' ? null : check,
    artifact_share: draft.artifactShare.trim() === '' ? null : draft.artifactShare.trim(),
    radar_home: draft.radarHome === '' ? null : draft.radarHome,
  };
  return { put: { settings, revision: base.revision } };
}

const isField = (name: string | null): name is GitField =>
  name !== null && (FIELDS as readonly string[]).includes(name);

/** The Git sync section of the form of an existing project (owner only). */
export function ProjectGitSettings({
  projectId,
  stations = [],
  initialDraft,
}: {
  readonly projectId: string;
  /** The stations, for the hint of a share that no station exports (§15.5). */
  readonly stations?: readonly StationView[];
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
  // The remote URL that a save already stored; the project list shows it after the next read.
  const [storedRemote, setStoredRemote] = useState<string | null>(null);
  // The revision after that store; the settings of the same save are sent with it.
  const [storedRevision, setStoredRevision] = useState<number | null>(null);
  // A changed URL that names another repository waits here for the owner's confirmation.
  const [rebind, setRebind] = useState<{ readonly url: string; readonly text: string } | null>(
    null,
  );
  // The list read again (a save, Reload) brings a new revision: the form starts from it.
  useEffect(() => {
    if (project === undefined || base?.revision === project.git_revision) return;
    setBase({ settings: project.git, revision: project.git_revision });
    setDraft(gitDraftOf(project));
    setProblems({});
    setFailure(null);
    setStale(false);
    setStoredRemote(null);
    setStoredRevision(null);
  }, [project, base?.revision]);
  if (env === undefined || !env.actions.canAdmin || project === undefined) return null;
  if (base === null || draft === null) return null;
  const { actions } = env;
  const set = (patch: Partial<GitDraft>, field: GitField) => {
    setDraft({ ...draft, ...patch });
    setProblems(({ [field]: _gone, ...rest }) => rest);
    setSaved(false);
  };
  const finish = (put: GitSettingsPut) =>
    actions.saveGit(projectId, put).then((result) => {
      switch (result.kind) {
        case 'saved':
          setBase({ settings: result.answer.git, revision: result.answer.git_revision });
          setStoredRevision(null);
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
    });
  const save = (confirmed: boolean) => {
    if (saving) return;
    const built = buildGitPut(draft, base);
    setSaved(false);
    setFailure(null);
    setStale(false);
    if ('problems' in built) {
      setProblems(built.problems);
      return;
    }
    setProblems({});
    setSaving(true);
    const url = remoteChange(draft, storedRemote ?? project.remote_url);
    const run = async () => {
      let put = built.put;
      if (url !== null) {
        const remote = await actions.setRemote(projectId, url, confirmed);
        if (remote.kind === 'rebind_needed') {
          setRebind({ url, text: remote.message });
          return;
        }
        if (remote.kind === 'invalid') {
          setProblems({ remote_url: remote.message });
          return;
        }
        setStoredRemote(url);
        // The route raises the revision by one; another value means another save came between.
        if (remote.answer.git_revision !== (storedRevision ?? base.revision) + 1) {
          setFailure('The Git settings changed. Reload and try again.');
          setStale(true);
          return;
        }
        setStoredRevision(remote.answer.git_revision);
        put = { ...put, revision: remote.answer.git_revision };
      } else if (storedRevision !== null) {
        put = { ...put, revision: storedRevision };
      }
      await finish(put);
    };
    run()
      .catch((e: unknown) => setFailure(message(e)))
      .finally(() => setSaving(false));
  };
  const other = foreignMachine(draft.artifactShare, stations);
  const off = draft.cleanup === 'off';
  const homes = homesByPosition(project.homes);
  const radarDefault = defaultRadarHome(project.homes, stations);
  return (
    <>
      <form
        aria-label="Git sync"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          save(false);
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
              label="Remote URL"
              hint="Filled from the origin of the first home. Check it. Fleet Deck compares the exact URL form (user@host:path and ssh://user@host/path are different)."
              span={2}
              error={problems.remote_url}
            >
              <input
                value={draft.remoteUrl}
                autoComplete="off"
                onChange={(e) => set({ remoteUrl: e.target.value }, 'remote_url')}
              />
            </Field>
            {project.remote_ssh_user === null || project.remote_ssh_user === undefined ? null : (
              <p className="fd-note fd-fieldwrap" data-span="2">
                SSH login: {project.remote_ssh_user}
              </p>
            )}
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
                {draft.cleanup === 'unknown' ? (
                  <option value="unknown">Unknown value</option>
                ) : null}
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
            <Field label="Land as" error={problems.land}>
              <select value={draft.land} onChange={(e) => set({ land: e.target.value }, 'land')}>
                <option value="">
                  {project.forge === null ? 'Merge' : 'Pull request'} (default)
                </option>
                <option value="pr">Pull request</option>
                <option value="merge">Merge</option>
                {draft.land === 'unknown' ? <option value="unknown">Unknown value</option> : null}
              </select>
            </Field>
            <Field
              label="Check command before landing (optional)"
              hint="Runs on the station only when allow_check_cmd = true."
              span={2}
              error={problems.check_cmd}
            >
              <input
                value={draft.checkCmd}
                autoComplete="off"
                onChange={(e) => set({ checkCmd: e.target.value }, 'check_cmd')}
              />
            </Field>
            <Field
              label="Artifact share (optional)"
              hint="A Taildrive share as <machine>/<share>."
              span={2}
              error={problems.artifact_share}
            >
              <input
                value={draft.artifactShare}
                autoComplete="off"
                placeholder="nas/artifacts"
                onChange={(e) => set({ artifactShare: e.target.value }, 'artifact_share')}
              />
            </Field>
            {other === null ? null : (
              <p className="fd-note fd-fieldwrap" data-span="2">
                Fleet Deck cannot check shares on {other}, because it does not run a Fleet Deck
                station.
              </p>
            )}
            {radarDefault === null ? null : (
              <Field
                label="Radar station"
                hint="This station checks the branches for conflicts every 5 minutes."
                error={problems.radar_home}
              >
                <select
                  value={draft.radarHome}
                  onChange={(e) => set({ radarHome: e.target.value }, 'radar_home')}
                >
                  <option value="">
                    {stationName(radarDefault.station_id, stations)} (default)
                  </option>
                  {homes.map((h) => (
                    <option key={h.station_id} value={h.station_id}>
                      {stationName(h.station_id, stations)}
                    </option>
                  ))}
                </select>
              </Field>
            )}
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
      {rebind === null ? null : (
        <Dialog title={`Rebind the project to ${rebind.url}?`} onClose={() => setRebind(null)}>
          <p className="fd-note">{rebind.text}</p>
          <div className="fd-pane-row">
            <button
              type="button"
              className="fd-primary"
              onClick={() => {
                setRebind(null);
                save(true);
              }}
            >
              Rebind
            </button>
            <button type="button" className="fd-ghost" onClick={() => setRebind(null)}>
              Cancel
            </button>
          </div>
        </Dialog>
      )}
    </>
  );
}
