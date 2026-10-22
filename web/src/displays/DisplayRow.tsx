// R13 P2c (design §5.2, §6D item 2): one display of the owner's list: its settings, its binding,
// "Show Mission" and "Revoke". "Save" sends only the keys that changed. Turning `show_diffs` on,
// a new station and a new binding need the owner's elevation (the deck says so; `withElevation`).
import { useId, useState } from 'react';
import { relativeAge } from '../app/sessions';
import type { DisplayView, StationView } from '../gen';
import type { SettingsActions } from '../settings/actions';
import type { ElevationAsk } from '../settings/elevation';
import { describe } from '../settings/http';
import { Field } from '../ui/page';
import type { DisplayActions, DisplayChange } from './actions';
import { withElevation } from './elevate';
import { StationChoices } from './StationChoices';

type Draft = {
  readonly name: string;
  readonly ceiling: readonly string[];
  readonly showDiffs: boolean;
  readonly wake: boolean;
  readonly quiet: string;
  readonly bindStation: string;
  readonly bindMonitor: string;
};

const draftOf = (d: DisplayView): Draft => ({
  name: d.name,
  ceiling: d.ceiling,
  showDiffs: d.show_diffs,
  wake: d.wake_on_needs_you,
  quiet: d.quiet_hours ?? '',
  bindStation: d.binding?.station_id ?? '',
  bindMonitor: d.binding?.monitor ?? '',
});

const sameSet = (a: readonly string[], b: readonly string[]) =>
  a.length === b.length && a.every((x) => b.includes(x));

/** The keys that differ, or the problem with the draft. */
export function changeOf(
  d: DisplayView,
  draft: Draft,
): { readonly change: DisplayChange } | { readonly problem: string } {
  const change: {
    -readonly [K in keyof DisplayChange]: DisplayChange[K];
  } = {};
  if (draft.name.trim() === '') return { problem: 'Give a name.' };
  if (draft.name.trim() !== d.name) change.name = draft.name.trim();
  if (draft.ceiling.length === 0) return { problem: 'Choose at least one station.' };
  if (!sameSet(draft.ceiling, d.ceiling)) change.ceiling = draft.ceiling;
  if (draft.showDiffs !== d.show_diffs) change.show_diffs = draft.showDiffs;
  if (draft.wake !== d.wake_on_needs_you) change.wake_on_needs_you = draft.wake;
  const quiet = draft.quiet.trim();
  if (quiet !== '' && !/^\d{2}:\d{2}-\d{2}:\d{2}$/.test(quiet)) {
    return { problem: 'Quiet hours look like 22:00-07:00.' };
  }
  if (quiet !== (d.quiet_hours ?? '')) change.quiet_hours = quiet === '' ? null : quiet;
  if (draft.bindStation === '') {
    if (d.binding !== null) change.binding = null;
  } else {
    const monitor = draft.bindMonitor.trim();
    if (!/^[A-Za-z0-9._-]{1,64}$/.test(monitor)) return { problem: 'Give a monitor name.' };
    if (d.binding?.station_id !== draft.bindStation || d.binding.monitor !== monitor) {
      change.binding = { station_id: draft.bindStation, monitor };
    }
  }
  return { change };
}

