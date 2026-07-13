import React, { useRef, useState } from 'react';
import TermPane from './TermPane.jsx';
import { useModal } from '../useModal.js';

// The wall of screens (v1.9): N live terminals at once, one tile per agent.
//
// THE RULE THAT MAKES IT SAFE: every tile streams, exactly ONE tile types. A
// keystroke here is not a UI action — it lands in a real agent's TUI and cannot
// be taken back — so "which terminal am I typing into" must never be a thing the
// human has to infer. The focused tile is drawn with an unmistakable ring and
// says so in its header; every other tile is stdin-disabled at the xterm level,
// not merely ignored on the way out.
//
// Click a tile to move the focus. Esc is NOT a close key here for the same
// reason it isn't in the modal: it belongs to the focused agent's TUI. Closing
// is the ✕, and ⤢ promotes one tile to the full-size modal.

const COLS = (n) => (n <= 1 ? 1 : n <= 4 ? 2 : n <= 9 ? 3 : 4);

export default function TermGrid({ tiles, fallbackFocusRef, onClose, onExpand }) {
  // The focused tile owns the keyboard. Default to the first — a grid with
  // nothing focused would look like a broken keyboard rather than a choice.
  const [focused, setFocused] = useState(tiles[0]?.spawnId ?? null);
  const [notes, setNotes] = useState({}); // spawnId -> {kind,text} | null
  const dialogRef = useRef(null);
  // M-A2 (terminal variant) — restore focus on close; NO Tab trap / focus steal
  // (Tab and focus belong to the focused agent's xterm). R3-4 — fallbackFocusRef
  // (the header Terminals button, our own opener) keeps parity with the modal.
  useModal(dialogRef, { trap: false, initialFocus: false, fallbackRef: fallbackFocusRef });

  const cols = COLS(tiles.length);

  // M-A3 — a keyboard path for moving the typing focus between tiles, since the
  // click handoff was mouse-only. Ctrl+←/→ cycles. Handled in the CAPTURE phase
  // on the wrapper so it is consumed BEFORE the focused agent's xterm sees it
  // (otherwise the chord would also be typed into the agent).
  const cycle = (dir) => setFocused((cur) => {
    const i = tiles.findIndex((t) => t.spawnId === cur);
    const base = i < 0 ? 0 : i;
    return tiles[(base + dir + tiles.length) % tiles.length]?.spawnId ?? cur;
  });
  const onCaptureKey = (e) => {
    if (e.ctrlKey && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
      e.preventDefault();
      e.stopPropagation();
      cycle(e.key === 'ArrowRight' ? 1 : -1);
    }
  };

  return (
    <div
      className="fd-gridwrap"
      role="presentation"
      onKeyDownCapture={onCaptureKey}
      onKeyDown={(e) => e.stopPropagation()}
    >
      <div className="fd-grid" role="dialog" aria-modal="true" aria-label={`Live terminals — ${tiles.length} agents`} ref={dialogRef}>
        <div className="fd-gridhead">
          <span className="lbl">LIVE TERMINALS</span>
          <span className="fd-gridcount">{tiles.length}</span>
          <span className="fd-termhint">
            ⌨ keystrokes go to the <b>focused</b> agent only — click a tile or press <b>Ctrl+←/→</b> to move the focus · <b>⇧⏎</b> for a newline
          </span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close terminals" onClick={onClose}>✕</button>
        </div>

        <div className="fd-gridbody" style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))` }}>
          {tiles.map((t) => {
            const live = t.spawnId === focused;
            const note = notes[t.spawnId];
            return (
              <div
                key={t.spawnId}
                className={`fd-tile${live ? ' live' : ''}${note ? ' ended' : ''}`}
                onMouseDown={() => setFocused(t.spawnId)}
                role="presentation"
              >
                <div className="fd-tilehead">
                  <span className="callsign">{t.callsign || t.spawnId}</span>
                  {live
                    ? <span className="fd-tilefocus">⌨ typing here</span>
                    : <span className="fd-tilewatch">watching</span>}
                  <span className="fd-spacer" />
                  <button
                    type="button"
                    className="fd-ghostbtn"
                    aria-label={`Expand ${t.callsign || t.spawnId}`}
                    title="open full size"
                    onMouseDown={(e) => e.stopPropagation()}
                    onClick={() => onExpand?.(t)}
                  >⤢</button>
                </div>
                <div className="fd-tilebody">
                  <TermPane
                    spawnId={t.spawnId}
                    live={live}
                    fontSize={11}
                    onNote={(n) => setNotes((m) => (m[t.spawnId] === n ? m : { ...m, [t.spawnId]: n }))}
                  />
                  {note && (
                    <div className={`fd-tilenote ${note.kind}`}>
                      {note.kind === 'err' ? '✗' : '⏻'} {note.text}
                    </div>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
