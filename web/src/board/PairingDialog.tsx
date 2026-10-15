import { useEffect, useState } from 'react';
import { Dialog } from '../integrations/Dialog';
import type { PairingInvitation, StationView } from '../gen';
import { type SettingsEnv, useSettings } from '../settings/context';
import { groupDevices } from '../tailnet/model';
import { TailnetPairing } from '../tailnet/TailnetPairing';
import { InviteView } from './InviteView';
import { ListenView } from './ListenView';
import type { StationActions } from './pairing';

type Props = {
  readonly actions: StationActions;
  readonly stations: readonly StationView[];
  readonly onClose: () => void;
};

type View = 'start' | 'tailnet' | 'invite' | 'listen';
/** The tailnet choice: still loading, not available, or the number of addable computers. */
type Count = 'loading' | 'unavailable' | number;

function countText(count: Count): string {
  if (count === 'loading') return 'Checking Tailscale…';
  if (count === 'unavailable') return 'Tailscale is not available on this deck';
  if (count === 0) return 'No computer can be added now';
  return count === 1 ? '1 computer can be added' : `${count} computers can be added`;
}

function Start({
  env,
  onPick,
}: {
  readonly env: SettingsEnv | undefined;
  readonly onPick: (view: View) => void;
}) {
  const [count, setCount] = useState<Count>('loading');
  const tailnet = env?.tailnet;
  // One look at the tailnet, for the number on the card. The tailnet view polls by itself.
  useEffect(() => {
    if (tailnet === undefined) return undefined;
    let alive = true;
    tailnet
      .devices()
      .then(
        (d) =>
          alive && setCount(d.available ? groupDevices(d.devices).addable.length : 'unavailable'),
      )
      .catch(() => alive && setCount('unavailable'));
    return () => {
      alive = false;
    };
  }, [tailnet]);
  return (
    <>
      <h3>Where is the new station?</h3>
      {env === undefined ? null : (
        <button type="button" className="fd-add-choice" onClick={() => onPick('tailnet')}>
          <span className="fd-add-choice-title">A computer on my tailnet</span>
          <span className="fd-add-choice-note">{countText(count)}</span>
        </button>
      )}
      <button type="button" className="fd-add-choice" onClick={() => onPick('invite')}>
        <span className="fd-add-choice-title">Any computer, with a one-time invitation</span>
        <span className="fd-add-choice-note">
          The new station connects to this deck. It works behind a router or a firewall.
        </span>
      </button>
      <details className="fd-add-fold">
        <summary>Other ways</summary>
        <button type="button" className="fd-add-text-btn" onClick={() => onPick('listen')}>
          Connect to a station that listens
        </button>
      </details>
    </>
  );
}

/**
 * Adds a station in one guided flow. The invitation and the pasted text are secrets: they stay in
 * this dialog's memory only and are gone when the dialog closes or the owner goes back.
 */
export function PairingDialog({ actions, stations, onClose }: Props) {
  const [view, setView] = useState<View>('start');
  const [invitation, setInvitation] = useState<PairingInvitation | null>(null);
  const [pasted, setPasted] = useState('');
  // The tailnet part is the owner's: the settings context exists for the owner's login only.
  const env = useSettings();

  const back = () => {
    setInvitation(null);
    setPasted('');
    setView('start');
  };

  // Every way out (Close, Escape, an outside click) drops the secrets from memory first.
  const close = () => {
    setInvitation(null);
    setPasted('');
    onClose();
  };

  return (
    <Dialog title="Add station" onClose={close}>
      {view === 'start' ? null : (
        <div>
          <button type="button" className="fd-add-text-btn" onClick={back}>
            ← Back
          </button>
        </div>
      )}
      {view === 'start' ? <Start env={env} onPick={setView} /> : null}
      {view === 'tailnet' && env !== undefined ? (
        <TailnetPairing env={env} onInvite={() => setView('invite')} />
      ) : null}
      {view === 'invite' ? (
        <InviteView
          actions={actions}
          stations={stations}
          invitation={invitation}
          setInvitation={setInvitation}
        />
      ) : null}
      {view === 'listen' ? (
        <ListenView actions={actions} pasted={pasted} setPasted={setPasted} />
      ) : null}
      <button type="button" onClick={close}>
        Close
      </button>
    </Dialog>
  );
}
