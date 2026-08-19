import { useEffect, useRef } from 'react';
import ConfirmDialogFrame from './ConfirmDialogFrame.tsx';

interface KillConfirmProps {
  callsign: string;
  tmuxWindow: string | null | undefined;
  alive: boolean;
  provisioning: boolean;
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}

// v1.8 — the kill confirmation. Killing is the one board action that ends an
// agent mid-thought, so it NEVER happens on a single click: the card's ☠ chip
// and the drawer's "Kill pane" button both land here, and this dialog is the
// only way through. It names the callsign and the tmux window it is about to
// close, and it says plainly what dies (the process) and what does not (the
// worktree, the branch, the code).
//
// Conventions match Compose / LanPanel: the scrim is the backdrop and cancels
// on click, Esc cancels (App's global handler closes this first — see the
// killOpen ref there), and the hazard button is the only affirmative.
export default function KillConfirm({
  callsign,
  tmuxWindow,
  alive,
  provisioning,
  busy,
  onCancel,
  onConfirm,
}: KillConfirmProps) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  // the SAFE choice takes focus on open — a stray ⏎ cancels, never kills
  useEffect(() => {
    cancelRef.current?.focus();
  }, []);

  return (
    <ConfirmDialogFrame
      ariaLabel={`${provisioning ? 'Cancel clone for' : 'Kill'} ${callsign}`}
      busy={busy}
      title={provisioning ? '○ CANCEL CLONE' : '☠ KILL SESSION'}
      titleClassName="haz"
      onCancel={onCancel}
    >
      {provisioning ? (
        <>
          <div className="ask">
            Cancel the repository clone for <b>{callsign}</b>?
          </div>
          <div className="sub">
            FleetDeck stops Git and its credential helpers, removes the temporary checkout, and
            moves the card to OFFLINE. No agent or tmux window has started yet.
          </div>
        </>
      ) : (
        <>
          <div className="ask">
            Kill <b>{callsign}</b> and close its tmux window{' '}
            <span className="win">{(tmuxWindow ?? '') || '(unknown)'}</span>?
          </div>
          <div className="sub">
            The agent’s process dies immediately. Whatever it was doing this turn stops unfinished
            and the card moves to OFFLINE.
          </div>
          <div className="sub">
            Its worktree and branch are left on disk, untouched — no files are deleted, no commits
            are lost, and every uncommitted change stays exactly where it is.
          </div>
        </>
      )}

      {alive && !provisioning && (
        <div className="fd-lanwarn">
          ⚠ This session is not offline — it is still alive. Killing it now forces the pane down
          mid-flight.
        </div>
      )}

      <div className="foot">
        <span className="fd-spacer" />
        <button
          type="button"
          className="fd-ghostbtn"
          ref={cancelRef}
          disabled={busy}
          onClick={onCancel}
        >
          Cancel
        </button>
        <button type="button" className="fd-dangerbtn" disabled={busy} onClick={onConfirm}>
          {busy
            ? provisioning
              ? 'Canceling…'
              : 'Killing…'
            : provisioning
              ? 'Cancel clone'
              : alive
                ? '☠ Force kill'
                : '☠ Kill session'}
        </button>
      </div>
    </ConfirmDialogFrame>
  );
}
