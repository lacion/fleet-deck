// R3.2 (DL-164): one form for "New project" and "Edit".
import type { IntegrationsStatus, StationView, TrackerKind } from '../gen';
import { Icon, Plus, X } from '../ui/icons';
import {
  draftProblem,
  FORGE_HOST,
  MAX_TRACKERS,
  type ProjectDraft,
  type TrackerDraft,
} from './model';

type Props = {
  readonly stations: readonly StationView[];
  readonly draft: ProjectDraft;
  readonly onChange: (next: ProjectDraft) => void;
  readonly editing: boolean;
  readonly saving: boolean;
  readonly error: string | null;
  /** The status of the station from the last "Check": the tracker ids it knows. */
  readonly status: IntegrationsStatus | null;
  readonly onSave: () => void;
  readonly onCancel: () => void;
};

const KINDS: readonly TrackerKind[] = ['jira', 'linear'];

export function ProjectForm({
  stations,
  draft,
  onChange,
  editing,
  saving,
  error,
  status,
  onSave,
  onCancel,
}: Props) {
  const problem = draftProblem(draft);
  const set = (patch: Partial<ProjectDraft>) => onChange({ ...draft, ...patch });
  const setTracker = (i: number, patch: Partial<TrackerDraft>) =>
    set({ trackers: draft.trackers.map((t, at) => (at === i ? { ...t, ...patch } : t)) });
  return (
    <form
      className="fd-int-form fd-int-projectform"
      aria-label={editing ? 'Edit project' : 'New project'}
      onSubmit={(e) => {
        e.preventDefault();
        if (problem === null && !saving) onSave();
      }}
    >
      <h3>{editing ? 'Edit project' : 'New project'}</h3>
      <label>
        Name
        <input
          value={draft.name}
          maxLength={80}
          autoComplete="off"
          onChange={(e) => set({ name: e.target.value })}
        />
      </label>
      <label>
        Station
        <select value={draft.station} onChange={(e) => set({ station: e.target.value })}>
          {stations.map((s) => (
            <option key={s.station_id} value={s.station_id}>
              {s.name}
            </option>
          ))}
        </select>
      </label>
      <label>
        Path (a repository on the station)
        <input
          value={draft.path}
          autoComplete="off"
          placeholder="/home/me/code/app"
          onChange={(e) => set({ path: e.target.value })}
        />
      </label>
      <fieldset className="fd-int-fieldset">
        <legend>Forge</legend>
        <label>
          Forge
          <select
            value={draft.forge}
            onChange={(e) => {
              const forge = e.target.value as ProjectDraft['forge'];
              const known = Object.values(FORGE_HOST);
              set({
                forge,
                host:
                  forge === 'none' || known.includes(draft.host) || draft.host === ''
                    ? ''
                    : draft.host,
              });
            }}
          >
            <option value="none">None</option>
            <option value="github">GitHub</option>
            <option value="gitlab">GitLab</option>
          </select>
        </label>
        {draft.forge === 'none' ? null : (
          <>
            <label>
              Host
              <input
                value={draft.host}
                autoComplete="off"
                placeholder={FORGE_HOST[draft.forge]}
                onChange={(e) => set({ host: e.target.value })}
              />
            </label>
            <label>
              Repository (owner/name)
              <input
                value={draft.repo}
                autoComplete="off"
                placeholder="owner/name"
                onChange={(e) => set({ repo: e.target.value })}
              />
            </label>
          </>
        )}
      </fieldset>
      <fieldset className="fd-int-fieldset">
        <legend>Trackers</legend>
        {draft.trackers.length === 0 ? <p className="fd-note">No trackers.</p> : null}
        {draft.trackers.map((t, i) => (
          <div className="fd-int-tracker" key={i} role="group" aria-label={`Tracker ${i + 1}`}>
            <label>
              Tracker id
              <input
                value={t.tracker}
                list="fd-int-tracker-ids"
                autoComplete="off"
                onChange={(e) => setTracker(i, { tracker: e.target.value })}
              />
            </label>
            <label>
              Kind
              <select
                value={t.kind}
                onChange={(e) => setTracker(i, { kind: e.target.value as TrackerKind })}
              >
                {KINDS.map((k) => (
                  <option key={k} value={k}>
                    {k}
                  </option>
                ))}
              </select>
            </label>
            <label>
              Key
              <input
                value={t.key}
                autoComplete="off"
                placeholder="PROJ"
                onChange={(e) => setTracker(i, { key: e.target.value })}
              />
            </label>
            <button
              type="button"
              className="fd-ghost fd-sm"
              aria-label={`Remove tracker ${i + 1}`}
              onClick={() => set({ trackers: draft.trackers.filter((_, at) => at !== i) })}
            >
              <Icon of={X} size={14} /> Remove
            </button>
          </div>
        ))}
        <datalist id="fd-int-tracker-ids">
          {(status?.trackers ?? []).map((t) => (
            <option key={t.id} value={t.id}>
              {t.kind}
            </option>
          ))}
        </datalist>
        <div>
          <button
            type="button"
            className="fd-ghost fd-sm"
            disabled={draft.trackers.length >= MAX_TRACKERS}
            onClick={() =>
              set({ trackers: [...draft.trackers, { tracker: '', kind: 'jira', key: '' }] })
            }
          >
            <Icon of={Plus} size={14} /> Add a tracker
          </button>
        </div>
        <p className="fd-note">
          The tracker id is the id of a <code>[[trackers]]</code> entry in the station config. Press
          Check on a saved project to list the ids that the station knows.
        </p>
      </fieldset>
      <label>
        Prompt template (sessions from tickets)
        <textarea
          rows={4}
          value={draft.promptTemplate}
          maxLength={4000}
          onChange={(e) => set({ promptTemplate: e.target.value })}
        />
      </label>
      <label>
        Review prompt (review sessions)
        <textarea
          rows={4}
          value={draft.reviewPrompt}
          maxLength={4000}
          onChange={(e) => set({ reviewPrompt: e.target.value })}
        />
      </label>
      <p className="fd-note">A prompt left empty uses the default prompt of the deck.</p>
      {error === null ? (
        problem === null ? null : (
          <p className="fd-note">{problem}</p>
        )
      ) : (
        <p className="fd-note fd-int-error" role="alert">
          {error}
        </p>
      )}
      <div className="fd-plan-actions">
        <button type="submit" className="fd-primary" disabled={problem !== null || saving}>
          {saving ? 'Saving…' : 'Save'}
        </button>
        <button type="button" className="fd-ghost" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}
