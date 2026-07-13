import { useCallback, useEffect, useRef, useState } from 'react';

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
  const [clearNote, setClearNote] = useState(null);
  const clearTimer = useRef(null);

  // Stable identity (refs + setters only) so the memoized action reporters that
  // depend on it don't churn every render.
  const showNote = useCallback((note, ms) => {
    clearTimeout(clearTimer.current);
    setClearNote(note);
    if (ms) clearTimer.current = setTimeout(() => setClearNote(null), ms);
  }, []);

  useEffect(() => () => clearTimeout(clearTimer.current), []);

  return { clearNote, setClearNote, showNote };
}
