import { useEffect } from 'react';
import { getQuestion } from '../qbus.ts';
import type { QuestionEntry } from '../../../contracts/index.ts';

// A ref-like open-state entry. `current` is OPTIONAL/unknown on purpose: this
// guard accepts anything threaded into its list and probes for `current` at
// runtime, so the structural view must let that probe be honest (an entry may
// not carry `current` at all — a plain boolean does not).
interface OverlayRef {
  current?: unknown;
}
// What blockingOverlayOpen accepts per entry: a ref-like, a plain boolean/value,
// or a nullish (ignored) slot.
type OverlayEntry = OverlayRef | boolean | null | undefined;

// The four overlays that reach this hook directly are real refs whose `.current`
// is read unguarded (they are always threaded), so `current` is required here.
interface BoolRef {
  current: boolean;
}

// The Compose payload the `c` hotkey opens with.
interface ComposeArg {
  target: string;
}

// State setters this hook only ever RESETS (called with null) or TOGGLES.
type ResetFn = (v: null) => void;
type ToggleFn = (v: boolean) => void;

interface UseBoardHotkeysArgs {
  pendingQs: readonly QuestionEntry[];
  selQ: string | null;
  setSelQ: (id: string) => void;
  gridOpen: BoolRef;
  killOpen: BoolRef;
  armOpen: BoolRef;
  renameOpen: BoolRef;
  fsOpen: boolean;
  setKillAsk: ResetFn;
  setArmAsk: ResetFn;
  setRenameAsk: ResetFn;
  setDrawerSid: ResetFn;
  setCompose: (v: ComposeArg | null) => void;
  setSpawnForm: ResetFn;
  setLanOpen: ToggleFn;
  setWtOpen: ToggleFn;
  setFsView: ResetFn;
  setHelpOpen: ToggleFn;
  composeOpen: OverlayRef | null | undefined;
  spawnOpen: OverlayRef | null | undefined;
  lanOpen: OverlayRef | null | undefined;
  wtOpen: OverlayRef | null | undefined;
  helpOpen: OverlayRef | null | undefined;
  drawerOpen: OverlayRef | null | undefined;
}

// The board's global keyboard shortcuts (the canonical human-readable list is
// HOTKEYS in board/src/helpText.js — shown by the "?" overlay; keep both in
// the same commit):
//   j / k · ↓ / ↑   move the inbox rail selection
//   y / n           answer the selected permission (allow / deny)
//   1-9             pick the n-th option of the selected choice
//   Enter           focus the selected freeform's answer box
//   c               open Compose (to all)
//   ?               open the help overlay
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
//
// M-F7 — a global answer/nav key must NEVER fire while a modal overlay owns the
// screen, for the SAME reason Esc is trapped above (M-F6 handles). With a
// permission SELECTED in the rail, opening a BUTTON-ONLY dialog (Kill /
// Move-to-tmux) leaves `typing` false, so an un-guarded `y` would route to
// h.allow?.() and SILENTLY approve the hidden tool instead of acting on the
// dialog. Symmetric to the Esc guard: Esc closes the topmost modal, every other
// global key (y/n · 1-9 · Enter · j/k · c) is a no-op under one.
//
// Pure + ref-based so a future test can import it, and so the stable keydown
// closure reads open-state synchronously (never a stale state snapshot, exactly
// like the Esc branch). Each entry is a ref-like { current }; nullish/undefined
// entries are simply ignored, so overlays whose ref hasn't been threaded yet
// don't block.
export function blockingOverlayOpen(overlays: readonly OverlayEntry[]): boolean {
  // Each entry is either a ref-like { current } (open when .current is truthy) or
  // a plain boolean/value (open when itself truthy). Accepting both keeps this
  // guard robust to how any given overlay exposes its open-state — a peer branch
  // threads a plain-boolean overlay flag into this same list — so a value dropped
  // in here can never silently fail to suppress. Nullish entries are ignored.
  return overlays.some((o) => {
    if (o == null) return false;
    if (typeof o === 'object' && 'current' in o) return !!o.current;
    return !!o;
  });
}

