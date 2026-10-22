// R13 P2c (design §5.2, §6D item 2): one display of the owner's list: its settings and its
// binding, with "Switch to Mission view" and "Revoke…" in the menu of its heading. Discard and Save
// show only while the draft differs; Save sends only the keys that changed. Turning `show_diffs`
// on, a new station and a new binding need the owner's elevation (the deck says so;
// `withElevation`).
import { useEffect, useId, useRef, useState } from 'react';
import { relativeAge } from '../app/sessions';
import type { DisplayView, MonitorInfo, StationView } from '../gen';
import type { SettingsActions } from '../settings/actions';
import type { ElevationAsk } from '../settings/elevation';
import { describe } from '../settings/http';
import { Ellipsis, Icon, X } from '../ui/icons';
import { Menu } from '../ui/Menu';
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

/** The draft differs from the display: a change to send, or a problem to show at Save. */
export function isDirty(d: DisplayView, draft: Draft): boolean {
  const out = changeOf(d, draft);
  return 'problem' in out || Object.keys(out.change).length > 0;
}

/** An age with "ago": "3 d ago"; "now" stays "now". */
const ago = (at: number, now: number): string => {
  const age = relativeAge(at, now);
  return age === 'now' ? age : `${age} ago`;
};

/** One option text of a monitor: `<name> — <description>`, or only the name. */
const monitorText = (m: MonitorInfo): string =>
  m.description === '' ? m.name : `${m.name} — ${m.description}`;

