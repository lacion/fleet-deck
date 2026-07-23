import React, { useCallback, useEffect, useRef, useState } from 'react';
import TermPane from './TermPane.jsx';
import { useModal } from '../useModal.js';
import { clampWinRect, TERMWIN_MIN } from '../util.js';

// The floating live terminal (v2.6) — replaces the full-screen TermModal so the
// board behind it stays visible AND interactive. The screen and the socket live
// in TermPane, unchanged; this is the chrome: a draggable header, a resize
// corner, ─ minimize (to the dock chip App renders), ⤢ maximize, ✕ close.
//
// EVERY key typed here goes to the agent — Esc included (Claude's TUI needs
// it) — so the window stops keydown propagation. Unlike the old modal, board
// hotkeys stay LIVE while this is open (the grid still suppresses them): with
// focus on the board, y/n/j/k act on the board. That is the point of floating.
//
// Geometry is clamped so the drag bar can never leave the screen, and persisted
// (App owns the rect + localStorage) so the window comes back where you left it.
// On small/touch viewports the CSS takes over with a full-screen layout and the
// handles hide — same experience as the old modal.
//
// While MINIMIZED the component stays mounted under display:none — the /ws/term
// socket and the xterm screen survive — and stdin is forced off so a hidden
// terminal can never receive a keystroke.
// The small/touch media query the CSS uses to force full-screen — the gesture
// engine checks the same query so a visually-inert drag can't still write
// geometry to localStorage.
const COARSE_MQ = '(max-width: 720px), (pointer: coarse)';

