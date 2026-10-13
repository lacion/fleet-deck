// The phone's Back of a session pane: go back in the history when the board has navigated at
// least once in this page load, else (a deep link or a reload) go to the home list.

let navigations = 0;

/** Count one navigation (App calls it on every hash change). */
export function noteNavigation(): void {
  navigations += 1;
}

/** True when the board has navigated since the page loaded, so `history.back()` stays inside it. */
export function canGoBack(): boolean {
  return navigations > 0;
}

/** Tests only: forget the navigations. */
export function resetBackNav(): void {
  navigations = 0;
}
