import { useState } from 'react';
import { useOverlay } from '../app/overlay';
import type { PairingInvitation } from '../gen';
import { expiresText, requestedName, type StationActions } from './pairing';

type Props = {
  readonly actions: StationActions;
  readonly onClose: () => void;
};

type Direction = 'station_dials' | 'deck_dials';

const message = (error: unknown) =>
  error instanceof Error ? error.message : 'The deck did not answer.';

/**
 * Pairs a station in either direction. The invitation text is a secret: it
 * stays in this dialog's memory only and is gone when the dialog closes.
 */
export function PairingDialog({ actions, onClose }: Props) {
  useOverlay();
  const [direction, setDirection] = useState<Direction>('station_dials');
  const [invitation, setInvitation] = useState<PairingInvitation | null>(null);
  const [pasted, setPasted] = useState('');
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [paired, setPaired] = useState<string | null>(null);

  const run = (work: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    void work()
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(false));
  };

  return (
    <div className="fd-dialog" role="dialog" aria-modal="true" aria-labelledby="fd-pair-title">
      <h3 id="fd-pair-title">Add station</h3>
      <fieldset>
        <legend>Which side dials?</legend>
        <label>
          <input
            type="radio"
            name="fd-direction"
            checked={direction === 'station_dials'}
            onChange={() => setDirection('station_dials')}
          />
          The station dials this deck (station behind NAT)
        </label>
        <label>
          <input
            type="radio"
            name="fd-direction"
            checked={direction === 'deck_dials'}
            onChange={() => setDirection('deck_dials')}
          />
          This deck dials the station (station listens)
        </label>
      </fieldset>
      {direction === 'station_dials' ? (
        <section aria-label="Invite a station">
          <p>Create a one-time invitation. Then run the join command on the station.</p>
          <button
            type="button"
            disabled={busy}
            onClick={() =>
              run(async () => {
                setInvitation(await actions.invite());
              })
            }
          >
            {invitation === null ? 'Create invitation' : 'Create a new invitation'}
          </button>
          {invitation === null ? null : (
            <div className="fd-invitation">
              <label htmlFor="fd-invitation-text">Invitation (secret, single use)</label>
              <textarea
                id="fd-invitation-text"
                readOnly
                rows={3}
                value={invitation.invitation}
                onFocus={(event) => event.currentTarget.select()}
              />
              <p>
                On the station, run <code>{invitation.join_command}</code> and paste the invitation.
              </p>
              <p>Expires: {expiresText(invitation.expires_at)}</p>
            </div>
          )}
        </section>
      ) : (
        <form
          aria-label="Pair a listening station"
          onSubmit={(event) => {
            event.preventDefault();
            const text = pasted;
            run(async () => {
              const station = await actions.add({
                invitation: text,
                name: requestedName(name),
              });
              setPasted('');
              setName('');
              setPaired(station.name);
            });
          }}
        >
          <p>
            On the station, run <code>fleetdeck station invite</code>. Paste its invitation here.
          </p>
          <label htmlFor="fd-station-invitation">Station invitation</label>
          <textarea
            id="fd-station-invitation"
            required
            rows={3}
            autoComplete="off"
            spellCheck={false}
            value={pasted}
            onChange={(event) => setPasted(event.target.value)}
          />
          <label htmlFor="fd-station-name">Name on this deck (optional)</label>
          <input
            id="fd-station-name"
            value={name}
            maxLength={64}
            placeholder="The station's own name"
            onChange={(event) => setName(event.target.value)}
          />
          <button type="submit" disabled={busy || pasted.trim() === ''}>
            {busy ? 'Pairing…' : 'Pair station'}
          </button>
        </form>
      )}
      {paired === null ? null : <p role="status">Paired station {paired}.</p>}
      {error === null ? null : <p role="alert">{error}</p>}
      <button
        type="button"
        onClick={() => {
          setInvitation(null);
          setPasted('');
          onClose();
        }}
      >
        Close
      </button>
    </div>
  );
}
