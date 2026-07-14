import React, { useEffect, useRef, useState } from 'react';
import { useModal } from '../useModal.js';
import { animalOf, suffixOf, validSuffix, SUFFIX_MAX } from '../util.js';

// v2.1 "Rename" (0.7.1) — name a session yourself. A callsign is
// `<animal>-<suffix>`, and the ANIMAL IS NOT YOURS TO PICK: the daemon mints it
// once and every surface that ever showed this card has been reading it since.
// Only the suffix is renameable ('wren-a9e1' → 'wren-docs-review').
//
// That asymmetry is the whole design of this dialog: the animal is rendered as
// STATIC TEXT welded to the left edge of the input, and the input holds the
// suffix ALONE. You can see the callsign you are building, in one line, and you
// can see which half you own — nobody types 'wren-' into the box and wonders why
// they got 'wren-wren-docs'.
//
// The charset is validated live (util.js validSuffix — the client-side copy of
// the daemon's rule; the daemon is still the authority) so a name the daemon
// would refuse can't leave this dialog: the hint turns hazard-colored and the
// confirm goes disabled. Everything else — the name already being taken, a
// reserved word — only the daemon can know, and its refusal reason lands verbatim
// on the shared feedback strip.
//
// Structure + focus-trap mirror KillConfirm / ArmMoveConfirm: the scrim is the
// backdrop and cancels on click, Esc cancels (App's global handler closes this
// first — see the renameOpen ref there), and the affirmative is the act-colored
// Send button, not the hazard one: renaming is benign and reversible (the quiet
// "reset to the automatic name" below sends {clear:true} and puts the ticket or
// hex name back). The one deviation from those two dialogs: the INPUT owns
// initial focus, not the safe button — this dialog exists to be typed into, and
// ⏎ submits (Compose's convention) rather than cancelling.
export default function RenameDialog({ callsign, tmuxWindow, busy, onCancel, onConfirm, onReset }) {
  const animal = animalOf(callsign);
  const current = suffixOf(callsign);
  const [suffix, setSuffix] = useState(current);
  const inputRef = useRef(null);
  const dialogRef = useRef(null);
  // M-A2 — trap Tab + restore focus on close; the input owns initial focus.
  useModal(dialogRef, { initialFocus: false });
  // Pre-filled with the CURRENT suffix and selected, so the common case (throw
  // the hex away and type a real name) is one keystroke, and the rarer case
  // (tweak the name you already gave it) is still an edit rather than a retype.
  useEffect(() => {
    const el = inputRef.current;
    el?.focus();
    el?.select();
  }, []);

  const next = suffix.trim();
  const valid = validSuffix(next);
  const unchanged = next === current;

  const cancel = () => { if (!busy) onCancel(); };
  const confirm = () => { if (!busy && valid) onConfirm(next); };
  const reset = () => { if (!busy) onReset(); };

  // The hint line does three jobs, in priority order: say what's wrong, or show
  // the callsign this will actually produce. It never scolds an empty box the
  // instant the dialog opens with a name already in it — you get the charset
  // sentence only once you've typed something the daemon would refuse.
  const hint = !next
    ? { bad: true, text: 'a suffix is required — or reset to the automatic name below' }
    : !valid
      ? { bad: true, text: `letters, digits and dashes only · must start with a letter or digit · ${SUFFIX_MAX} characters max` }
      : unchanged
        ? { bad: false, text: 'this is already its name' }
        : { bad: false, text: `→ ${animal}-${next}` };

  return (
    <div className="fd-composewrap" onClick={cancel}>
      <div
        className="fd-compose fd-killask fd-renameask"
        role="dialog"
        aria-modal="true"
        aria-label={`Rename ${callsign}`}
        ref={dialogRef}
        onClick={(e) => e.stopPropagation()}
      >
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <span className="lbl">✎ RENAME SESSION</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Cancel" disabled={busy} onClick={cancel}>✕</button>
        </div>

        <div className="ask">
          Rename <b>{callsign}</b>.
        </div>
        <div className="sub">
          The animal is the daemon’s — it never changes. Everything after it is yours.
        </div>

        {/* the callsign, built in place: fixed animal + editable suffix, one line */}
        <div className="fd-namerow">
          <span className="fd-namefix" aria-hidden="true">{animal}-</span>
          <input
            ref={inputRef}
            className="fd-input fd-nameinput"
            type="text"
            // the animal is static text OUTSIDE the input, so the field's label
            // has to say what the field alone holds, for anyone not seeing it
            aria-label={`New suffix for ${callsign} — the callsign will be ${animal}-<suffix>`}
            aria-invalid={!!next && !valid}
            maxLength={SUFFIX_MAX}
            spellCheck={false}
            autoComplete="off"
            placeholder="docs-review"
            value={suffix}
            disabled={busy}
            onChange={(e) => setSuffix(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); confirm(); } }}
          />
        </div>
        <div className={`fd-namehint${hint.bad ? ' bad' : ''}`}>{hint.text}</div>

        <div className="sub">
          Mail addressed to the old name still reaches it — the board remembers the name it used to answer to.
        </div>
        {tmuxWindow && (
          // Honest about the one thing that does NOT follow the name: the tmux
          // window was frozen at spawn and is deliberately never renamed (a crash
          // between the tmux rename and the DB write would strand the pane), so
          // the card's ⌗ chip keeps showing the old name. Say so, rather than let
          // it read as a bug.
          <div className="sub">
            Its tmux window stays <span className="win">{tmuxWindow}</span> — the pane, its terminal and its kill
            chip are untouched by a rename.
          </div>
        )}

        <div className="foot">
          {/* quiet, deliberately: reverting is a real door, but it is not the
              thing you opened this dialog to do. Sends {clear:true} — the daemon
              puts the ticket name back, or the hex name when there is no ticket. */}
          <button type="button" className="fd-ghostbtn reset" disabled={busy} onClick={reset}>
            ⟲ reset to the automatic name
          </button>
          <span className="fd-spacer" />
          <button type="button" className="fd-ghostbtn" disabled={busy} onClick={cancel}>
            Cancel
          </button>
          <button type="button" className="send" disabled={busy || !valid} onClick={confirm}>
            {busy ? 'Renaming…' : '✎ Rename'}
          </button>
        </div>
      </div>
    </div>
  );
}
