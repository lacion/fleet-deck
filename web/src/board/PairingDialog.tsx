import { useState } from 'react';
import { Dialog } from '../integrations/Dialog';
import type { PairingInvitation } from '../gen';
import { useSettings } from '../settings/context';
import { TailnetPairing } from '../tailnet/TailnetPairing';
import { expiresText, requestedName, type StationActions } from './pairing';

type Props = {
  readonly actions: StationActions;
  readonly onClose: () => void;
};

type Direction = 'station_dials' | 'deck_dials';
type Tab = 'invitation' | 'tailnet';

const message = (error: unknown) =>
  error instanceof Error ? error.message : 'The deck did not answer.';

/**
 * Pairs a station in either direction. The invitation text is a secret: it
 * stays in this dialog's memory only and is gone when the dialog closes.
 */
export function PairingDialog({ actions, onClose }: Props) {
  const [direction, setDirection] = useState<Direction>('station_dials');
  const [invitation, setInvitation] = useState<PairingInvitation | null>(null);
  const [pasted, setPasted] = useState('');
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [paired, setPaired] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>('invitation');
  // The tailnet tab is the owner's: the settings context exists for the owner's login only.
  const env = useSettings();

  const run = (work: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    void work()
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(false));
  };

  // Every way out (Close, Escape, an outside click) drops the secrets from memory first.
  const close = () => {
    setInvitation(null);
    setPasted('');
    onClose();
  };

  return (
    <Dialog title="Add station" onClose={close}>
      {env === undefined ? null : (
        <div className="fd-pane-tabs" role="tablist" aria-label="How to add a station">
          {(
            [
              ['invitation', 'Invitation'],
              ['tailnet', 'On your tailnet'],
            ] as const
          ).map(([id, label]) => (
            <button
              key={id}
              type="button"
              role="tab"
              aria-selected={tab === id}
              onClick={() => setTab(id)}
            >
              {label}
            </button>
          ))}
        </div>
      )}
      {env !== undefined && tab === 'tailnet' ? <TailnetPairing env={env} /> : null}
      {env !== undefined && tab === 'tailnet' ? null : (
        <>
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
                    On the station, run <code>{invitation.join_command}</code> and paste the
                    invitation.
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
                On the station, run <code>fleetdeck station invite</code>. Paste its invitation
                here.
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
        </>
      )}
      <button type="button" onClick={close}>
        Close
      </button>
    </Dialog>
  );
}
