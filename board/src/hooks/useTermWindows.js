import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { spawnTermable } from '../util.js';

// v1.4 — the identity a live terminal captures at open, so its stream survives
// the card mutating (or vanishing) mid-view. Pure, so it lives at module scope
// and both openTerm and openGrid share it.
const termIdentity = (s) => ({
  spawnId: s.spawn.spawn_id,
  callsign: s.callsign || s.session_id,
  window: s.spawn.tmux_window,
});

// The terminal subsystem: the single live terminal (modal), the wall of screens
// (grid), and the watch-set that seeds the wall. `term` and `grid` are ONE
// keyboard — opening either closes the other — so this hook owns both and the
// invariant between them.
//
// `killAsk` is threaded in only so the two keydown MIRRORS live together, since
// the hotkey handler reads them synchronously off refs (a stale closure over
// state would misroute the key):
//   termOpen — "a live terminal has the keyboard": Esc is the agent's, never
//              ours (the modal OR the grid, whose focused tile owns Esc too);
//   killOpen — "the kill dialog is modal over everything": Esc cancels IT.
export function useTermWindows(sessions, killAsk) {
  const [term, setTerm] = useState(null); // null | { spawnId, callsign, window }
  const [grid, setGrid] = useState(null); // null | [{ spawnId, callsign, window }]
  // which agents are ticked for the grid (by session id, so the set survives a
  // card re-render; resolved to spawn identities at open time).
  const [watch, setWatch] = useState(() => new Set());

  const termOpen = useRef(false);
  useEffect(() => { termOpen.current = !!term || !!grid; }, [term, grid]);
  const killOpen = useRef(false);
  useEffect(() => { killOpen.current = !!killAsk; }, [killAsk]);

  // Only board-spawned panes exist to be watched: a plain `claude` in your own
  // terminal has no pane the daemon owns.
  const termableSessions = useMemo(() => sessions.filter(spawnTermable), [sessions]);
  const watchable = useMemo(
    () => termableSessions.filter((s) => watch.has(s.session_id)),
    [termableSessions, watch],
  );

  // v1.4 — open the live terminal for a board-spawned session. useCallback so the
  // card lane's props stay stable (M-P4); termIdentity is at module scope.
  const openTerm = useCallback((s) => {
    if (!spawnTermable(s)) return;
    setGrid(null); // the modal and the wall are one keyboard; never both
    setTerm(termIdentity(s));
  }, []);

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
  // so "just show me everything" is one click.
  const openGrid = useCallback((list) => {
    const tiles = (list && list.length ? list : termableSessions).filter(spawnTermable).map(termIdentity);
    if (!tiles.length) return;
    setTerm(null);
    setGrid(tiles);
  }, [termableSessions]);

  return {
    term, setTerm, grid, setGrid, watch,
    termableSessions, watchable,
    openTerm, toggleWatch, openGrid,
    termOpen, killOpen,
  };
}
