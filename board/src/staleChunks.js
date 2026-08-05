// Stale-chunk guard (BUG-083).
//
// The terminal views are React.lazy imports, so Vite emits them as hashed
// chunks that are fetched on first use. Every board build empties the output
// directory (vite.config.js: emptyOutDir), so a tab still running the previous
// entry holds chunk URLs that no longer exist after a daemon upgrade takes
// over. When the operator then opens a terminal, the dynamic import 404s, Vite
// fires `vite:preloadError`, and without a handler the rejection can unmount
// the whole React tree — terminal access fails until a manual reload.
//
// The fix: listen for `vite:preloadError` before the app renders and reload
// the shell once so it picks up the new entry and chunk hashes. The reload is
// rate-limited via sessionStorage so a genuinely broken deploy cannot put the
// tab into a reload loop.
//
// Pure ESM with the window object injected, so it loads under `node --test`
// with no bundler (same idiom as qr.js / util.js).

const STORAGE_KEY = 'fd-stale-chunk-reload-at';
// Ignore further preload errors for this long after a reload we triggered.
const RELOAD_GUARD_MS = 30_000;

// installStaleChunkGuard(win) -> teardown function.
// `win` is the browser window (or a test double with addEventListener /
// removeEventListener / location.reload / sessionStorage).
export function installStaleChunkGuard(win, { guardMs = RELOAD_GUARD_MS } = {}) {
  let reloading = false;

  function onPreloadError(event) {
    // Stop Vite from re-throwing the failed import as an uncaught error.
    event?.preventDefault?.();
    if (reloading) return;
    // Reload at most once per guard window: if the new entry also fails, the
    // stored timestamp from the previous attempt suppresses the loop.
    const now = Date.now();
    try {
      const last = Number(win.sessionStorage?.getItem(STORAGE_KEY) || 0);
      if (last && now - last < guardMs) return;
      win.sessionStorage?.setItem(STORAGE_KEY, String(now));
    } catch {
      // sessionStorage can throw (private mode, disabled storage) — the
      // in-memory `reloading` flag still bounds this page's lifetime.
    }
    reloading = true;
    win.location.reload();
  }

  win.addEventListener('vite:preloadError', onPreloadError);
  return () => win.removeEventListener('vite:preloadError', onPreloadError);
}
