import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { spawnTermable, clampWinRect } from '../util.js';

// v1.4 — the identity a live terminal captures at open, so its stream survives
// the card mutating (or vanishing) mid-view. Pure, so it lives at module scope
// and both openTerm and openGrid share it.
const termIdentity = (s) => ({
  spawnId: s.spawn.spawn_id,
  callsign: s.callsign || s.session_id,
  window: s.spawn.tmux_window,
});

// v2.6 — the floating window's geometry, persisted so it reopens where you left
// it. One key for all terminals (predictable), clamped on read: a rect saved on
// a big monitor must still be grabbable on a laptop.
const RECT_KEY = 'fd-termwin';
function loadRect() {
  let saved = null;
  try { saved = JSON.parse(localStorage.getItem(RECT_KEY) || 'null'); } catch { /* corrupt entry */ }
  return clampWinRect(saved || {}, { w: window.innerWidth, h: window.innerHeight });
}

// The terminal subsystem: the single live terminal (a FLOATING window since
// v2.6 — drag/resize/minimize, the board stays interactive), the wall of
// screens (grid, still a full modal), and the watch-set that seeds the wall.
// `term` and `grid` are ONE keyboard — opening either closes the other — so
// this hook owns both and the invariant between them.
//
// `killAsk` / `armAsk` / `renameAsk` are threaded in only so the keydown MIRRORS
// live together, since the hotkey handler reads them synchronously off refs (a
// stale closure over state would misroute the key):
//   gridOpen   — "the WALL owns the whole screen": board hotkeys are dead while
//                it is up. The floating window deliberately does NOT suppress
//                them (that is what floating means) — its own keys never leak
//                because the window stops propagation itself;
//   killOpen   — "the kill dialog is modal over everything": Esc cancels IT;
//   armOpen    — "the move-to-tmux dialog is modal too": Esc cancels IT (v2.0),
//                leaving the drawer it may have been opened from standing;
//   renameOpen — same for the rename dialog (v2.1). Esc from INSIDE its text
//                input must abandon the rename, not close the drawer under it.
export function useTermWindows(sessions, killAsk, armAsk, renameAsk) {
  const [term, setTerm] = useState(null); // null | { spawnId, callsign, window }
  const [grid, setGrid] = useState(null); // null | [{ spawnId, callsign, window }]
  const [termMin, setTermMin] = useState(false); // v2.6 minimized to the dock chip
  const [termMax, setTermMax] = useState(false); // v2.6 maximize toggle
  const [termRect, setTermRectState] = useState(loadRect);
  // which agents are ticked for the grid (by session id, so the set survives a
  // card re-render; resolved to spawn identities at open time).
  const [watch, setWatch] = useState(() => new Set());

  const setTermRect = useCallback((r) => {
    setTermRectState(r);
    try { localStorage.setItem(RECT_KEY, JSON.stringify(r)); } catch { /* quota */ }
  }, []);

  // v2.6 — the GRID is the modal one; the floating term window is not. Only
  // gridOpen feeds the hotkey suppression list now.
  const gridOpen = useRef(false);
  useEffect(() => { gridOpen.current = !!grid; }, [grid]);
  const killOpen = useRef(false);
  useEffect(() => { killOpen.current = !!killAsk; }, [killAsk]);
  const armOpen = useRef(false);
  useEffect(() => { armOpen.current = !!armAsk; }, [armAsk]);
  const renameOpen = useRef(false);
  useEffect(() => { renameOpen.current = !!renameAsk; }, [renameAsk]);

  // Only board-spawned panes exist to be watched: a plain `claude` in your own
  // terminal has no pane the daemon owns.
  const termableSessions = useMemo(() => sessions.filter(spawnTermable), [sessions]);
  const watchable = useMemo(
    () => termableSessions.filter((s) => watch.has(s.session_id)),
    [termableSessions, watch],
  );

  // v1.4 — open the live terminal for a board-spawned session. useCallback so the
  // card lane's props stay stable (M-P4); termIdentity is at module scope.
  // Reopening (any card) un-minimizes: the human asked to SEE a terminal.
  const openTerm = useCallback((s) => {
    if (!spawnTermable(s)) return;
    setGrid(null); // the window and the wall are one keyboard; never both
    setTermMin(false);
    setTerm(termIdentity(s));
  }, []);

  const closeTerm = useCallback(() => { setTerm(null); setTermMin(false); }, []);
  const minimizeTerm = useCallback(() => setTermMin(true), []);
  const restoreTerm = useCallback(() => setTermMin(false), []);
  const toggleTermMax = useCallback(() => setTermMax((m) => !m), []);

  const toggleWatch = useCallback((s) => {
    if (!spawnTermable(s)) return;
    setWatch((prev) => {
      const next = new Set(prev);
      if (next.has(s.session_id)) next.delete(s.session_id);
      else next.add(s.session_id);
      return next;
    });
  }, []);

  // The wall of screens. Passed the ticked agents (or nothing → every live pane),
  // so "just show me everything" is one click. Opening the wall closes the
  // floating window entirely (not to the dock): one keyboard, one owner.
  const openGrid = useCallback((list) => {
    const tiles = (list && list.length ? list : termableSessions).filter(spawnTermable).map(termIdentity);
    if (!tiles.length) return;
    setTerm(null);
    setTermMin(false);
    setGrid(tiles);
  }, [termableSessions]);

  return {
    term, setTerm, grid, setGrid, watch,
    termMin, minimizeTerm, restoreTerm, closeTerm,
    termMax, toggleTermMax, termRect, setTermRect,
    termableSessions, watchable,
    openTerm, toggleWatch, openGrid,
    gridOpen, killOpen, armOpen, renameOpen,
  };
}
