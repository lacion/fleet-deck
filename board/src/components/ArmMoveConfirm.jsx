import React, { useEffect, useRef, useState } from 'react';
import { useModal } from '../useModal.js';

// v2.0 "Move to tmux" — the confirmation for adopting a session the board did
// NOT spawn into a board-owned tmux pane. One dialog, two copy variants keyed on
// whether the card is still LIVE:
//   · live  → the move is DEFERRED. Two processes can't drive one conversation,
//     so fleetdeck ARMS the move and waits: when you exit this session in your
//     terminal, it resumes it in a board-owned pane. The arm expires in ~30 min.
//   · ended → the move is IMMEDIATE: the ended session resumes in a board-owned
//     pane right now (no exit-your-CLI language — there is no CLI left to exit).
//
// Like SpawnForm's "run unsupervised" (SpawnForm.jsx:46-47,100,311-327), the
// permission bypass is a TWO-STEP gate: checking "run unsupervised" only reveals
// the confirm row; a second, explicit arm checkbox is what puts
// dangerously_skip_permissions:true on the POST. Unchecked — the default, safe
// path — sends {} and permission cards land on this board as usual. While
// revealed-but-unarmed the confirm button is disabled: the dialog never quietly
// downgrades the choice.
//
// Structure + focus-trap mirror KillConfirm: the scrim is the backdrop and
// cancels on click, Esc cancels (App's global handler closes this first — see
// the armOpen ref there), the SAFE (Cancel) button owns initial focus, and the
// affirmative is the only way through. Move-to-tmux is a benign action (it
// resumes a session, it does not end one), so the affirmative is the act-colored
// Send button — not the hazard button kill wears.
export default function ArmMoveConfirm({ callsign, live, busy, onCancel, onConfirm }) {
  const cancelRef = useRef(null);
  const dialogRef = useRef(null);
  const [unsup, setUnsup] = useState(false); // step 1: reveal the confirm
  const [armed, setArmed] = useState(false); // step 2: actually send the flag
  // M-A2 — trap Tab + restore focus on close; the SAFE button owns initial focus.
  useModal(dialogRef, { initialFocus: false });
  // the SAFE choice takes focus on open — a stray ⏎ cancels, never moves
  useEffect(() => { cancelRef.current?.focus(); }, []);

  const cancel = () => { if (!busy) onCancel(); };
  // revealed-but-unarmed refuses to send — exactly like SpawnForm's Spawn button
  const blocked = unsup && !armed;
  const confirm = () => { if (!busy && !blocked) onConfirm(unsup && armed); };

  return (
    <div className="fd-composewrap" onClick={cancel}>
      <div
        className="fd-compose fd-killask fd-armmove"
        role="dialog"
        aria-modal="true"
        aria-label={`Move ${callsign} to tmux`}
        ref={dialogRef}
        onClick={(e) => e.stopPropagation()}
      >
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <span className="lbl">⇥ MOVE TO TMUX</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Cancel" disabled={busy} onClick={cancel}>✕</button>
        </div>

        <div className="ask">
          Move <b>{callsign}</b> into a board-owned tmux pane?
        </div>

        {live ? (
          <>
            <div className="sub">
              This session is still live, and two processes can’t drive one conversation — so the move is
              deferred. <b>When you exit this session in your terminal</b>, fleetdeck resumes it in a
              board-owned tmux pane and the card returns to QUEUED.
            </div>
            <div className="sub">
              The arm expires in ~30 minutes. Nothing happens until you exit — keep working as normal, and
              a <span className="win">/clear</span> won’t trigger it.
            </div>
          </>
        ) : (
          <div className="sub">
            Resume this ended session in a board-owned tmux pane now. Its transcript and cwd are reused, so
            the conversation picks up exactly where it left off; the card returns to QUEUED.
          </div>
        )}

        {/* v1.3-style unsupervised gate: reveal, then arm */}
        <label className="fd-check hazard">
          <input
            type="checkbox"
            checked={unsup}
            disabled={busy}
            onChange={(e) => { setUnsup(e.target.checked); if (!e.target.checked) setArmed(false); }}
          />
          run unsupervised
        </label>
        {unsup && (
          <div className="fd-hazardconfirm">
            <div className="warn">⚠ the moved session will never ask permission for anything</div>
            <div className="sub">no permission cards will ever reach this board for it</div>
            <label className="fd-check hazard">
              <input
                type="checkbox"
                checked={armed}
                disabled={busy}
                onChange={(e) => setArmed(e.target.checked)}
              />
              I understand — arm it
            </label>
          </div>
        )}

        <div className="foot">
          {blocked && (
            <span className="note" style={{ color: 'var(--hazard)' }}>arm the unsupervised confirm — or uncheck it</span>
          )}
          <span className="fd-spacer" />
          <button type="button" className="fd-ghostbtn" ref={cancelRef} disabled={busy} onClick={cancel}>
            Cancel
          </button>
          <button type="button" className="send" disabled={busy || blocked} onClick={confirm}>
            {busy
              ? (live ? 'Arming…' : 'Moving…')
              : (live ? '⇥ Arm move' : '⇥ Move now')}
          </button>
        </div>
      </div>
    </div>
  );
}
