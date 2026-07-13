import { useEffect } from 'react';
import { getQuestion } from '../qbus.js';

// The board's global keyboard shortcuts:
//   j / k · ↓ / ↑   move the inbox rail selection
//   y / n           answer the selected permission (allow / deny)
//   1-9             pick the n-th option of the selected choice
//   Enter           focus the selected freeform's answer box
//   c               open Compose (to all)
//   Esc             close the topmost overlay
//
// M-F6 — the answer keys reach the selected card through its registered
// imperative handle (qbus.getQuestion), NEVER document.querySelector('.fd-allow')
// & friends. A renamed CSS class can no longer silently kill y/n/1-9; a card
// that isn't mounted simply has no handle and the key is a no-op, exactly as a
// missing element used to be.
//
// termOpen / killOpen are refs (from useTermWindows) read synchronously so a
// stale closure over state can't misroute Esc.
export function useBoardHotkeys({
  pendingQs, selQ, setSelQ, termOpen, killOpen,
  setKillAsk, setDrawerSid, setCompose, setSpawnForm, setLanOpen, setWtOpen,
}) {
  useEffect(() => {
    const onKey = (e) => {
      const tag = (e.target.tagName || '').toLowerCase();
      const typing = tag === 'input' || tag === 'textarea' || tag === 'select';
      // Esc NEVER touches the live terminal — the agent's TUI needs it. The
      // modal stops propagation itself; this guard covers stray focus too.
      if (e.key === 'Escape') {
        if (termOpen.current) return;
        // the kill dialog is modal over everything else: Esc cancels IT, and
        // leaves the drawer it may have been opened from standing
        if (killOpen.current) { setKillAsk(null); return; }
        setDrawerSid(null); setCompose(null); setSpawnForm(null); setLanOpen(false);
        setWtOpen(false);
        return;
      }
      if (typing) return;
      const idx = pendingQs.findIndex((q) => q.id === selQ);
      if (e.key === 'j' || e.key === 'ArrowDown') {
        e.preventDefault();
        if (pendingQs.length) setSelQ(pendingQs[Math.min(pendingQs.length - 1, Math.max(0, idx) + (idx < 0 ? 0 : 1))].id);
      } else if (e.key === 'k' || e.key === 'ArrowUp') {
        e.preventDefault();
        if (pendingQs.length) setSelQ(pendingQs[Math.max(0, (idx < 0 ? 0 : idx) - 1)].id);
      } else if (e.key === 'c') {
        e.preventDefault();
        setCompose({ target: 'all' });
      } else {
        const q = pendingQs[idx];
        if (!q) return;
        // M-F6 — reach the selected card through its registered imperative
        // handle, not document.querySelector('.fd-allow') etc. A renamed CSS
        // class can no longer silently kill y/n/1-9.
        const h = getQuestion(q.id);
        if (!h) return;
        if (q.kind === 'permission' && e.key === 'y') h.allow?.();
        else if (q.kind === 'permission' && e.key === 'n') h.deny?.();
        else if (q.kind === 'choice' && /^[1-9]$/.test(e.key)) h.choose?.(Number(e.key));
        else if (q.kind === 'freeform' && e.key === 'Enter') { h.focusInput?.(); e.preventDefault(); }
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
    // Setters (useState) and the termOpen/killOpen refs are referentially stable,
    // so this effect still only re-subscribes when the rail selection changes.
  }, [pendingQs, selQ, termOpen, killOpen, setKillAsk, setDrawerSid, setCompose, setSpawnForm, setLanOpen, setWtOpen]);
}
