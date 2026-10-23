// R13 P2c (design §5.2, §6D item 2): one display of the owner's list: its settings and its
// binding, with "Switch to Mission view" and "Revoke…" in the menu of its heading. Discard and Save
// show only while the draft differs; Save sends only the keys that changed. Turning `show_diffs`
// on, a new station and a new binding need the owner's elevation (the deck says so;
// `withElevation`). The fields are the settings field rows (§4.7, `Field layout="row"`).
import { useEffect, useId, useRef, useState } from 'react';
import { relativeAge } from '../app/sessions';
import type { DisplayView, MonitorInfo, StationView } from '../gen';
import type { SettingsActions } from '../settings/actions';
import type { ElevationAsk } from '../settings/elevation';
import { describe } from '../settings/http';
import { NoticeIcon } from '../settings/NoticeIcon';
import { monitorLabel, monitorRaw } from '../ui/format';
import { Ellipsis, Icon } from '../ui/icons';
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
  /** `HH:MM-HH:MM`, or `''`: no quiet hours. */
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

/** The quiet hours that the switch turns on when the display has none yet. */
export const QUIET_DEFAULT = '22:00-07:00';

/** The text of a draft whose quiet hours lack a start or an end. */
export const QUIET_PROBLEM = 'Give the quiet hours a start and an end.';

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
  if (quiet !== '' && !/^\d{2}:\d{2}-\d{2}:\d{2}$/.test(quiet)) return { problem: QUIET_PROBLEM };
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

/** The answer of the last action of a row: done, or nothing to do. */
type Note = { readonly text: string; readonly tone: 'ok' | 'info' };

/** A display seen within this time is online: it reads the deck every few seconds. */
export const ONLINE_MS = 2 * 60_000;

/** The link of a display: online, the last time the deck saw it, or never. */
export function seenText(lastSeen: number | null, now: number): string {
  if (lastSeen === null) return 'Never connected';
  return now - lastSeen < ONLINE_MS ? 'Online' : `Last seen ${ago(lastSeen, now)}`;
}

/** The hint of a control that waits for a binding: the stored value in words, then what to do. */
export const UNBOUND_ON = 'On after you bind a station and a monitor.';
export const UNBOUND_OFF = 'Bind a station and a monitor first.';

/**
 * The binding: a station ("None" clears it and its monitor), then the monitor of the station: a
 * name to type, with the monitors that the station reports as suggestions, so that a monitor that
 * is off now (a wall screen) can be bound too.
 */
function Binding({
  draft,
  saved,
  stations,
  patch,
}: {
  readonly draft: Draft;
  /** The saved binding: a return to its station gives its monitor back. */
  readonly saved: DisplayView['binding'];
  readonly stations: readonly StationView[];
  readonly patch: (next: Partial<Draft>) => void;
}) {
  const bound = draft.bindStation !== '';
  const listId = useId();
  const monitorsOf = (id: string): readonly MonitorInfo[] =>
    stations.find((s) => s.station_id === id)?.info?.desktop?.monitors ?? [];
  const monitors = monitorsOf(draft.bindStation);
  // A new station: its saved monitor, its only monitor, or none yet. "None" has no monitor.
  const monitorFor = (id: string): string => {
    if (saved !== null && saved.station_id === id) return saved.monitor;
    const list = monitorsOf(id);
    return list.length === 1 ? (list[0]?.name ?? '') : '';
  };
  return (
    <>
      <Field label="Bound station" layout="row">
        <select
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
      </Field>
      {/* No station, no monitor: the row is absent. The owner types the name; a station that
          reports its monitors suggests them, and a name that it does not report says so. */}
      {!bound ? null : (
        <Field
          label="Bound monitor"
          layout="row"
          hint={
            monitors.length === 0 || draft.bindMonitor.trim() === ''
              ? undefined
              : monitors.some((m) => m.name === draft.bindMonitor.trim())
                ? undefined
                : `${draft.bindMonitor.trim()} is not connected now.`
          }
        >
          <input
            value={draft.bindMonitor}
            list={monitors.length === 0 ? undefined : listId}
            autoComplete="off"
            spellCheck={false}
            onChange={(e) => patch({ bindMonitor: e.target.value })}
          />
          {monitors.length === 0 ? null : (
            <datalist id={listId}>
              {monitors.map((m) => (
                <option key={m.name} value={m.name} label={monitorLabel(m)} title={monitorRaw(m)} />
              ))}
            </datalist>
          )}
        </Field>
      )}
    </>
  );
}

