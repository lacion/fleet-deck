import React, { useEffect, useRef } from 'react';

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
export default function KillConfirm({ callsign, tmuxWindow, alive, busy, onCancel, onConfirm }) {
  const cancelRef = useRef(null);
  // the SAFE choice takes focus on open — a stray ⏎ cancels, never kills
  useEffect(() => { cancelRef.current?.focus(); }, []);

  const cancel = () => { if (!busy) onCancel(); };

  return (
    <div className="fd-composewrap" onClick={cancel}>
      <div
        className="fd-compose fd-killask"
        role="dialog"
        aria-modal="true"
        aria-label={`Kill ${callsign}`}
        onClick={(e) => e.stopPropagation()}
      >
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <span className="lbl haz">☠ KILL SESSION</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Cancel" disabled={busy} onClick={cancel}>✕</button>
        </div>

        <div className="ask">
          Kill <b>{callsign}</b> and close its tmux window <span className="win">{tmuxWindow || '(unknown)'}</span>?
        </div>

        <div className="sub">
          The agent’s process dies immediately. Whatever it was doing this turn stops unfinished and the
          card moves to OFFLINE.
        </div>
        <div className="sub">
          Its worktree and branch are left on disk, untouched — no files are deleted, no commits are lost,
          and every uncommitted change stays exactly where it is.
        </div>

        {alive && (
          <div className="fd-lanwarn">
            ⚠ This session is not offline — it is still alive. Killing it now forces the pane down mid-flight.
          </div>
        )}

        <div className="foot">
          <span className="fd-spacer" />
          <button type="button" className="fd-ghostbtn" ref={cancelRef} disabled={busy} onClick={cancel}>
            Cancel
          </button>
          <button type="button" className="fd-dangerbtn" disabled={busy} onClick={onConfirm}>
            {busy ? 'Killing…' : (alive ? '☠ Force kill' : '☠ Kill session')}
          </button>
        </div>
      </div>
    </div>
  );
}
