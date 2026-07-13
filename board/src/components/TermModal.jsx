import React, { useRef, useState } from 'react';
import TermPane from './TermPane.jsx';
import { useModal } from '../useModal.js';

// Live terminal onto a board-owned pane (v1.4), full size. The screen and the
// socket live in TermPane, which the grid's tiles share; this is the chrome.
//
// EVERY key goes to the agent — Esc included (Claude's TUI needs it), so the
// modal stops keydown propagation and App's global shortcuts (Esc-to-close,
// j/k/c/y/n…) never fire while it is open. Closing is the ✕ button only.
export default function TermModal({ spawnId, callsign, tmuxWindow, fallbackFocusRef, onClose }) {
  // null | {kind:'exit'|'err'|'close', text} — non-destructive: the terminal
  // stays on screen, frozen, under the strip.
  const [note, setNote] = useState(null);
  const dialogRef = useRef(null);
  // M-A2 (terminal variant) — restore focus to the opener on close, but NO Tab
  // trap and NO initial-focus steal: xterm claims focus itself and Tab must
  // reach the agent (autocomplete), so trapping it would break the terminal.
  // R3-4 — when opened by promoting a grid tile, the ⤢ opener is gone by close;
  // fallbackFocusRef (the header Terminals button) catches focus in that case.
  useModal(dialogRef, { trap: false, initialFocus: false, fallbackRef: fallbackFocusRef });

  return (
    // no backdrop-click close, no Esc close — Esc belongs to the agent's TUI.
    // stopPropagation shields App's window-level shortcuts while typing here.
    <div className="fd-termwrap" role="presentation" onKeyDown={(e) => e.stopPropagation()}>
      <div className="fd-term" role="dialog" aria-modal="true" aria-label={`Live terminal ${callsign || spawnId}`} ref={dialogRef}>
        <div className="fd-termhead">
          <span className="callsign">{callsign || spawnId}</span>
          {tmuxWindow && <span className="fd-panechip">⌗ {tmuxWindow}</span>}
          <span className="fd-termhint">
            ⌨ keystrokes go to the live agent — Esc included · <b>⇧⏎</b> for a newline · close with ✕
          </span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close terminal" onClick={onClose}>✕</button>
        </div>
        <div className="fd-termbody">
          <TermPane spawnId={spawnId} live onNote={setNote} />
          {note && (
            <div className={`fd-termnote ${note.kind}`}>
              <span className="msg">{note.kind === 'err' ? '✗' : '⏻'} {note.text}</span>
              <button type="button" className="fd-ghostbtn" onClick={onClose}>Close</button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