export function useBoardHotkeys({
  pendingQs,
  selQ,
  setSelQ,
  gridOpen,
  killOpen,
  armOpen,
  renameOpen,
  fsOpen,
  setKillAsk,
  setArmAsk,
  setRenameAsk,
  setDrawerSid,
  setCompose,
  setSpawnForm,
  setLanOpen,
  setWtOpen,
  setFsView,
  setHelpOpen,
  // Overlay refs. The four above (grid/kill/arm/rename) reach this hook directly;
  // compose/spawnForm/lan/wt/help are plain state in App, which mirrors each into
  // a ref (the same way useTermWindows mirrors killAsk→killOpen) and threads it
  // here, so ALL overlays suppress the answer/nav keys. blockingOverlayOpen still
  // ignores any entry that is nullish, so an unthreaded ref is simply inert.
  // fsOpen (the file viewer) is a plain boolean, which blockingOverlayOpen also
  // accepts.
  //
  // v2.6 — the FLOATING terminal window is deliberately absent from this hook:
  // it is non-modal, so with focus on the BOARD the hotkeys work (that is the
  // point of floating). Keys typed INTO the window never reach here — it stops
  // propagation itself, exactly as the old modal did. Only the GRID (the wall,
  // still full-screen) suppresses.
  composeOpen,
  spawnOpen,
  lanOpen,
  wtOpen,
  helpOpen,
  drawerOpen,
}: UseBoardHotkeysArgs) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = ((e.target as Element).tagName || '').toLowerCase();
      const typing = tag === 'input' || tag === 'textarea' || tag === 'select';
      // Esc NEVER touches a live terminal — the agent's TUI needs it. The grid
      // stops propagation itself; this guard covers stray focus too. The
      // floating window is NOT closed by Esc either (✕ / dock only): with board
      // focus, Esc falls through to the overlay chain below, and the window
      // stays — closing a terminal must always be a deliberate click.
      if (e.key === 'Escape') {
        if (gridOpen.current) return;
        // the kill / move-to-tmux / rename dialogs are modal over everything
        // else: Esc cancels the open one, and leaves the drawer it may have been
        // opened from standing (only one of the three is ever open at a time).
        // Rename is checked here, ABOVE the `typing` guard below, on purpose:
        // its dialog is a text input, and Esc from inside it must abandon the
        // rename rather than fall through to closing the drawer underneath.
        if (killOpen.current) {
          setKillAsk(null);
          return;
        }
        if (armOpen.current) {
          setArmAsk(null);
          return;
        }
        if (renameOpen.current) {
          setRenameAsk(null);
          return;
        }
        // the help overlay peels off alone, like the dialogs above it
        if (helpOpen?.current) {
          setHelpOpen(false);
          return;
        }
        // v2.2 — the file viewer opens OVER the drawer; Esc peels it off alone
        // so the drawer you launched it from is still there behind it. (Its
        // search box eats the first Esc itself when it holds a query.)
        if (fsOpen) {
          setFsView(null);
          return;
        }
        setDrawerSid(null);
        setCompose(null);
        setSpawnForm(null);
        setLanOpen(false);
        setWtOpen(false);
        return;
      }
      if (typing) return;
      // Modified chords are NEVER board hotkeys: Cmd/Ctrl+C is the user COPYING
      // (an unguarded 'c' here used to open Compose over their selection), and
      // Alt-chords belong to the browser/OS. Shift stays allowed — '?' IS
      // Shift+/ on most layouts.
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      // M-F7 — under an open modal, don't let y/n · 1-9 · Enter (or j/k · c · ?)
      // leak past the overlay; Esc above already owns the modal. Read the same
      // synchronously-read refs so a stale closure can't misroute an answer.
      // (v2.6: gridOpen replaced termOpen here — the floating terminal window
      // is non-modal by design and does not suppress.)
      if (
        blockingOverlayOpen([
          gridOpen,
          killOpen,
          armOpen,
          renameOpen,
          composeOpen,
          spawnOpen,
          lanOpen,
          wtOpen,
          fsOpen,
          helpOpen,
          drawerOpen,
        ])
      )
        return;
      const idx = pendingQs.findIndex((q) => q.id === selQ);
      if (e.key === 'j' || e.key === 'ArrowDown') {
        e.preventDefault();
        if (pendingQs.length) {
          const next =
            pendingQs[Math.min(pendingQs.length - 1, Math.max(0, idx) + (idx < 0 ? 0 : 1))];
          if (next) setSelQ(next.id);
        }
      } else if (e.key === 'k' || e.key === 'ArrowUp') {
        e.preventDefault();
        if (pendingQs.length) {
          const prev = pendingQs[Math.max(0, (idx < 0 ? 0 : idx) - 1)];
          if (prev) setSelQ(prev.id);
        }
      } else if (e.key === 'c') {
        e.preventDefault();
        setCompose({ target: 'all' });
      } else if (e.key === '?') {
        // Shift+/ arrives as key '?'; the `typing` guard above keeps a literal
        // "?" typed into an input from opening help.
        e.preventDefault();
        setHelpOpen(true);
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
        else if (q.kind === 'freeform' && e.key === 'Enter') {
          h.focusInput?.();
          e.preventDefault();
        }
      }
    };
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
    };
    // Setters (useState) and the gridOpen/killOpen/armOpen/renameOpen refs are
    // referentially stable, so this effect still only re-subscribes when the rail
    // selection changes (and when the file viewer opens/closes — fsOpen is a
    // plain boolean, not a ref).
  }, [
    pendingQs,
    selQ,
    gridOpen,
    killOpen,
    armOpen,
    renameOpen,
    fsOpen,
    composeOpen,
    spawnOpen,
    lanOpen,
    wtOpen,
    helpOpen,
    drawerOpen,
    setKillAsk,
    setArmAsk,
    setRenameAsk,
    setDrawerSid,
    setCompose,
    setSpawnForm,
    setLanOpen,
    setWtOpen,
    setFsView,
    setHelpOpen,
  ]);
}
