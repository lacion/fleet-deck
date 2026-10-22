import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { Dialog } from '../integrations/Dialog';
import type { PairingInvitation, StationView } from '../gen';
import { type SettingsEnv, useSettings } from '../settings/context';
import { groupDevices } from '../tailnet/model';
import { TailnetPairing } from '../tailnet/TailnetPairing';
import { InviteView } from './InviteView';
import { ListenView } from './ListenView';
import type { StationActions } from './pairing';
import { ChevronLeft, ChevronRight, Globe, Icon, type LucideIcon, Send, X } from '../ui/icons';

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
  // The cause and the next step (§5): the tailnet view lists each computer and why it is not free.
  if (count === 0) return 'No computer on your tailnet is free to add. Choose it to see why.';
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
  // A tailnet without a computer to add is a quiet choice under the invitation (it still opens
  // the tailnet view, which says why).
  const dim = count === 0 || count === 'unavailable';
  // The move remounts the tailnet choice; when it had the focus, the focus goes to the first
  // choice, so that Escape and Tab still work inside the dialog.
  const choices = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (dim && document.activeElement === document.body) {
      choices.current?.querySelector<HTMLElement>('button')?.focus();
    }
  }, [dim]);
  const tailnetChoice =
    env === undefined ? null : (
      <Choice
        icon={Globe}
        title="A computer on my tailnet"
        note={countText(count)}
        dim={dim}
        onClick={() => onPick('tailnet')}
      />
    );
  return (
    <>
      <h3 className="fd-add-question">Where is the new station?</h3>
      <div className="fd-add-choices" ref={choices}>
        {dim ? null : tailnetChoice}
        <Choice
          icon={Send}
          title="Any computer, with a one-time invitation"
          note="The new station connects to this deck. It works behind a router or a firewall."
          onClick={() => onPick('invite')}
        />
        {dim ? tailnetChoice : null}
      </div>
      <details className="fd-add-fold">
        <summary>Other ways</summary>
        <button type="button" className="fd-add-text-btn" onClick={() => onPick('listen')}>
          Connect to a station that listens
        </button>
      </details>
    </>
  );
}

/** One way to add a station: an icon, the title and a note, and a chevron (none when dim). */
function Choice({
  icon,
  title,
  note,
  dim = false,
  onClick,
}: {
  readonly icon: LucideIcon;
  readonly title: string;
  readonly note: string;
  readonly dim?: boolean;
  readonly onClick: () => void;
}) {
  return (
    <button
      type="button"
      className="fd-add-choice"
      data-dim={dim ? 'true' : undefined}
      onClick={onClick}
    >
      <span className="fd-add-choice-icon">
        <Icon of={icon} />
      </span>
      <span className="fd-add-choice-text">
        <span className="fd-add-choice-title">{title}</span>
        <span className="fd-add-choice-note">{note}</span>
      </span>
      {dim ? null : <Icon of={ChevronRight} />}
    </button>
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
      <div className="fd-add-dialog">
        {view === 'start' ? null : (
          <div className="fd-add-back">
            <button type="button" className="fd-ghost fd-sm" onClick={back}>
              <Icon of={ChevronLeft} />
              Back
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
        <button
          type="button"
          className="fd-iconbtn fd-add-close"
          aria-label="Close"
          onClick={close}
        >
          <Icon of={X} />
        </button>
      </div>
    </Dialog>
  );
}
