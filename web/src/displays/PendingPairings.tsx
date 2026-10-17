// R13 P2c (design §5B item 2, §5C item 1, §5E item 4): the pairings that wait for the owner. A row
// shows the source and the user agent, never the code: the owner types the 6 digits that the wall
// screen shows. A managed `wall.open` launch names its display: the form is locked to it.
import { type FormEvent, useState } from 'react';
import { relativeAge } from '../app/sessions';
import type { DisplayView, PendingPairing, StationView } from '../gen';
import type { SettingsActions } from '../settings/actions';
import type { ElevationAsk } from '../settings/elevation';
import { describe } from '../settings/http';
import { Field } from '../ui/page';
import { useNow } from '../ui/useNow';
import type { DisplayActions } from './actions';
import { withElevation } from './elevate';
import { StationChoices } from './StationChoices';

/** The longest user agent that a row shows. */
const AGENT_MAX = 120;

type Props = {
  readonly pairing: PendingPairing;
  readonly displays: readonly DisplayView[];
  readonly stations: readonly StationView[];
  readonly actions: DisplayActions;
  readonly settings: SettingsActions;
  readonly ask: ElevationAsk;
  readonly onDone: () => void;
  readonly now: number;
};

/** A row also tells the section that it was approved: the reload then removes the row. */
type RowProps = Props & { readonly onApproved: () => void };

function PendingRow({
  pairing,
  displays,
  stations,
  actions,
  settings,
  ask,
  onDone,
  onApproved,
  now,
}: RowProps) {
  const expected = pairing.expected_display_id ?? null;
  const locked = expected !== null && displays.some((d) => d.display_id === expected);
  const [mode, setMode] = useState<'new' | 'existing'>(locked ? 'existing' : 'new');
  const [existing, setExisting] = useState(
    locked ? (expected ?? '') : (displays[0]?.display_id ?? ''),
  );
  const [name, setName] = useState('');
  const [ceiling, setCeiling] = useState<readonly string[]>([]);
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const agent =
    pairing.user_agent.length > AGENT_MAX
      ? `${pairing.user_agent.slice(0, AGENT_MAX)}…`
      : pairing.user_agent;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    if (!/^\d{6}$/.test(code)) {
      setError('The code has 6 digits.');
      return;
    }
    if (mode === 'existing' && existing === '') {
      setError('Choose a display.');
      return;
    }
    if (mode === 'new' && (name.trim() === '' || ceiling.length === 0)) {
      setError('Give a name and choose at least one station.');
      return;
    }
    const what =
      mode === 'existing'
        ? { display_id: existing }
        : {
            name: name.trim(),
            ceiling: stations.map((s) => s.station_id).filter((id) => ceiling.includes(id)),
          };
    setBusy(true);
    setError(null);
    withElevation(settings, ask, (elevation) =>
      actions.approve(pairing.pairing_id, code, what, elevation),
    ).then(
      (shown) => {
        setBusy(false);
        if (shown === null) return;
        setCode('');
        onApproved();
        onDone();
      },
      (e: unknown) => {
        setBusy(false);
        setError(describe(e));
      },
    );
  };
  return (
    <li className="fd-panel fd-disp-pending" data-pairing={pairing.pairing_id}>
      <p className="fd-disp-source">
        <strong>{pairing.source}</strong>{' '}
        <span className="fd-note">{relativeAge(pairing.created_at, now)}</span>
      </p>
      <p className="fd-note">{agent === '' ? 'No user agent.' : agent}</p>
      <form className="fd-set-dialogform" onSubmit={submit} aria-label="Approve this display">
        {locked ? (
          <p className="fd-note">
            This launch belongs to{' '}
            <strong>{displays.find((d) => d.display_id === expected)?.name}</strong>.
          </p>
        ) : (
          <fieldset className="fd-disp-modes">
            <legend className="fd-fieldlabel">Display</legend>
            <label className="fd-disp-check">
              <input
                type="radio"
                name={`mode-${pairing.pairing_id}`}
                checked={mode === 'new'}
                onChange={() => setMode('new')}
              />{' '}
              New display
            </label>
            {displays.length === 0 ? null : (
              <label className="fd-disp-check">
                <input
                  type="radio"
                  name={`mode-${pairing.pairing_id}`}
                  checked={mode === 'existing'}
                  onChange={() => setMode('existing')}
                />{' '}
                Existing display
              </label>
            )}
          </fieldset>
        )}
        {mode === 'existing' ? (
          <Field label="Existing display">
            <select
              value={existing}
              disabled={locked}
              onChange={(e) => setExisting(e.target.value)}
            >
              {displays.map((d) => (
                <option key={d.display_id} value={d.display_id}>
                  {d.name}
                </option>
              ))}
            </select>
          </Field>
        ) : (
          <>
            <Field label="Name">
              <input value={name} maxLength={80} onChange={(e) => setName(e.target.value)} />
            </Field>
            <StationChoices
              stations={stations}
              value={ceiling}
              onChange={setCeiling}
              label="Stations that it may show"
            />
          </>
        )}
        <Field label="Code on the wall screen">
          <input
            value={code}
            inputMode="numeric"
            autoComplete="off"
            maxLength={6}
            pattern="[0-9]{6}"
            onChange={(e) => setCode(e.target.value.replace(/\D/g, ''))}
          />
        </Field>
        {error === null ? null : (
          <p className="fd-fielderror" role="alert">
            {error}
          </p>
        )}
        <div className="fd-formactions">
          <button type="submit" className="fd-primary" disabled={busy}>
            {busy ? 'Approving…' : 'Approve'}
          </button>
        </div>
      </form>
    </li>
  );
}

export function PendingPairings(
  props: Omit<Props, 'pairing' | 'now'> & {
    readonly pending: readonly PendingPairing[];
    readonly now?: number;
  },
) {
  const clock = useNow(props.now, 1000);
  const { pending, now: _given, ...rest } = props;
  // The approved row leaves the list at the next read, so the section keeps the text.
  const [approved, setApproved] = useState(false);
  return (
    <section aria-label="Waiting for a code" className="fd-disp-section">
      <h2 className="fd-form-title">Waiting for a code</h2>
      {approved ? (
        <p role="status">Approved. The wall display signs in within a few seconds.</p>
      ) : null}
      {pending.length === 0 ? (
        <p className="fd-note">No display waits for a code.</p>
      ) : (
        <ul className="fd-disp-list">
          {pending.map((p) => (
            <PendingRow
              key={p.pairing_id}
              {...rest}
              pairing={p}
              now={clock}
              onApproved={() => setApproved(true)}
            />
          ))}
        </ul>
      )}
    </section>
  );
}
