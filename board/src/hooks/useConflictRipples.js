import { useEffect, useRef, useState } from 'react';

// One-shot conflict ripple: when a conflict row FIRST appears, the cards of its
// participants pulse for ~2 s. It must fire only on a genuinely NEW conflict —
// never on the initial snapshot (history doesn't ripple) and never again for a
// conflict already on the board.
//
// The guard is prevConflicts: `keys` is the set of conflict identities already
// seen, and `sawData` latches once the board has received real data, so the
// very first painted snapshot (which arrives with keys=null) can't ripple its
// whole backlog at once.
//
// Returns the live `ripples` Map (sid -> until-ms) that SessionCard reads via
// BoardLanes.
export function useConflictRipples(snap) {
  const [ripples, setRipples] = useState(() => new Map());
  const prevConflicts = useRef({ keys: null, sawData: false });
  // Pending removal timers, tracked so they can be cleared on unmount — WITHOUT
  // clearing per effect-run, which would cancel a ripple mid-flight.
  const timers = useRef(new Set());

  useEffect(() => {
    const store = prevConflicts.current;
    const list = snap.conflicts || [];
    const keyOf = (c) => `${c.at}:${c.rel_path || c.file}:${(c.sessions || []).join(',')}`;
    const keys = new Set(list.map(keyOf));
    const isData = (snap.up_ms || 0) > 0 || (snap.sessions || []).length > 0 || list.length > 0;
    if (store.keys && store.sawData) {
      const until = Date.now() + 2000;
      const add = new Map();
      for (const c of list) {
        if (!store.keys.has(keyOf(c))) for (const sid of c.sessions || []) add.set(sid, until);
      }
      if (add.size) {
        setRipples((prev) => new Map([...prev, ...add]));
        const t = setTimeout(() => {
          timers.current.delete(t);
          setRipples((prev) => {
            const m = new Map(prev);
            for (const [sid, u] of add) if (m.get(sid) === u) m.delete(sid);
            return m;
          });
        }, 2200);
        timers.current.add(t);
      }
    }
    store.keys = keys;
    if (isData) store.sawData = true;
  }, [snap]);

  // Unmount-only cleanup: drop any ripple-removal timers still pending.
  useEffect(() => () => { for (const t of timers.current) clearTimeout(t); }, []);

  return ripples;
}
