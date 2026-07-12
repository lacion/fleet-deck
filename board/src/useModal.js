import { useEffect, useState } from 'react';

// M-A2 — one modal-behaviour hook for every dialog on the board: mark it
// aria-modal at the call site, and this handles the three things a modal owes a
// keyboard/AT user:
//   1. initial focus lands INSIDE the dialog (not left on whatever was behind);
//   2. Tab is trapped so focus can't wander out to the dead board underneath;
//   3. focus is RESTORED to the element that opened the dialog when it closes.
//
// Two knobs, because two dialogs are special:
//   · trap:false / initialFocus:false for the terminal modal + grid — a live
//     terminal must receive Tab itself (autocomplete) and xterm claims its own
//     focus, so we only keep focus-restore for those. Everything else traps.
//   · initialFocus:false where the dialog already parks focus deliberately
//     (Compose's textarea, SpawnForm's caret, KillConfirm's SAFE button).
const FOCUSABLE = [
  'a[href]',
  'button:not([disabled])',
  'textarea:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

export function useModal(ref, { trap = true, initialFocus = true } = {}) {
  // Capture the opener at FIRST render (a useState initializer runs once, before
  // any child commits) — so even a modal whose child grabs focus on mount (the
  // terminal's xterm) still restores to the element that opened it on close.
  const [opener] = useState(() => (typeof document !== 'undefined' ? document.activeElement : null));

  useEffect(() => {
    const node = ref.current;

    if (initialFocus && node) {
      const first = node.querySelector(FOCUSABLE);
      (first || node).focus?.();
    }

    const onKey = (e) => {
      if (e.key !== 'Tab' || !node) return;
      const items = [...node.querySelectorAll(FOCUSABLE)].filter(
        (el) => el.offsetParent !== null || el === document.activeElement,
      );
      if (!items.length) { e.preventDefault(); return; }
      const first = items[0];
      const last = items[items.length - 1];
      const active = document.activeElement;
      if (e.shiftKey) {
        if (active === first || !node.contains(active)) { e.preventDefault(); last.focus(); }
      } else if (active === last || !node.contains(active)) {
        e.preventDefault();
        first.focus();
      }
    };

    if (trap && node) node.addEventListener('keydown', onKey);
    return () => {
      if (trap && node) node.removeEventListener('keydown', onKey);
      // only steal focus back if the opener is still in the document
      if (opener && opener.focus && document.contains(opener)) opener.focus();
    };
  }, [ref, trap, initialFocus, opener]);
}
