import { useCallback, useEffect, useRef, useState } from 'react';
import { fetchWorktrees, removeWorktree, reasonOf } from '../api.ts';

// The last commit on a worktree's branch, as the daemon reports it.
export interface WorktreeLastCommit {
  at: number;
  subject: string;
  sha: string;
}

// One /api/worktrees row: a checkout the daemon judged (verdict + the evidence
// that produced it). The daemon does the judging; the board never guesses.
export interface Worktree {
  path: string;
  callsign: string | null;
  session_id: string;
  session_alive: boolean;
  spawn_status: string | null;
  exists: boolean;
  branch: string | null;
  base: string | null;
  dirty: number;
  dirty_files: string[];
  ahead: number;
  upstream: string | null;
  unpushed: number;
  merged: boolean;
  last_commit: WorktreeLastCommit | null;
  note?: string | null;
  verdict: string;
}

// The success shape of /api/worktrees — a refined view of res.json, whose
// declared type (ApiJson, imported and unmodifiable) is the lowest-common
// envelope and doesn't carry `worktrees`. `worktrees` is REQUIRED here so the
// `as WorktreesResponse` refinement is a genuine narrowing (an optional field
// would make ApiJson structurally assignable, and the lint would strip the cast
// as unnecessary). The `body?.worktrees` optional chain still yields
// `Worktree[] | undefined`, so the Array.isArray guard below stays honest.
interface WorktreesResponse {
  ok?: boolean;
  worktrees: Worktree[];
}

// A failed forced removal can carry the exact foreign-owned paths that blocked
// git plus the daemon-generated recovery command. Preserve that evidence for
// the modal instead of collapsing the response to its one-line reason.
interface WorktreeRemoveFailure {
  blocked_paths?: unknown;
  blocked_owner?: unknown;
  fix_command?: unknown;
}

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
export function useWorktrees(sessionCount: number) {
  const [worktrees, setWorktrees] = useState<Worktree[] | null>(null); // null = never loaded
  const [wtLoading, setWtLoading] = useState(false);
  const [wtErr, setWtErr] = useState<string | null>(null);
  const [wtSupported, setWtSupported] = useState(true); // 404 → older daemon
  const wtGone = useRef(false); // this daemon has no /api/worktrees — stop asking
  // A refresh can be triggered by boot, a session-count mutation, the modal,
  // and a completed removal at nearly the same time. Only the newest request
  // may publish; otherwise a slower, older git scan can resurrect a row that
  // was just removed (and can clear the loading indicator too early).
  const loadSeq = useRef(0);

  const loadWorktrees = useCallback(async () => {
    if (wtGone.current) return;
    const seq = ++loadSeq.current;
    setWtLoading(true);
    try {
      const res = await fetchWorktrees();
      if (seq !== loadSeq.current) return;
      // ApiJson (res.json's type) doesn't carry `worktrees`; read it off the
      // endpoint's own body shape.
      const body = res.json as WorktreesResponse | null;
      const wl = body?.worktrees;
      if (res.status === 404) {
        wtGone.current = true;
        setWtSupported(false);
        setWorktrees([]);
        setWtErr(null);
      } else if (res.ok && body?.ok !== false && Array.isArray(wl)) {
        setWorktrees(wl);
        setWtErr(null);
      } else if (res.status !== 401) {
        // 401 is the token gate's business, not ours
        setWtErr(reasonOf(res, `could not list worktrees (${res.status})`));
      }
    } finally {
      if (seq === loadSeq.current) setWtLoading(false);
    }
  }, []);
  useEffect(() => {
    void loadWorktrees();
  }, [loadWorktrees, sessionCount]);

  // The POST only. The modal owns the confirmation that precedes force:true and
  // shows the daemon's refusal verbatim; this just reports the outcome back.
  const removeWt = useCallback(
    async (path: string, opts: { force?: boolean; deleteBranch?: boolean }) => {
      const res = await removeWorktree(path, opts);
      // `as const` pins the `ok` discriminant to its literal (a bare object
      // literal widens `true`/`false` to `boolean`), so this union stays
      // assignable to the modal's { ok: true } | { ok: false; reason } prop.
      if (res.ok && res.json?.ok !== false) return { ok: true as const, json: res.json };
      const body = res.json as WorktreeRemoveFailure | null;
      return {
        ok: false as const,
        reason: reasonOf(res, `remove failed (${res.status})`),
        blocked_paths: Array.isArray(body?.blocked_paths)
          ? body.blocked_paths.filter((p): p is string => typeof p === 'string')
          : undefined,
        blocked_owner: typeof body?.blocked_owner === 'string' ? body.blocked_owner : undefined,
        fix_command: typeof body?.fix_command === 'string' ? body.fix_command : undefined,
      };
    },
    [],
  );

  const wtCount = Array.isArray(worktrees) ? worktrees.length : 0;
  const wtHazard = (worktrees ?? []).some(
    (w) => w.verdict === 'has-work' || w.verdict === 'unknown',
  );

  return {
    worktrees,
    wtLoading,
    wtErr,
    wtSupported,
    loadWorktrees,
    removeWorktree: removeWt,
    wtCount,
    wtHazard,
  };
}
