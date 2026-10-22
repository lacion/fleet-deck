// R13 P2c (design §5.2, §5C item 7): "Add a wall display". The owner gives a name and the
// stations; the deck makes a one-time link (5 minutes) that this page shows as a URL and a QR code.
import { type FormEvent, useState } from 'react';
import type { DisplayLinkMade, StationView } from '../gen';
import type { SettingsActions } from '../settings/actions';
import type { ElevationAsk } from '../settings/elevation';
import { describe } from '../settings/http';
import { Field } from '../ui/page';
import { QrCode } from '../wall/QrCode';
import type { DisplayActions } from './actions';
import { withElevation } from './elevate';
import { StationChoices } from './StationChoices';

/** The cost of a wall, in one line (§5C item 7). */
export const COST_TEXT =
  'A wall can be read by programs that run as you on its machine: it shows the summaries, the assistant messages and the shown artifact of the stations you choose.';

export function AddDisplay({
  actions,
  settings,
  ask,
  stations,
  onDone,
}: {
  readonly actions: DisplayActions;
  readonly settings: SettingsActions;
  readonly ask: ElevationAsk;
  readonly stations: readonly StationView[];
  readonly onDone: () => void;
}) {
  const [name, setName] = useState('');
  const [ceiling, setCeiling] = useState<readonly string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [made, setMade] = useState<DisplayLinkMade | null>(null);
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
    <section className="fd-panel fd-disp-add" aria-label="Add a wall display">
      <h2 className="fd-form-title fd-disp-title">Add a wall display</h2>
      <p className="fd-note">{COST_TEXT}</p>
      <form className="fd-set-dialogform" onSubmit={submit} aria-label="Add a wall display">
        <Field label="Name">
          <input value={name} maxLength={80} onChange={(e) => setName(e.target.value)} />
        </Field>
        <StationChoices
          stations={stations}
          value={ceiling}
          onChange={setCeiling}
          label="Stations that it may show"
        />
        {error === null ? null : (
          <p className="fd-fielderror" role="alert">
            {error}
          </p>
        )}
        <div className="fd-formactions">
          <button type="submit" disabled={busy}>
            {busy ? 'Making the link…' : 'Make a link'}
          </button>
        </div>
      </form>
      {made === null ? null : (
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
            <button type="button" className="fd-ghost" onClick={() => setMade(null)}>
              Hide
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
