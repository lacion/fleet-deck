// M-F6 — the board's global hotkeys (y/n permission · 1-9 choice · Enter into a
// freeform · scroll-to on the NEEDS YOU chip) used to reach the inbox by
// document.querySelector('.fd-allow').click() and friends. That silently BREAKS
// the day someone renames a CSS class: the keys stop working with no error, no
// test, nothing.
//
// Instead each rendered QuestionCard registers a small imperative handle here,
// keyed by question id, and App's keydown handler calls the handle directly. No
// DOM class is load-bearing anymore; a card that isn't mounted simply has no
// handle and the key is a no-op, exactly as a missing element used to be.
//
// Handle shape (all optional / self-guarding):
//   allow()          permission → allow · plan → approve
//   deny()           permission/plan → deny
//   choose(n)        choice → act on the n-th option (1-based), as clicking it
//   focusInput()     freeform → focus the answer textarea
//   scrollIntoView() bring the card into view (NEEDS YOU chip)
const registry = new Map();

export function registerQuestion(id, handle) {
  registry.set(id, handle);
  return () => { if (registry.get(id) === handle) registry.delete(id); };
}

export function getQuestion(id) {
  return registry.get(id) || null;
}