/** The binding: a station, then its monitor; the X clears both. */
function Binding({
  draft,
  saved,
  stations,
  busy,
  patch,
}: {
  readonly draft: Draft;
  /** The saved binding: a return to its station gives its monitor back. */
  readonly saved: DisplayView['binding'];
  readonly stations: readonly StationView[];
  readonly busy: boolean;
  readonly patch: (next: Partial<Draft>) => void;
}) {
  const stationId = useId();
  const bound = draft.bindStation !== '';
  const monitorsOf = (id: string): readonly MonitorInfo[] =>
    stations.find((s) => s.station_id === id)?.info?.desktop?.monitors ?? [];
  const monitors = monitorsOf(draft.bindStation);
  // A new station: its saved monitor, its only monitor, or none yet.
  const monitorFor = (id: string): string => {
    if (saved !== null && saved.station_id === id) return saved.monitor;
    const list = monitorsOf(id);
    return list.length === 1 ? (list[0]?.name ?? '') : '';
  };
  return (
    <>
      <div className="fd-fieldwrap fd-disp-bindrow">
        <div className="fd-fieldcontrol">
          <label className="fd-fieldlabel" htmlFor={stationId}>
            Bound station
          </label>
          <span className="fd-disp-bindctl">
            <select
              id={stationId}
              value={draft.bindStation}
              onChange={(e) =>
                patch({ bindStation: e.target.value, bindMonitor: monitorFor(e.target.value) })
              }
            >
              <option value="">None</option>
              {stations.map((s) => (
                <option key={s.station_id} value={s.station_id}>
                  {s.name}
                </option>
              ))}
            </select>
            {bound ? (
              <button
                type="button"
                className="fd-iconbtn fd-sm"
                aria-label="Clear binding"
                title="Clear binding"
                disabled={busy}
                onClick={() => patch({ bindStation: '', bindMonitor: '' })}
              >
                <Icon of={X} size={14} />
              </button>
            ) : null}
          </span>
        </div>
      </div>
      {/* No station, no monitor: the row is absent. A station that reports its monitors gives a
          list of them; else the owner types the name. */}
      {!bound ? null : (
        <Field label="Bound monitor">
          {monitors.length === 0 ? (
            <input
              value={draft.bindMonitor}
              onChange={(e) => patch({ bindMonitor: e.target.value })}
            />
          ) : (
            <select
              value={draft.bindMonitor}
              onChange={(e) => patch({ bindMonitor: e.target.value })}
            >
              {draft.bindMonitor === '' ? (
                <option value="" disabled>
                  Choose a monitor
                </option>
              ) : monitors.some((m) => m.name === draft.bindMonitor) ? null : (
                <option value={draft.bindMonitor}>{`${draft.bindMonitor} (not connected)`}</option>
              )}
              {monitors.map((m) => (
                <option key={m.name} value={m.name}>
                  {monitorText(m)}
                </option>
              ))}
            </select>
          )}
        </Field>
      )}
    </>
  );
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
  const [saving, setSaving] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState(false);
  const cancelRef = useRef<HTMLButtonElement>(null);
  // The menu closes when Revoke… opens the confirmation: the focus goes to its safe answer.
  useEffect(() => {
    if (confirm) cancelRef.current?.focus();
  }, [confirm]);
  const patch = (next: Partial<Draft>) => {
    setDraft((d) => ({ ...d, ...next }));
    setNote(null);
  };
  const dirty = isDirty(base, draft);
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
  const discard = () => {
    setDraft(draftOf(base));
    setError(null);
    setNote(null);
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
    setSaving(true);
    setError(null);
    setNote(null);
    withElevation(settings, ask, (elevation) =>
      actions.change(display.display_id, out.change, elevation),
    ).then(
      (shown) => {
        setBusy(false);
        setSaving(false);
        if (shown === null) return;
        setBase(shown);
        setDraft(draftOf(shown));
        setNote('Saved.');
        onDone();
      },
      (e: unknown) => {
        setBusy(false);
        setSaving(false);
        setError(describe(e));
      },
    );
  };
  return (
    <li className="fd-panel fd-disp-row" data-display={display.display_id}>
      <header className="fd-disp-head">
        <h3 className="fd-disp-name">{display.name}</h3>
        <span className="fd-note fd-disp-seen">
          {`Added ${ago(display.created_at, now)} · ${
            display.last_seen === null ? 'never used' : `last seen ${ago(display.last_seen, now)}`
          }`}
        </span>
        <Menu summary={<Icon of={Ellipsis} />} summaryClass="fd-iconbtn" label="Display actions">
          {(close) => (
            <>
              <button
                type="button"
                disabled={busy}
                onClick={() => {
                  close();
                  run(
                    () =>
                      actions.directive(display.display_id, {
                        mode: 'mission',
                        pin: false,
                        filter: null,
                      }),
                    'The wall switched to Mission view.',
                  );
                }}
              >
                Switch to Mission view
              </button>
              <button
                type="button"
                className="fd-danger"
                disabled={busy}
                onClick={() => {
                  close();
                  setConfirm(true);
                }}
              >
                Revoke…
              </button>
            </>
          )}
        </Menu>
      </header>
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
          <button
            type="button"
            className="fd-ghost"
            ref={cancelRef}
            onClick={() => setConfirm(false)}
          >
            Cancel
          </button>
        </div>
      ) : null}
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
      <Field
        label="Show code changes"
        hint="The wall shows the changed lines of a session, not only its changed files."
      >
        <input
          type="checkbox"
          role="switch"
          className="fd-switch"
          aria-checked={draft.showDiffs}
          checked={draft.showDiffs}
          onChange={(e) => patch({ showDiffs: e.target.checked })}
        />
      </Field>
      <Binding draft={draft} saved={base.binding} stations={stations} busy={busy} patch={patch} />
      <Field
        label="Wake the screen"
        hint="Turn on the bound monitor when a session needs you, except in quiet hours."
      >
        <input
          type="checkbox"
          role="switch"
          className="fd-switch"
          aria-checked={draft.wake}
          checked={draft.wake}
          onChange={(e) => patch({ wake: e.target.checked })}
        />
      </Field>
      <Field
        label="Quiet hours"
        hint="Hours when the screen stays dark, for example 22:00-07:00, in the local time of the bound station."
      >
        <input
          value={draft.quiet}
          placeholder="Off"
          onChange={(e) => patch({ quiet: e.target.value })}
        />
      </Field>
      {error === null ? null : (
        <p className="fd-fielderror" role="alert">
          {error}
        </p>
      )}
      {note === null ? null : <p role="status">{note}</p>}
      {dirty ? (
        <div className="fd-formactions">
          <button type="button" className="fd-ghost" disabled={busy} onClick={discard}>
            Discard
          </button>
          <button type="button" disabled={busy} onClick={save}>
            {saving ? 'Saving…' : 'Save'}
          </button>
        </div>
      ) : null}
    </li>
  );
}
