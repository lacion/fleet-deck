import { useCallback, useRef, useState } from 'react';
import { reviveSpawn, enableRemote, adoptSession, reasonOf } from './api.js';

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
  // v2.0 Move-to-tmux — the adopt/arm/disarm in-flight guard. Keyed on
  // SESSION_ID, not spawn_id: the card being moved has NO spawn row yet (that is
  // the whole point), so the lock is the session. adopt, arm and disarm all POST
  // to the same endpoint on the same session, so they share ONE lock.
  const adoptRef = useRef(new Set());
  const [reviving, setReviving] = useState(() => new Set());
  const [enabling, setEnabling] = useState(() => new Set());
  const [revivingAll, setRevivingAll] = useState(false);
  const [adopting, setAdopting] = useState(() => new Set());

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
  //
  // R3-1 — the bulk must acquire the SAME per-id lock (revRef) a single Revive
  // uses, not just the bulk lock (allRef). Otherwise clicking a card's Revive
  // and THEN "Revive all" fires two POSTs at that one spawn, and whichever
  // finishes first frees the shared id out from under the other. So we lock only
  // the ids NOT already in flight, remember exactly which ones WE locked, and in
  // the finally release only those — never the id a single Revive still owns.
  const reviveAll = useCallback(async (list, onResult) => {
    if (allRef.current || !list?.length) return;
    allRef.current = true;
    setRevivingAll(true);
    // Take the per-id lock atomically (check-then-add, no await between), so a
    // spawn a single Revive is already handling is skipped here, not POSTed twice.
    const mine = [];
    for (const s of list) {
      const id = s?.spawn?.spawn_id;
      if (!id || revRef.current.has(id)) continue;
      revRef.current.add(id);
      mine.push(s);
    }
    setReviving(new Set(revRef.current));
    let okN = 0;
    const fails = [];
    for (const s of mine) {
      const id = s.spawn.spawn_id;
      const label = s.callsign || id;
      const res = await reviveSpawn(id);
      if (res.ok && res.json?.ok !== false) okN += 1;
      else fails.push(`${label}: ${reasonOf(res)}`);
      revRef.current.delete(id); // only ids this bulk locked ever reach here
      setReviving(new Set(revRef.current));
    }
    allRef.current = false;
    setRevivingAll(false);
    // total is what THIS bulk attempted — any id skipped above is being revived
    // by its own single Revive, which reports its own outcome.
    onResult?.({ okN, total: mine.length, fails });
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

  // v2.0 Move-to-tmux — the single POST owner for adopt / arm / disarm. `body`
  // picks the intent ({} or {dangerously_skip_permissions:true} to adopt-or-arm,
  // {disarm:true} to disarm); the daemon decides adopt-vs-arm from the session's
  // own liveness. M-F2 — one owner, shared by the card chip AND the drawer (via
  // useFleetActions), so whichever surface fires first takes the per-session lock
  // and the other is a no-op until it resolves. onResult carries the branch the
  // daemon took so the reporter can say "moved" vs "armed" vs the reason.
  const adopt = useCallback(async (s, body, onResult) => {
    const id = s?.session_id;
    if (!id || adoptRef.current.has(id)) return;
    adoptRef.current.add(id);
    setAdopting(new Set(adoptRef.current));
    try {
      const res = await adoptSession(id, body || {});
      const ok = res.ok && res.json?.ok !== false;
      onResult?.(ok
        ? { ok: true, adopted: !!res.json?.adopted, armed: !!res.json?.armed }
        : { ok: false, reason: reasonOf(res) });
    } finally {
      adoptRef.current.delete(id);
      setAdopting(new Set(adoptRef.current));
    }
  }, []);

  return { reviving, enabling, revivingAll, adopting, revive, reviveAll, enableRemote: enable, adopt };
}