export default function TermWindow({
  spawnId, callsign, tmuxWindow, fallbackFocusRef, onClose,
  rect, onRect, minimized, onMinimize, maximized, onToggleMax,
}) {
  // null | {kind:'exit'|'err'|'close', text} — non-destructive: the terminal
  // stays on screen, frozen, under the strip.
  const [note, setNote] = useState(null);
  // M1 (adversarial review) — the float is NON-modal, so board answer keys
  // (y/n/1-9) stay live when focus is on the board. "Are my keys going to this
  // terminal right now?" must therefore be answerable at a glance, in the same
  // amber language the grid's focused tile uses: track focus-within and say so
  // in the header.
  const [focused, setFocused] = useState(false);
  const dialogRef = useRef(null);
  // M-A2 (terminal variant) — restore focus to the opener on close, but NO Tab
  // trap and NO initial-focus steal: xterm claims focus itself and Tab must
  // reach the agent. R3-4 — fallbackFocusRef catches focus when the opener
  // (a grid tile's ⤢) is gone by close time.
  useModal(dialogRef, { trap: false, initialFocus: false, fallbackRef: fallbackFocusRef });

  // Live geometry during a drag/resize lives in a ref and is applied straight
  // to style — one rAF per pointermove, no React re-render (a re-render per
  // move would fight xterm's ResizeObserver refit). The committed rect goes up
  // to App (state + localStorage) on pointerup only.
  const liveRect = useRef(rect);
  useEffect(() => { liveRect.current = rect; }, [rect]);
  const raf = useRef(0);

  const applyStyle = (r) => {
    const el = dialogRef.current;
    if (!el) return;
    el.style.left = `${r.x}px`;
    el.style.top = `${r.y}px`;
    el.style.width = `${r.w}px`;
    el.style.height = `${r.h}px`;
  };

  // One pointer gesture engine for both the drag bar and the resize corner.
  const gesture = (mode) => (e) => {
    if (maximized) return;
    // n3 — on small/touch viewports the CSS forces full-screen (!important), so
    // a drag is visually inert; don't let it silently commit geometry either.
    if (window.matchMedia?.(COARSE_MQ).matches) return;
    if (e.button !== 0) return;
    if (mode === 'drag' && e.target.closest('button')) return; // header buttons are not a handle
    e.preventDefault();
    const start = { px: e.clientX, py: e.clientY, ...liveRect.current };
    const viewport = () => ({ w: window.innerWidth, h: window.innerHeight });
    const el = e.currentTarget;
    el.setPointerCapture?.(e.pointerId);
    const move = (ev) => {
      const dx = ev.clientX - start.px;
      const dy = ev.clientY - start.py;
      const vp = viewport();
      // m1 — resizing grows from a fixed top-left, so cap the RIGHT/BOTTOM edge
      // at the viewport too (drag deliberately may hang a window off an edge;
      // resize must not). clampWinRect still owns min size + reachability.
      const next = mode === 'drag'
        ? clampWinRect({ ...start, x: start.x + dx, y: start.y + dy }, vp)
        : clampWinRect({
          ...start,
          w: Math.min(Math.max(TERMWIN_MIN.w, start.w + dx), vp.w - start.x),
          h: Math.min(Math.max(TERMWIN_MIN.h, start.h + dy), vp.h - start.y),
        }, vp);
      liveRect.current = next;
      cancelAnimationFrame(raf.current);
      raf.current = requestAnimationFrame(() => applyStyle(next));
    };
    const up = () => {
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerup', up);
      el.removeEventListener('pointercancel', up);
      cancelAnimationFrame(raf.current);
      applyStyle(liveRect.current);
      onRect(liveRect.current); // commit: state + localStorage
    };
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerup', up);
    el.addEventListener('pointercancel', up);
  };

  // Window resizes re-clamp the committed rect so the drag bar stays reachable.
  // Debounced (m5): a browser-window drag fires dozens of resize events, and
  // each commit is a state update + synchronous localStorage write.
  useEffect(() => {
    let t = 0;
    const onWin = () => {
      clearTimeout(t);
      t = setTimeout(() => {
        const next = clampWinRect(liveRect.current, { w: window.innerWidth, h: window.innerHeight });
        if (next.x !== liveRect.current.x || next.y !== liveRect.current.y
          || next.w !== liveRect.current.w || next.h !== liveRect.current.h) {
          onRect(next);
        }
      }, 150);
    };
    window.addEventListener('resize', onWin);
    return () => { clearTimeout(t); window.removeEventListener('resize', onWin); };
  }, [onRect]);

  // Minimize must also take the keyboard away from the hidden terminal: blur
  // whatever xterm focused, and park focus somewhere real (the dock chip App
  // renders is the natural landing — fallbackFocusRef covers the gap).
  const minimize = useCallback(() => {
    if (document.activeElement && dialogRef.current?.contains(document.activeElement)) {
      document.activeElement.blur();
    }
    onMinimize();
  }, [onMinimize]);

  return (
    // No scrim, no backdrop — the board behind stays interactive. stopPropagation
    // shields App's window-level shortcuts from keys typed INTO the terminal;
    // the old modal's blanket hotkey suppression is gone on purpose.
    <div
      className={`fd-termfloat${maximized ? ' max' : ''}${minimized ? ' min' : ''}${focused ? ' kbd' : ''}`}
      role="dialog"
      aria-modal="false"
      aria-label={`Live terminal ${callsign || spawnId}`}
      ref={dialogRef}
      style={maximized ? undefined : { left: rect.x, top: rect.y, width: rect.w, height: rect.h }}
      onKeyDown={(e) => e.stopPropagation()}
      onFocus={() => setFocused(true)}
      onBlur={(e) => { if (!e.currentTarget.contains(e.relatedTarget)) setFocused(false); }}
    >
      <div className="fd-termhead fd-termgrab" onPointerDown={gesture('drag')}>
        <span className="callsign">{callsign || spawnId}</span>
        {tmuxWindow && <span className="fd-panechip">⌗ {tmuxWindow}</span>}
        {/* M1 — the float is non-modal, so this must be legible from across the
            room: amber "⌨ typing here" when the terminal holds the keyboard
            (the grid's focused-tile language), grey "keys go to the BOARD"
            when it doesn't. */}
        {focused
          ? <span className="fd-tilefocus">⌨ typing here</span>
          : <span className="fd-tilewatch">keys go to the board — click to type here</span>}
        <span className="fd-termhint">
          ⌨ Esc included · <b>⇧⏎</b> for a newline
        </span>
        <span className="fd-spacer" />
        <button type="button" className="fd-winbtn" aria-label="Minimize terminal" title="minimize to the dock" onClick={minimize}>─</button>
        <button type="button" className="fd-winbtn" aria-label={maximized ? 'Restore terminal size' : 'Maximize terminal'} title={maximized ? 'restore' : 'maximize'} onClick={onToggleMax}>{maximized ? '⤡' : '⤢'}</button>
        <button type="button" className="fd-x" aria-label="Close terminal" onClick={onClose}>✕</button>
      </div>
      <div className="fd-termbody">
        <TermPane spawnId={spawnId} live={!minimized} onNote={setNote} />
        {note && (
          <div className={`fd-termnote ${note.kind}`}>
            <span className="msg">{note.kind === 'err' ? '✗' : '⏻'} {note.text}</span>
            <button type="button" className="fd-ghostbtn" onClick={onClose}>Close</button>
          </div>
        )}
      </div>
      {/* m3 — the corner hides while the end-of-stream strip is up: it would
          paint over the strip's Close button, and resizing a dead pane buys
          nothing. */}
      {!maximized && !note && (
        <div
          className="fd-termresize"
          aria-hidden="true"
          onPointerDown={gesture('resize')}
        >◢</div>
      )}
    </div>
  );
}