/**
 * Quiet hours: a switch, and while it is on the two times (From, to). Both times write one
 * `HH:MM-HH:MM` text; off writes none. The markup of a `Field layout="row"` with one more row
 * (the times) between the switch and the hint.
 */
function QuietHours({
  value,
  bound,
  onChange,
}: {
  readonly value: string;
  /** A monitor is bound: without one the control waits, and its hint says why. */
  readonly bound: boolean;
  readonly onChange: (next: string) => void;
}) {
  const on = value !== '';
  const disabled = !bound;
  // The times of the last "on": a switch turned off and on again gives them back.
  const [last, setLast] = useState(on ? value : QUIET_DEFAULT);
  const [from = '', to = ''] = value.split('-');
  const set = (next: string) => {
    setLast(next);
    onChange(next);
  };
  return (
    <div className="fd-fieldwrap" data-layout="row">
      <label className="fd-fieldcontrol">
        <span className="fd-fieldlabel">Quiet hours</span>
        <input
          type="checkbox"
          role="switch"
          className="fd-switch"
          aria-checked={on}
          checked={on}
          disabled={disabled}
          onChange={(e) => onChange(e.target.checked ? last : '')}
        />
      </label>
      {on ? (
        <span className="fd-set-inline fd-disp-quiet" role="group" aria-label="Quiet hours">
          <label className="fd-disp-time">
            From
            <input
              type="time"
              step={60}
              value={from}
              disabled={disabled}
              onChange={(e) => set(`${e.target.value}-${to}`)}
            />
          </label>
          <label className="fd-disp-time">
            to
            <input
              type="time"
              step={60}
              value={to}
              disabled={disabled}
              onChange={(e) => set(`${from}-${e.target.value}`)}
            />
          </label>
        </span>
      ) : null}
      <span className="fd-fieldhint">
        {bound
          ? 'The screen stays dark in these hours, in the local time of the bound station.'
          : on
            ? UNBOUND_ON
            : UNBOUND_OFF}
      </span>
    </div>
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
  const [note, setNote] = useState<Note | null>(null);
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
  // Without a bound monitor there is nothing to wake or to keep dark: the two controls wait for a
  // binding, and their stored values stay.
  const bound = draft.bindStation !== '';
  const online = display.last_seen !== null && now - display.last_seen < ONLINE_MS;
  const run = (what: () => Promise<unknown>, done: string | null) => {
    setBusy(true);
    setError(null);
    setNote(null);
    what().then(
      () => {
        setBusy(false);
        setNote(done === null ? null : { text: done, tone: 'ok' });
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
      setNote({ text: 'Nothing changed.', tone: 'info' });
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
        setNote({ text: 'Saved.', tone: 'ok' });
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
        {/* The link state: the station glyph of a link (online) or a hollow ring, then words. */}
        <span className="fd-note fd-disp-seen" title={`Added ${ago(display.created_at, now)}`}>
          <span className="fd-dot" data-status={online ? 'online' : 'idle'} aria-hidden="true" />
          {seenText(display.last_seen, now)}
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
      <Field label="Name" layout="row">
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
        label="Stations it shows"
      />
      <Field
        label="Show code changes"
        layout="row"
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
      <Binding draft={draft} saved={base.binding} stations={stations} patch={patch} />
      <Field
        label="Wake the screen"
        layout="row"
        hint={
          bound
            ? 'Turn on the bound monitor when a session needs you, except in quiet hours.'
            : draft.wake
              ? UNBOUND_ON
              : UNBOUND_OFF
        }
      >
        <input
          type="checkbox"
          role="switch"
          className="fd-switch"
          aria-checked={draft.wake}
          checked={draft.wake}
          disabled={!bound}
          onChange={(e) => patch({ wake: e.target.checked })}
        />
      </Field>
      <QuietHours value={draft.quiet} bound={bound} onChange={(quiet) => patch({ quiet })} />
      {error === null ? null : (
        <p className="fd-fielderror" role="alert">
          {error}
        </p>
      )}
      {note === null ? null : (
        <div className="fd-set-notice" data-tone={note.tone} role="status">
          <NoticeIcon tone={note.tone} />
          <span>{note.text}</span>
        </div>
      )}
      {/* One save look on the Settings page (§2.2): Save is the general primary, as in the save bar
          of the station tabs. Only the changed card shows the two answers. */}
      {dirty ? (
        <div className="fd-formactions">
          <button type="button" className="fd-ghost fd-sm" disabled={busy} onClick={discard}>
            Discard
          </button>
          <button type="button" className="fd-primary fd-sm" disabled={busy} onClick={save}>
            {saving ? 'Saving…' : 'Save'}
          </button>
        </div>
      ) : null}
    </li>
  );
}
