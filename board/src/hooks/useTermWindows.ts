import { useCallback, useMemo, useRef, useState } from 'react';
import { spawnTermable, clampWinRect } from '../util.ts';
import { storageGet, storageSet } from '../storage.ts';
import type { SessionEntry, SessionSpawn } from '../../../contracts/index.ts';

// The identity a live terminal captures at open (see termIdentity below).
interface TermIdentity {
  spawnId: string;
  callsign: string;
  window: string | null;
}
// The floating window's live value: an identity plus the per-open stamp.
interface TermState extends TermIdentity {
  n: number;
}

// spawnTermable returns true only after confirming `s.spawn` is present (it
// leads with `if (!s?.spawn) return false`), so this is a sound type-predicate
// wrapper around it: the sessions it lets through carry a NON-optional spawn,
// which is exactly what every termIdentity read needs. Same runtime check.
const isTermable = (s: SessionEntry): s is SessionEntry & { spawn: SessionSpawn } =>
  spawnTermable(s);

// v1.4 — the identity a live terminal captures at open, so its stream survives
// the card mutating (or vanishing) mid-view. Pure, so it lives at module scope
// and both openTerm and openGrid share it.
const termIdentity = (s: SessionEntry & { spawn: SessionSpawn }): TermIdentity => ({
  spawnId: s.spawn.spawn_id,
  callsign: s.callsign || s.session_id,
  window: s.spawn.tmux_window,
});

// 2.1 focus-terminal — a per-open stamp so re-asking for the terminal that is
// ALREADY open still reads as a fresh request: TermWindow is keyed on it
// (App.jsx), so a re-open REMOUNTS the window and replays its attention pulse
// instead of no-oping on an identical identity. Monotonic (never Date.now —
// two opens in the same millisecond must still differ). Only openTerm/
// expandTerm stamp: the grid's tiles compare identities field-by-field and
// must stay stamp-free.
let termOpenStamp = 0;

// v2.6 — the floating window's geometry, persisted so it reopens where you left
// it. One key for all terminals (predictable), clamped on read: a rect saved on
// a big monitor must still be grabbable on a laptop.
const RECT_KEY = 'fd-termwin';
function loadRect() {
  let saved: unknown = null;
  try {
    saved = JSON.parse((storageGet(RECT_KEY) ?? '') || 'null');
  } catch {
    /* corrupt entry */
  }
  return clampWinRect(saved ?? {}, { w: window.innerWidth, h: window.innerHeight });
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
export function useTermWindows(
  sessions: SessionEntry[],
  killAsk: unknown,
  armAsk: unknown,
  renameAsk: unknown,
) {
  const [term, setTerm] = useState<TermState | null>(null); // null | { spawnId, callsign, window }
  const [grid, setGrid] = useState<TermIdentity[] | null>(null); // null | [{ spawnId, callsign, window }]
  const [termMin, setTermMin] = useState(false); // v2.6 minimized to the dock chip
  const [termMax, setTermMax] = useState(false); // v2.6 maximize toggle
  const [termRect, setTermRectState] = useState(loadRect);
  // which agents are ticked for the grid (by session id, so the set survives a
  // card re-render; resolved to spawn identities at open time).
  const [watch, setWatch] = useState<Set<string>>(() => new Set());

  const setTermRect = useCallback((r: ReturnType<typeof clampWinRect>) => {
    setTermRectState(r);
    storageSet(RECT_KEY, JSON.stringify(r));
  }, []);

  // v2.6 — the GRID is the modal one; the floating term window is not. Only
  // gridOpen feeds the hotkey suppression list now.
  const gridOpen = useRef(false);
  gridOpen.current = !!grid;
  const killOpen = useRef(false);
  killOpen.current = !!killAsk;
  const armOpen = useRef(false);
  armOpen.current = !!armAsk;
  const renameOpen = useRef(false);
  renameOpen.current = !!renameAsk;

  // Only board-spawned panes exist to be watched: a plain `claude` in your own
  // terminal has no pane the daemon owns.
  const termableSessions = useMemo(() => sessions.filter(isTermable), [sessions]);
  const watchable = useMemo(
    () => termableSessions.filter((s) => watch.has(s.session_id)),
    [termableSessions, watch],
  );

  // v1.4 — open the live terminal for a board-spawned session. useCallback so the
  // card lane's props stay stable (M-P4); termIdentity is at module scope.
  // Reopening (any card) un-minimizes: the human asked to SEE a terminal.
  const openTerm = useCallback((s: SessionEntry) => {
    if (!isTermable(s)) return;
    setGrid(null); // the window and the wall are one keyboard; never both
    setTermMin(false);
    setTermMax(false); // m2 — maximize is per-viewing, never inherited
    setTerm({ ...termIdentity(s), n: ++termOpenStamp });
  }, []);

  const closeTerm = useCallback(() => {
    setTerm(null);
    setTermMin(false);
    setTermMax(false);
  }, []);
  const minimizeTerm = useCallback(() => {
    setTermMin(true);
  }, []);
  const restoreTerm = useCallback(() => {
    setTermMin(false);
  }, []);
  const toggleTermMax = useCallback(() => {
    setTermMax((m) => !m);
  }, []);
  // The grid's ⤢ promotes a tile identity to the floating window — same reset
  // discipline as openTerm (m2: no inherited minimize/maximize).
  const expandTerm = useCallback((identity: TermIdentity) => {
    setGrid(null);
    setTermMin(false);
    setTermMax(false);
    setTerm({ ...identity, n: ++termOpenStamp }); // stamp: a promoted tile may already BE the open window
  }, []);

  // v1.3 (ux) — view-only detach: the tile leaves the wall, the tmux pane
  // stays alive (hard kill stays behind KillConfirm). The session also leaves
  // the watch-set, or the next watch-seeded openGrid would resurrect the tile
  // the human just closed. watch keys on session_id but the identity carries
  // only spawnId/callsign — resolve via the live sessions; when the session is
  // already gone the stale watch entry is filtered at open time anyway.
  const closeGridTile = useCallback(
    (identity: TermIdentity) => {
      setGrid((prev) => {
        if (!prev) return prev;
        const next = prev.filter(
          (t) => !(t.spawnId === identity.spawnId && t.window === identity.window),
        );
        return next.length ? next : null; // last tile closes the wall
      });
      setWatch((prev) => {
        if (!prev.size) return prev;
        const sid =
          termableSessions.find((s) => s.spawn.spawn_id === identity.spawnId)?.session_id ??
          // callsign IS the session_id for nameless agents — the only lossless
          // fallback once the session row is gone.
          (prev.has(identity.callsign) ? identity.callsign : null);
        if (!sid || !prev.has(sid)) return prev;
        const next = new Set(prev);
        next.delete(sid);
        return next;
      });
    },
    [termableSessions],
  );

  const toggleWatch = useCallback((s: SessionEntry) => {
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
  const openGrid = useCallback(
    (list?: SessionEntry[] | null) => {
      const tiles = (list?.length ? list : termableSessions).filter(isTermable).map(termIdentity);
      if (!tiles.length) return;
      setTerm(null);
      setTermMin(false);
      setGrid(tiles);
    },
    [termableSessions],
  );

  return {
    term,
    setTerm,
    grid,
    setGrid,
    watch,
    termMin,
    minimizeTerm,
    restoreTerm,
    closeTerm,
    expandTerm,
    termMax,
    toggleTermMax,
    termRect,
    setTermRect,
    termableSessions,
    watchable,
    openTerm,
    toggleWatch,
    openGrid,
    closeGridTile,
    gridOpen,
    killOpen,
    armOpen,
    renameOpen,
  };
}
