import { useCallback, useRef, useState } from 'react';
import { reviveSpawn, enableRemote, reasonOf } from './api.js';

// M-F2 — revive and enable-remote used to be implemented TWICE: once on the
// card chip (App) and once in the drawer's OWNED PANE (Drawer), each with its
// OWN in-flight flag. So the same session's card chip and drawer button could
// each fire a POST, and the second landed as a spurious 409 mid-turn race.
//
// This hook is the single owner of BOTH POSTs and the per-spawn in-flight set
// that guards them. App holds one instance and hands the actions + busy sets to
// the card lane AND down to the drawer, so whichever surface you click first
// takes the lock and the other is a no-op until it resolves. Each surface still
// renders its OWN feedback — the caller passes an `onResult(result)` per click,
// so the card reports on the header strip and the drawer reports inline, off the
// one shared request.
//
// Result shapes handed to onResult:
//   revive        { ok }                         | { ok:false, reason }
//   reviveAll     { okN, total, fails:[string] }
//   enableRemote  { ok, url, pending }           | { ok:false, reason }
export function useSpawnActions() {
  // The refs are the source of truth for the in-flight guard (read synchronously
  // at click time); the state Sets mirror them so busy chips re-render.
  const revRef = useRef(new Set());
  const enRef = useRef(new Set());
  const allRef = useRef(false);
  const [reviving, setReviving] = useState(() => new Set());
  const [enabling, setEnabling] = useState(() => new Set());
  const [revivingAll, setRevivingAll] = useState(false);

  const revive = useCallback(async (s, onResult) => {
    const id = s?.spawn?.spawn_id;
    if (!id || revRef.current.has(id)) return;
    revRef.current.add(id);
    setReviving(new Set(revRef.current));
    try {
      const res = await reviveSpawn(id);
      const ok = res.ok && res.json?.ok !== false;
      onResult?.(ok ? { ok: true } : { ok: false, reason: reasonOf(res) });
    } finally {
      revRef.current.delete(id);
      setReviving(new Set(revRef.current));
    }
  }, []);

  // Revive-all: sequential POSTs (one summary result). Each id joins the shared
  // in-flight set so its own card chip reads "reviving…" too.
  const reviveAll = useCallback(async (list, onResult) => {
    if (allRef.current || !list?.length) return;
    allRef.current = true;
    setRevivingAll(true);
    for (const s of list) revRef.current.add(s.spawn.spawn_id);
    setReviving(new Set(revRef.current));
    let okN = 0;
    const fails = [];
    for (const s of list) {
      const id = s.spawn.spawn_id;
      const label = s.callsign || id;
      const res = await reviveSpawn(id);
      if (res.ok && res.json?.ok !== false) okN += 1;
      else fails.push(`${label}: ${reasonOf(res)}`);
      revRef.current.delete(id);
      setReviving(new Set(revRef.current));
    }
    allRef.current = false;
    setRevivingAll(false);
    onResult?.({ okN, total: list.length, fails });
  }, []);

  const enable = useCallback(async (s, onResult) => {
    const id = s?.spawn?.spawn_id;
    if (!id || enRef.current.has(id)) return;
    enRef.current.add(id);
    setEnabling(new Set(enRef.current));
    try {
      const res = await enableRemote(id);
      const ok = res.ok && res.json?.ok !== false;
      if (ok) onResult?.({ ok: true, url: res.json?.url || null, pending: !!res.json?.pending });
      else onResult?.({ ok: false, reason: reasonOf(res) });
    } finally {
      enRef.current.delete(id);
      setEnabling(new Set(enRef.current));
    }
  }, []);

  return { reviving, enabling, revivingAll, revive, reviveAll, enableRemote: enable };
}
