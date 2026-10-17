import { type FormEvent, useState } from 'react';
import type { DeckSession, StationView } from '../gen';
import { spawnModes } from '../spawn/spawn';
import { FolderPicker, type ListWorkspaces } from '../ui/FolderPicker';
import { Field, FormActions, FormGrid, fieldError } from '../ui/page';
import {
  CUSTOM,
  type Draft,
  draftProblem,
  emptyDraft,
  MAX_PRECHECK,
  MAX_PROMPT,
  PRESETS,
} from './automations';

type Props = {
  readonly stations: readonly StationView[];
  readonly initial: Draft;
  /** The deck refused the last save; shown in the form actions. */
  readonly error: string | null;
  readonly saving: boolean;
  readonly editing: boolean;
  readonly onSave: (draft: Draft) => void;
  readonly onCancel: () => void;
  /** Reads a folder of the station (the folder picker); absent: the folder is typed only. */
  readonly list?: ListWorkspaces | undefined;
  readonly sessions?: readonly DeckSession[] | undefined;
  /** The errors show at the first render, as after a submit try (tests). */
  readonly initialTried?: boolean;
};

/** The label of a provider instance: `claude (personal)` when they differ, else the provider. */
export const providerOption = (p: { provider: string; instance: string }): string =>
  p.instance === p.provider ? p.provider : `${p.provider} (${p.instance})`;

/** A draft that passes every rule: one field is checked on this base. */
const SOUND: Draft = {
  ...emptyDraft('s', 'i'),
  name: 'x',
  path: '/p',
  prompt: 'x',
};

type FieldName =
  | 'name'
  | 'prompt'
  | 'station'
  | 'path'
  | 'instance'
  | 'cron'
  | 'timezone'
  | 'precheck'
  | 'graceMinutes';

/** The rule of one field of the draft, or `null`. */
function problemOf(field: FieldName, d: Draft): string | null {
  switch (field) {
    case 'cron':
      return draftProblem({ ...SOUND, preset: d.preset, cron: d.cron });
    default:
      return draftProblem({ ...SOUND, [field]: d[field] });
  }
}

