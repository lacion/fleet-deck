// R13 P2c (design §5.2, §5C item 7): "Add a wall display", the panel that opens under the heading
// of the wall displays. The owner gives a name and the stations; the deck makes a one-time link
// (5 minutes) that the panel then shows as a URL and a QR code. Done (or Cancel) closes the panel.
import { type FormEvent, useEffect, useId, useRef, useState } from 'react';
import type { DisplayLinkMade, StationView } from '../gen';
import type { SettingsActions } from '../settings/actions';
import type { ElevationAsk } from '../settings/elevation';
import { describe } from '../settings/http';
import { Field } from '../ui/page';
import { QrCode } from '../wall/QrCode';
import type { DisplayActions } from './actions';
import { withElevation } from './elevate';
import { StationChoices } from './StationChoices';

/** The cost of a wall, in two short sentences (§5C item 7). */
export const COST_TEXT =
  'A wall shows the summaries, assistant messages and shown artifact of the stations you pick. Programs that run as you on its machine can read them.';

export function AddDisplay({
  id,
  actions,
  settings,
  ask,
  stations,
  onDone,
  onClose,
}: {
  /** The id of the panel (the `aria-controls` of the button that opens it). */
  readonly id?: string;
  readonly actions: DisplayActions;
  readonly settings: SettingsActions;
  readonly ask: ElevationAsk;
  readonly stations: readonly StationView[];
  readonly onDone: () => void;
  /** Cancel, or Done after the link: the owner closes the panel. */
  readonly onClose: () => void;
}) {
  const [name, setName] = useState('');
  const [ceiling, setCeiling] = useState<readonly string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [made, setMade] = useState<DisplayLinkMade | null>(null);
  const titleId = useId();
  const nameRef = useRef<HTMLInputElement>(null);
  const doneRef = useRef<HTMLButtonElement>(null);
  // The panel opens on a press of its button: the focus goes to the first field, and to Done when
  // the link shows (the form leaves the panel).
  useEffect(() => {
    (made === null ? nameRef.current : doneRef.current)?.focus();
  }, [made]);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    if (name.trim() === '' || ceiling.length === 0) {
      setError('Give a name and choose at least one station.');
      return;
    }
    setBusy(true);
    setError(null);
    withElevation(settings, ask, (elevation) =>
      actions.makeLink(
        name.trim(),
        stations.filter((s) => ceiling.includes(s.station_id)).map((s) => s.station_id),
        elevation,
      ),
    ).then(
      (link) => {
        setBusy(false);
        if (link === null) return;
        setMade(link);
        setName('');
        setCeiling([]);
        onDone();
      },
      (e: unknown) => {
        setBusy(false);
        setError(describe(e));
      },
    );
  };
  return (
    <section id={id} className="fd-panel fd-disp-add" aria-labelledby={titleId}>
      <h3 className="fd-form-title fd-disp-title" id={titleId}>
        Add a wall display
      </h3>
      {made === null ? (
        <>
          <p className="fd-note">{COST_TEXT}</p>
          <form className="fd-set-dialogform" onSubmit={submit} aria-label="Add a wall display">
            <Field label="Name" layout="row">
              <input
                ref={nameRef}
                value={name}
                maxLength={80}
                onChange={(e) => setName(e.target.value)}
              />
            </Field>
            <StationChoices
              stations={stations}
              value={ceiling}
              onChange={setCeiling}
              label="Stations it shows"
            />
            {error === null ? null : (
              <p className="fd-fielderror" role="alert">
                {error}
              </p>
            )}
            <div className="fd-formactions">
              <button type="button" className="fd-ghost" disabled={busy} onClick={onClose}>
                Cancel
              </button>
              <button type="submit" className="fd-primary" disabled={busy}>
                {busy ? 'Making the link…' : 'Make a pairing link'}
              </button>
            </div>
          </form>
        </>
      ) : (
        <div className="fd-disp-link" role="status">
          <p>Open this link on the wall display. It works once and ends in 5 minutes.</p>
          <code className="fd-disp-url">{made.url}</code>
          <div className="fd-disp-linkactions">
            <button
              type="button"
              className="fd-ghost"
              onClick={() => void navigator.clipboard?.writeText(made.url)}
            >
              Copy
            </button>
            <button type="button" ref={doneRef} onClick={onClose}>
              Done
            </button>
          </div>
          <div className="fd-disp-qr">
            <QrCode url={made.url} />
          </div>
        </div>
      )}
    </section>
  );
}
