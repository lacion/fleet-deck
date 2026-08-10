import { useEffect, useRef, useState } from 'react';
import type { SessionEntry } from '../../../contracts/index.ts';

// One conflict row as this hook reads it. `sessions` is optional so the `?? []`
// guard below stays honest — the /state contract marks it required, but an older
// daemon (or a guarded-dropped row) can omit it, which is exactly what the guard
// is for.
interface ConflictLike {
  at: number;
  rel_path: string;
  file: string;
  sessions?: string[];
}

// The slice of the /state snapshot this hook reads. Each field is OPTIONAL so the
// board's boot-time empty snapshot (fields absent until the first real frame)
// keeps every `?? …` fall-through honest while a real Snapshot still flows in.
interface SnapLike {
  conflicts?: ConflictLike[];
  sessions?: SessionEntry[];
  up_ms?: number;
}

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
export function useConflictRipples(snap: SnapLike) {
  const [ripples, setRipples] = useState<Map<string, number>>(() => new Map());
  const prevConflicts = useRef<{ keys: Set<string> | null; sawData: boolean }>({
    keys: null,
    sawData: false,
  });
  // Pending removal timers, tracked so they can be cleared on unmount — WITHOUT
  // clearing per effect-run, which would cancel a ripple mid-flight.
  const timers = useRef<Set<ReturnType<typeof setTimeout>>>(new Set());

  useEffect(() => {
    const store = prevConflicts.current;
    const list = snap.conflicts ?? [];
    const keyOf = (c: ConflictLike) =>
      `${c.at}:${c.rel_path || c.file}:${(c.sessions ?? []).join(',')}`;
    const keys = new Set(list.map(keyOf));
    const isData = (snap.up_ms ?? 0) > 0 || (snap.sessions ?? []).length > 0 || list.length > 0;
    if (store.keys && store.sawData) {
      const until = Date.now() + 2000;
      const add = new Map<string, number>();
      for (const c of list) {
        if (!store.keys.has(keyOf(c))) for (const sid of c.sessions ?? []) add.set(sid, until);
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
  useEffect(
    () => () => {
      for (const t of timers.current) clearTimeout(t);
    },
    [],
  );

  return ripples;
}
