import { useState } from 'react';
import { type StationActions, requestedName } from './pairing';
import { useRun } from './useRun';

type Props = {
  readonly actions: StationActions;
  /** The pasted invitation is a secret: the dialog keeps it in memory and drops it on close. */
  readonly pasted: string;
  readonly setPasted: (text: string) => void;
};

/** This deck dials the station: paste the invitation that the station printed. */
export function ListenView({ actions, pasted, setPasted }: Props) {
  const { busy, error, run } = useRun();
  const [name, setName] = useState('');
  const [added, setAdded] = useState<string | null>(null);
  return (
    <form
      className="fd-add-flow"
      aria-label="Connect to a station that listens"
      onSubmit={(event) => {
        event.preventDefault();
        const text = pasted;
        run(async () => {
          const station = await actions.add({ invitation: text, name: requestedName(name) });
          setPasted('');
          setName('');
          setAdded(station.name);
        });
      }}
    >
      <h3>Connect to a station that listens</h3>
      <p>
        Use this when this deck can reach the station's address. On the station, run{' '}
        <code>fleetdeck station invite</code>. Paste its invitation here.
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
      <div>
        <button type="submit" className="fd-primary" disabled={busy || pasted.trim() === ''}>
          {busy ? 'Adding…' : 'Add station'}
        </button>
      </div>
      {added === null ? null : <p role="status">✓ {added} is added.</p>}
      {error === null ? null : <p role="alert">{error}</p>}
    </form>
  );
}
