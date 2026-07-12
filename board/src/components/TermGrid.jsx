import React, { useEffect, useState } from 'react';
import TermPane from './TermPane.jsx';

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

export default function TermGrid({ tiles, onClose, onExpand }) {
  // The focused tile owns the keyboard. Default to the first — a grid with
  // nothing focused would look like a broken keyboard rather than a choice.
  const [focused, setFocused] = useState(tiles[0]?.spawnId ?? null);
  const [notes, setNotes] = useState({}); // spawnId -> {kind,text} | null

  // If the focused agent disappears (killed, ended), hand the keyboard to a
  // survivor rather than leaving it pointing at nothing.
  useEffect(() => {
    if (tiles.some((t) => t.spawnId === focused)) return;
    setFocused(tiles[0]?.spawnId ?? null);
  }, [tiles, focused]);

  const cols = COLS(tiles.length);

  return (
    <div className="fd-gridwrap" role="presentation" onKeyDown={(e) => e.stopPropagation()}>
      <div className="fd-grid" role="dialog" aria-label={`Live terminals — ${tiles.length} agents`}>
        <div className="fd-gridhead">
          <span className="lbl">LIVE TERMINALS</span>
          <span className="fd-gridcount">{tiles.length}</span>
          <span className="fd-termhint">
            ⌨ keystrokes go to the <b>focused</b> agent only — click a tile to move the focus
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
