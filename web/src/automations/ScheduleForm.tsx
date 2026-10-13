import { type FormEvent, useState } from 'react';
import type { StationView } from '../gen';
import { spawnModes } from '../spawn/spawn';
import { CUSTOM, type Draft, draftProblem, MAX_PRECHECK, MAX_PROMPT, PRESETS } from './automations';

type Props = {
  readonly stations: readonly StationView[];
  readonly initial: Draft;
  /** The deck refused the last save; shown above the buttons. */
  readonly error: string | null;
  readonly saving: boolean;
  readonly editing: boolean;
  readonly onSave: (draft: Draft) => void;
  readonly onCancel: () => void;
};

/** The form of one automation (R5): what runs, where, when, and the precheck. */
export function ScheduleForm({
  stations,
  initial,
  error,
  saving,
  editing,
  onSave,
  onCancel,
}: Props) {
  const [draft, setDraft] = useState<Draft>(initial);
  const set = <K extends keyof Draft>(key: K, value: Draft[K]) =>
    setDraft((d) => ({ ...d, [key]: value }));
  const station = stations.find((s) => s.station_id === draft.station);
  const providers = station?.info?.providers ?? [];
  const provider = providers.find((p) => p.instance === draft.instance);
  const modes = spawnModes(provider);
  const canPrecheck = station?.info?.allow_setup_cmd === true;
  const problem = draftProblem(draft);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (problem === null) onSave(draft);
  };
  return (
    <div className="fd-spawn">
      <form aria-label="Automation" onSubmit={submit}>
        <div className="fd-panel">
          <h3 className="fd-label">What</h3>
          <label>
            Name
            <input value={draft.name} onChange={(e) => set('name', e.target.value)} />
          </label>
          <label>
            Prompt
            <textarea
              rows={5}
              value={draft.prompt}
              maxLength={MAX_PROMPT}
              onChange={(e) => set('prompt', e.target.value)}
            />
          </label>
          <label className="fd-spawn-opt">
            <input
              type="checkbox"
              checked={draft.reuse}
              onChange={(e) => set('reuse', e.target.checked)}
            />
            Reuse the session of the last run when it is idle (the prompt goes to it as a follow-up)
          </label>
        </div>
        <div className="fd-panel">
          <h3 className="fd-label">Where</h3>
          <label>
            Station
            <select
              value={draft.station}
              onChange={(e) => {
                const next = stations.find((s) => s.station_id === e.target.value);
                setDraft((d) => ({
                  ...d,
                  station: e.target.value,
                  instance: next?.info?.providers[0]?.instance ?? '',
                  mode: '',
                }));
              }}
            >
              <option value="">Choose a station</option>
              {stations.map((s) => (
                <option key={s.station_id} value={s.station_id}>
                  {s.name}
                  {s.reachable ? '' : ' (not linked)'}
                </option>
              ))}
            </select>
          </label>
          <label>
            Repository folder on the station
            <input
              value={draft.path}
              spellCheck={false}
              placeholder="/home/me/projects/app"
              onChange={(e) => set('path', e.target.value)}
            />
          </label>
          <p className="fd-note">
            Each run works in a new worktree of this folder, unless it reuses the last session.
          </p>
          <label>
            Provider
            <select
              value={draft.instance}
              onChange={(e) => setDraft((d) => ({ ...d, instance: e.target.value, mode: '' }))}
            >
              <option value="">Choose a provider</option>
              {providers.map((p) => (
                <option key={p.instance} value={p.instance}>
                  {p.instance} · {p.provider}
                </option>
              ))}
            </select>
          </label>
          <label>
            Permission mode
            <select value={draft.mode} onChange={(e) => set('mode', e.target.value)}>
              <option value="">Station default</option>
              {modes.map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
          </label>
          <label>
            Model (optional)
            <input
              value={draft.model}
              spellCheck={false}
              onChange={(e) => set('model', e.target.value)}
            />
          </label>
        </div>
        <div className="fd-panel">
          <h3 className="fd-label">When</h3>
          <label>
            Schedule
            <select value={draft.preset} onChange={(e) => set('preset', e.target.value)}>
              {PRESETS.map((p) => (
                <option key={p.value} value={p.value}>
                  {p.label} ({p.value})
                </option>
              ))}
              <option value={CUSTOM}>Cron expression</option>
            </select>
          </label>
          {draft.preset === CUSTOM ? (
            <label>
              Cron (minute hour day month weekday)
              <input
                value={draft.cron}
                spellCheck={false}
                placeholder="30 2 * * 1-5"
                onChange={(e) => set('cron', e.target.value)}
              />
            </label>
          ) : null}
          <label>
            Time zone
            <input
              value={draft.timezone}
              spellCheck={false}
              onChange={(e) => set('timezone', e.target.value)}
            />
          </label>
          <p className="fd-note">
            UTC or a fixed offset such as +02:00. A fixed offset does not follow daylight saving
            time.
          </p>
          <label>
            Grace period (minutes)
            <input
              inputMode="numeric"
              value={draft.graceMinutes}
              onChange={(e) => set('graceMinutes', e.target.value)}
            />
          </label>
          <p className="fd-note">
            A run that the deck could not start within this time after its due time is recorded as
            missed.
          </p>
          <label className="fd-spawn-opt">
            <input
              type="checkbox"
              checked={draft.enabled}
              onChange={(e) => set('enabled', e.target.checked)}
            />
            Enabled
          </label>
        </div>
        <div className="fd-panel">
          <h3 className="fd-label">Precheck (optional)</h3>
          <label>
            Shell command
            <textarea
              rows={2}
              spellCheck={false}
              autoCapitalize="off"
              maxLength={MAX_PRECHECK}
              value={draft.precheck}
              disabled={!canPrecheck && draft.precheck === ''}
              onChange={(e) => set('precheck', e.target.value)}
            />
          </label>
          <p className="fd-note">
            {canPrecheck || station === undefined
              ? 'Runs with /bin/sh on the station in the folder, with 60 seconds. A non-zero exit records a skipped run and starts nothing.'
              : 'This station does not run shell commands (allow_setup_cmd = false in station.toml).'}
          </p>
        </div>
        {error === null ? null : <p role="alert">{error}</p>}
        {problem === null ? null : <p className="fd-note">{problem}</p>}
        <div className="fd-plan-actions">
          <button type="submit" disabled={problem !== null || saving}>
            {saving ? 'Saving…' : editing ? 'Save changes' : 'Create automation'}
          </button>
          <button type="button" className="fd-ghost" onClick={onCancel}>
            Cancel
          </button>
        </div>
      </form>
    </div>
  );
}