export function DisplayRow({
  display,
  stations,
  actions,
  settings,
  ask,
  onDone,
  now,
}: {
  readonly display: DisplayView;
  readonly stations: readonly StationView[];
  readonly actions: DisplayActions;
  readonly settings: SettingsActions;
  readonly ask: ElevationAsk;
  readonly onDone: () => void;
  readonly now: number;
}) {
  const [draft, setDraft] = useState<Draft>(() => draftOf(display));
  const [base, setBase] = useState(display);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState(false);
  const listId = useId();
  const patch = (next: Partial<Draft>) => setDraft((d) => ({ ...d, ...next }));
  const monitors =
    stations.find((s) => s.station_id === draft.bindStation)?.info?.desktop?.monitors ?? [];
  const run = (what: () => Promise<unknown>, done: string | null) => {
    setBusy(true);
    setError(null);
    setNote(null);
    what().then(
      () => {
        setBusy(false);
        setNote(done);
        onDone();
      },
      (e: unknown) => {
        setBusy(false);
        setError(describe(e));
      },
    );
  };
  const save = () => {
    const out = changeOf(base, draft);
    if ('problem' in out) {
      setError(out.problem);
      return;
    }
    if (Object.keys(out.change).length === 0) {
      setNote('Nothing changed.');
      return;
    }
    setBusy(true);
    setError(null);
    setNote(null);
    withElevation(settings, ask, (elevation) =>
      actions.change(display.display_id, out.change, elevation),
    ).then(
      (shown) => {
        setBusy(false);
        if (shown === null) return;
        setBase(shown);
        setDraft(draftOf(shown));
        setNote('Saved.');
        onDone();
      },
      (e: unknown) => {
        setBusy(false);
        setError(describe(e));
      },
    );
  };
  return (
    <li className="fd-panel fd-disp-row" data-display={display.display_id}>
      <header className="fd-disp-head">
        <h3 className="fd-disp-name">{display.name}</h3>
        <span className="fd-note fd-disp-seen">
          Added {relativeAge(display.created_at, now)} ·{' '}
          {display.last_seen === null
            ? 'never used'
            : `last seen ${relativeAge(display.last_seen, now)}`}
        </span>
      </header>
      <Field label="Name">
        <input
          value={draft.name}
          maxLength={80}
          onChange={(e) => patch({ name: e.target.value })}
        />
      </Field>
      <StationChoices
        stations={stations}
        value={draft.ceiling}
        onChange={(ceiling) => patch({ ceiling })}
        label="Stations that it may show"
      />
      <label className="fd-disp-check">
        <input
          type="checkbox"
          role="switch"
          className="fd-switch"
          aria-checked={draft.showDiffs}
          checked={draft.showDiffs}
          onChange={(e) => patch({ showDiffs: e.target.checked })}
        />{' '}
        Show code changes (patches)
      </label>
      <label className="fd-disp-check">
        <input
          type="checkbox"
          role="switch"
          className="fd-switch"
          aria-checked={draft.wake}
          checked={draft.wake}
          onChange={(e) => patch({ wake: e.target.checked })}
        />{' '}
        Wake the screen when a session needs you
      </label>
      <Field label="Quiet hours" hint="Local time of the bound station. Empty: none.">
        <input
          value={draft.quiet}
          placeholder="22:00-07:00"
          onChange={(e) => patch({ quiet: e.target.value })}
        />
      </Field>
      <div className="fd-disp-bind">
        <Field label="Bound station">
          <select
            value={draft.bindStation}
            onChange={(e) => patch({ bindStation: e.target.value })}
          >
            <option value="">None</option>
            {stations.map((s) => (
              <option key={s.station_id} value={s.station_id}>
                {s.name}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Bound monitor">
          <input
            value={draft.bindMonitor}
            list={listId}
            disabled={draft.bindStation === ''}
            onChange={(e) => patch({ bindMonitor: e.target.value })}
          />
          <datalist id={listId}>
            {monitors.map((m) => (
              <option key={m.name} value={m.name} />
            ))}
          </datalist>
        </Field>
        <button
          type="button"
          className="fd-ghost"
          disabled={busy || draft.bindStation === ''}
          onClick={() => patch({ bindStation: '', bindMonitor: '' })}
        >
          Clear binding
        </button>
      </div>
      {error === null ? null : (
        <p className="fd-fielderror" role="alert">
          {error}
        </p>
      )}
      {note === null ? null : <p role="status">{note}</p>}
      {confirm ? (
        <div className="fd-disp-confirm" role="alertdialog" aria-label="Revoke this display">
          <p>Revoke this display? Its screen stops in 5 seconds.</p>
          <button
            type="button"
            className="fd-danger"
            disabled={busy}
            onClick={() => {
              setConfirm(false);
              run(() => actions.revoke(display.display_id), null);
            }}
          >
            Revoke
          </button>
          <button type="button" className="fd-ghost" onClick={() => setConfirm(false)}>
            Cancel
          </button>
        </div>
      ) : (
        <div className="fd-formactions">
          <button type="button" disabled={busy} onClick={save}>
            Save
          </button>
          <button
            type="button"
            className="fd-ghost"
            disabled={busy}
            onClick={() =>
              run(
                () =>
                  actions.directive(display.display_id, {
                    mode: 'mission',
                    pin: false,
                    filter: null,
                  }),
                'Showing Mission.',
              )
            }
          >
            Show Mission
          </button>
          <button
            type="button"
            className="fd-danger"
            disabled={busy}
            onClick={() => setConfirm(true)}
          >
            Revoke
          </button>
        </div>
      )}
    </li>
  );
}
