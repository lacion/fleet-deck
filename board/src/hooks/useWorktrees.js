import { useCallback, useEffect, useRef, useState } from 'react';
import { fetchWorktrees, removeWorktree, reasonOf } from '../api.js';

// v1.9 — worktrees. The daemon runs git per row to answer this, so the board
// does NOT poll it on a timer: it reads once at boot, again whenever the fleet
// gains or loses a session (a spawn creates a worktree; a death strands one),
// and on every open/refresh/removal from the modal. A 404 means this daemon
// predates the endpoint — we latch that and hide the affordance entirely rather
// than leaving a button that leads nowhere.
//
// The list lives in a hook (not the modal) because the HEADER carries its count:
// a worktree holding unpushed work is a fact about the fleet, not a detail of a
// modal that happens to be open. `sessionCount` is snap.sessions.length — the
// trigger that reloads the list when the fleet's shape changes.
export function useWorktrees(sessionCount) {
  const [worktrees, setWorktrees] = useState(null); // null = never loaded
  const [wtLoading, setWtLoading] = useState(false);
  const [wtErr, setWtErr] = useState(null);
  const [wtSupported, setWtSupported] = useState(true); // 404 → older daemon
  const wtGone = useRef(false); // this daemon has no /api/worktrees — stop asking

  const loadWorktrees = useCallback(async () => {
    if (wtGone.current) return;
    setWtLoading(true);
    try {
      const res = await fetchWorktrees();
      if (res.status === 404) {
        wtGone.current = true;
        setWtSupported(false);
        setWorktrees([]);
        setWtErr(null);
      } else if (res.ok && res.json?.ok !== false && Array.isArray(res.json?.worktrees)) {
        setWorktrees(res.json.worktrees);
        setWtErr(null);
      } else if (res.status !== 401) {
        // 401 is the token gate's business, not ours
        setWtErr(reasonOf(res, `could not list worktrees (${res.status})`));
      }
    } finally {
      setWtLoading(false);
    }
  }, []);
  useEffect(() => { loadWorktrees(); }, [loadWorktrees, sessionCount]);

  // The POST only. The modal owns the confirmation that precedes force:true and
  // shows the daemon's refusal verbatim; this just reports the outcome back.
  const removeWt = useCallback(async (path, opts) => {
    const res = await removeWorktree(path, opts);
    if (res.ok && res.json?.ok !== false) return { ok: true, json: res.json };
    return { ok: false, reason: reasonOf(res, `remove failed (${res.status})`) };
  }, []);

  const wtCount = Array.isArray(worktrees) ? worktrees.length : 0;
  const wtHazard = (worktrees || []).some((w) => w.verdict === 'has-work' || w.verdict === 'unknown');

  return { worktrees, wtLoading, wtErr, wtSupported, loadWorktrees, removeWorktree: removeWt, wtCount, wtHazard };
}