/** The form of one automation (R5, DL-171): what runs, where, when, and the safety rules. */
export function ScheduleForm({
  stations,
  initial,
  error,
  saving,
  editing,
  onSave,
  onCancel,
  list,
  sessions,
  initialTried = false,
}: Props) {
  const [draft, setDraft] = useState<Draft>(initial);
  const [touched, setTouched] = useState<ReadonlySet<FieldName>>(new Set());
  const [tried, setTried] = useState(initialTried);
  const touch = (field: FieldName) => setTouched((cur) => new Set(cur).add(field));
  const shown = (field: FieldName) =>
    fieldError(problemOf(field, draft), touched.has(field), tried);
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
    if (saving) return;
    if (problem === null) onSave(draft);
    else setTried(true);
  };
  return (
    <div className="fd-panel fd-formpanel">
      <form aria-label="Automation" noValidate onSubmit={submit}>
        <h2 className="fd-form-title">{editing ? 'Edit automation' : 'New automation'}</h2>
        <div className="fd-formsec fd-formsec-first" role="group" aria-label="What">
          <h3 className="fd-label">What</h3>
          <FormGrid>
            <Field label="Name" error={shown('name')} onBlur={() => touch('name')}>
              <input value={draft.name} onChange={(e) => set('name', e.target.value)} />
            </Field>
            <Field label="Prompt" span={2} error={shown('prompt')} onBlur={() => touch('prompt')}>
              <textarea
                rows={5}
                value={draft.prompt}
                maxLength={MAX_PROMPT}
                onChange={(e) => set('prompt', e.target.value)}
              />
            </Field>
          </FormGrid>
          <label className="fd-checkrow">
            <input
              type="checkbox"
              checked={draft.reuse}
              onChange={(e) => set('reuse', e.target.checked)}
            />
            Reuse the session of the last run when it is idle (the prompt goes to it as a follow-up)
          </label>
        </div>
        <div className="fd-formsec" role="group" aria-label="Where">
          <h3 className="fd-label">Where</h3>
          <FormGrid>
            <Field label="Station" error={shown('station')} onBlur={() => touch('station')}>
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
            </Field>
            <Field label="Provider" error={shown('instance')} onBlur={() => touch('instance')}>
              <select
                value={draft.instance}
                onChange={(e) => setDraft((d) => ({ ...d, instance: e.target.value, mode: '' }))}
              >
                <option value="">Choose a provider</option>
                {providers.map((p) => (
                  <option key={p.instance} value={p.instance}>
                    {providerOption(p)}
                  </option>
                ))}
              </select>
            </Field>
            <Field
              label="Repository folder on the station"
              span={2}
              group
              hint="Each run works in a new worktree of this folder, unless it reuses the last session."
              error={shown('path')}
              onBlur={() => touch('path')}
            >
              <FolderPicker
                stationId={draft.station}
                value={draft.path}
                onChange={(path) => set('path', path)}
                list={list}
                sessions={sessions}
                label="Repository folder on the station"
              />
            </Field>
            <Field label="Model (optional)">
              <input
                value={draft.model}
                spellCheck={false}
                onChange={(e) => set('model', e.target.value)}
              />
            </Field>
          </FormGrid>
        </div>
        <div className="fd-formsec" role="group" aria-label="When">
          <h3 className="fd-label">When</h3>
          <FormGrid>
            <Field label="Schedule">
              <select value={draft.preset} onChange={(e) => set('preset', e.target.value)}>
                {PRESETS.map((p) => (
                  <option key={p.value} value={p.value}>
                    {p.label} ({p.value})
                  </option>
                ))}
                <option value={CUSTOM}>Cron expression</option>
              </select>
            </Field>
            {draft.preset === CUSTOM ? (
              <Field
                label="Cron (minute hour day month weekday)"
                error={shown('cron')}
                onBlur={() => touch('cron')}
              >
                <input
                  value={draft.cron}
                  spellCheck={false}
                  placeholder="30 2 * * 1-5"
                  onChange={(e) => set('cron', e.target.value)}
                />
              </Field>
            ) : null}
            <Field
              label="Time zone"
              hint="UTC or a fixed offset such as +02:00. A fixed offset does not follow daylight saving time."
              error={shown('timezone')}
              onBlur={() => touch('timezone')}
            >
              <input
                value={draft.timezone}
                spellCheck={false}
                onChange={(e) => set('timezone', e.target.value)}
              />
            </Field>
            <Field
              label="Grace period (minutes)"
              hint="A run that the deck could not start within this time after its due time is recorded as missed."
              error={shown('graceMinutes')}
              onBlur={() => touch('graceMinutes')}
            >
              <input
                inputMode="numeric"
                value={draft.graceMinutes}
                onChange={(e) => set('graceMinutes', e.target.value)}
              />
            </Field>
          </FormGrid>
          <label className="fd-checkrow">
            <input
              type="checkbox"
              checked={draft.enabled}
              onChange={(e) => set('enabled', e.target.checked)}
            />
            Enabled
          </label>
        </div>
        <div className="fd-formsec" role="group" aria-label="Safety">
          <h3 className="fd-label">Safety</h3>
          <FormGrid>
            <Field label="Permission mode">
              <select value={draft.mode} onChange={(e) => set('mode', e.target.value)}>
                <option value="">Station default</option>
                {modes.map((m) => (
                  <option key={m} value={m}>
                    {m}
                  </option>
                ))}
              </select>
            </Field>
            <Field
              label="Run first (optional shell check on the station; a non-zero exit skips the run)"
              span={2}
              hint={
                canPrecheck || station === undefined
                  ? 'Runs with /bin/sh on the station in the folder, with 60 seconds. A non-zero exit records a skipped run and starts nothing.'
                  : 'This station does not run shell commands (allow_setup_cmd = false in station.toml).'
              }
              error={shown('precheck')}
              onBlur={() => touch('precheck')}
            >
              <textarea
                rows={2}
                spellCheck={false}
                autoCapitalize="off"
                maxLength={MAX_PRECHECK}
                value={draft.precheck}
                disabled={!canPrecheck && draft.precheck === ''}
                onChange={(e) => set('precheck', e.target.value)}
              />
            </Field>
          </FormGrid>
        </div>
        <FormActions>
          {error === null ? null : (
            <span className="fd-formerror" role="alert">
              {error}
            </span>
          )}
          <button type="button" className="fd-ghost" onClick={onCancel}>
            Cancel
          </button>
          <button type="submit" className="fd-primary" disabled={saving}>
            {saving ? 'Saving…' : editing ? 'Save changes' : 'Create automation'}
          </button>
        </FormActions>
      </form>
    </div>
  );
}
