import { useCallback, useEffect, useRef, useState } from 'react';
import type { SessionEntry } from '../../../contracts/index.ts';

// One rendered strip note. Every field is optional: reporters send whichever
// subset fits (`{msg, orphans}`, `{err}`, `{hd, msg, url}`, …) and the strip
// renders what is present. `orphans` is the list of orphan-worktree paths the
// clear path surfaces (joined for display).
export interface FeedbackNote {
  hd?: string;
  msg?: string;
  err?: string;
  url?: string;
  orphans?: string[];
}

// The spawn-failure banner's state.
export interface SpawnFail {
  sid: string;
  callsign: string;
  note: string;
  detail: string | null;
  open: boolean;
}

// The shared feedback strip under the header: ONE strip, many reporters (Clear,
// revive, revive-all, enable-remote, kill). A reporter calls showNote(note, ms):
// ms=0 keeps the strip up until the human dismisses it (orphan paths, a
// claude.ai link, a list of failures — all need reading time); any positive ms
// auto-clears. Every showNote cancels the previous timer, so a newer note is
// never yanked off screen by an older note's countdown.
//
// note shapes rendered by the strip:
//   {hd?, msg, orphans?, url?}   success / info
//   {hd?, err}                   failure
export function useFeedbackStrip() {
  const [clearNote, setClearNote] = useState<FeedbackNote | null>(null);
  const clearTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Stable identity (refs + setters only) so the memoized action reporters that
  // depend on it don't churn every render.
  const showNote = useCallback((note: FeedbackNote, ms: number) => {
    clearTimeout(clearTimer.current ?? undefined);
    setClearNote(note);
    if (ms) clearTimer.current = setTimeout(() => { setClearNote(null); }, ms);
  }, []);

  useEffect(() => () => { clearTimeout(clearTimer.current ?? undefined); }, []);

  // 2.3 option 2 — the spawn-failure banner. Any card that TRANSITIONS to
  // offline wearing a "spawn failed:" note lands here: tombstones from THIS
  // tab's SpawnForm (its own in-form watch covers those too, but a failure
  // after the human closed the form must still surface) and failures from any
  // other source — a curl'd /api/spawn, a second board tab, a killed daemon
  // mid-clone. Dedup is by session_id in a ref: a card that stays offline
  // across frames must banner exactly once, so the detection effect may depend
  // on the whole sessions array and re-render loops can't re-fire it. Only
  // tombstones (a spawn row, gone or stalled) count — an offline transition
  // without one is just a session that ended, and bannering that would cry
  // wolf on every normal exit.
  const [spawnFail, setSpawnFail] = useState<SpawnFail | null>(null); // {sid, callsign, note, detail, open}
  const seenRef = useRef<Set<string>>(new Set());

  const watchSpawnFailures = useCallback(
    (
      sessions: SessionEntry[] | null | undefined,
      prevSessions: SessionEntry[] | null | undefined,
    ) => {
      const prev = new Map(
        (prevSessions ?? []).map((s): [string, SessionEntry] => [s.session_id, s]),
      );
      for (const s of sessions ?? []) {
        if (s.col !== 'offline') continue;
        if (typeof s.note !== 'string' || !s.note.startsWith('spawn failed:')) continue;
        const sp = s.spawn;
        if (!sp || (sp.status !== 'gone' && sp.status !== 'stalled')) continue;
        if (prev.get(s.session_id)?.col === 'offline') continue; // was already dead
        if (seenRef.current.has(s.session_id)) continue; // bannered already
        seenRef.current.add(s.session_id);
        setSpawnFail({
          sid: s.session_id,
          callsign: s.callsign || s.session_id,
          note: s.note,
          // Already redacted and bounded daemon-side (gitStderrDetail) — the
          // banner is a NEW audience for it, not a new control on it. Remote
          // stderr is adversarial text; it renders as a text node, like the card's
          // own expander, and is labeled git's words, not ours.
          detail: (sp.fail_detail ?? '') || null,
          open: false,
        });
      }
    },
    [],
  );

  const dismissSpawnFail = useCallback(() => { setSpawnFail(null); }, []);
  const toggleSpawnFailDetail = useCallback(
    () => { setSpawnFail((f) => (f ? { ...f, open: !f.open } : f)); },
    [],
  );

  return {
    clearNote,
    setClearNote,
    showNote,
    spawnFail,
    watchSpawnFailures,
    dismissSpawnFail,
    toggleSpawnFailDetail,
  };
}
