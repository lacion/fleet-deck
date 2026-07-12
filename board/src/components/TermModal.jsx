import React, { useState } from 'react';
import TermPane from './TermPane.jsx';

// Live terminal onto a board-owned pane (v1.4), full size. The screen and the
// socket live in TermPane, which the grid's tiles share; this is the chrome.
//
// EVERY key goes to the agent — Esc included (Claude's TUI needs it), so the
// modal stops keydown propagation and App's global shortcuts (Esc-to-close,
// j/k/c/y/n…) never fire while it is open. Closing is the ✕ button only.
export default function TermModal({ spawnId, callsign, tmuxWindow, onClose }) {
  // null | {kind:'exit'|'err'|'close', text} — non-destructive: the terminal
  // stays on screen, frozen, under the strip.
  const [note, setNote] = useState(null);

  return (
    // no backdrop-click close, no Esc close — Esc belongs to the agent's TUI.
    // stopPropagation shields App's window-level shortcuts while typing here.
    <div className="fd-termwrap" role="presentation" onKeyDown={(e) => e.stopPropagation()}>
      <div className="fd-term" role="dialog" aria-label={`Live terminal ${callsign || spawnId}`}>
        <div className="fd-termhead">
          <span className="callsign">{callsign || spawnId}</span>
          {tmuxWindow && <span className="fd-panechip">⌗ {tmuxWindow}</span>}
          <span className="fd-termhint">⌨ keystrokes go to the live agent — Esc included · close with ✕</span>
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
