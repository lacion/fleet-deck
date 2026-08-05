// tests/board-stale-chunks.test.mjs
//
// Pure tests for the board's stale-chunk guard (board/src/staleChunks.js,
// BUG-083).
//
// TermWindow/TermGrid are React.lazy imports, emitted by Vite as hashed chunks
// that are fetched on first use. Every board build empties the output
// directory, so a tab still running the pre-upgrade entry requests hashes the
// new daemon no longer serves — the dynamic import 404s, Vite fires
// `vite:preloadError`, and the unhandled rejection can unmount the whole React
// tree until the operator manually reloads. The guard listens for that event,
// swallows the error, and reloads the shell once (rate-limited via
// sessionStorage so a broken deploy cannot loop).
//
// The module takes the window object as a parameter, so it loads under
// `node --test` with no bundler (same idiom as qr.js / util.js).

import test from 'node:test';
import assert from 'node:assert/strict';

import { installStaleChunkGuard } from '../board/src/staleChunks.js';

// Minimal window double: event registry, reload spy, and a sessionStorage
// polyfill backed by a Map.
function makeWindow() {
  const listeners = new Map();
  const storage = new Map();
  const win = {
    reloads: 0,
    addEventListener(type, fn) {
      if (!listeners.has(type)) listeners.set(type, []);
      listeners.get(type).push(fn);
    },
    removeEventListener(type, fn) {
      const arr = listeners.get(type) || [];
      const i = arr.indexOf(fn);
      if (i >= 0) arr.splice(i, 1);
    },
    dispatch(type, event = {}) {
      for (const fn of listeners.get(type) || []) fn(event);
    },
    location: {
      reload() {
        win.reloads += 1;
      },
    },
    sessionStorage: {
      getItem: (k) => (storage.has(k) ? storage.get(k) : null),
      setItem: (k, v) => storage.set(k, String(v)),
    },
  };
  return win;
}

function preloadErrorEvent() {
  let prevented = false;
  return {
    event: { preventDefault: () => { prevented = true; } },
    prevented: () => prevented,
  };
}

test('vite:preloadError triggers exactly one reload and swallows the error', () => {
  const win = makeWindow();
  installStaleChunkGuard(win);

  const e1 = preloadErrorEvent();
  win.dispatch('vite:preloadError', e1.event);
  assert.equal(win.reloads, 1, 'first preload error reloads the shell');
  assert.equal(e1.prevented(), true, 'error is preventDefaulted so it cannot unmount the tree');

  // A second preload error in the same page lifetime must not reload again.
  const e2 = preloadErrorEvent();
  win.dispatch('vite:preloadError', e2.event);
  assert.equal(win.reloads, 1, 'in-memory flag suppresses a second reload');
  assert.equal(e2.prevented(), true, 'second error is still swallowed');
});

test('a fresh page within the guard window does not reload again (loop protection)', () => {
  // Simulates the reloaded tab: same sessionStorage, new listeners.
  const win = makeWindow();
  installStaleChunkGuard(win);
  win.dispatch('vite:preloadError', preloadErrorEvent().event);
  assert.equal(win.reloads, 1);

  const win2 = makeWindow();
  win2.sessionStorage = win.sessionStorage; // sessionStorage survives reload
  installStaleChunkGuard(win2);
  win2.dispatch('vite:preloadError', preloadErrorEvent().event);
  assert.equal(win2.reloads, 0, 'stored timestamp suppresses a reload loop');
});

test('reload is allowed again after the guard window expires', () => {
  const win = makeWindow();
  installStaleChunkGuard(win, { guardMs: 1_000 });
  // Backdate the stored timestamp beyond the guard window.
  win.sessionStorage.setItem('fd-stale-chunk-reload-at', String(Date.now() - 60_000));
  win.dispatch('vite:preloadError', preloadErrorEvent().event);
  assert.equal(win.reloads, 1, 'stale timestamp means a genuine later failure still recovers');
});

test('unrelated errors are ignored; teardown removes the listener', () => {
  const win = makeWindow();
  const teardown = installStaleChunkGuard(win);

  win.dispatch('error', {});
  win.dispatch('unhandledrejection', {});
  assert.equal(win.reloads, 0, 'only vite:preloadError is handled');

  teardown();
  win.dispatch('vite:preloadError', preloadErrorEvent().event);
  assert.equal(win.reloads, 0, 'listener is gone after teardown');
});

test('works when sessionStorage throws (private mode)', () => {
  const win = makeWindow();
  win.sessionStorage = {
    getItem() { throw new Error('denied'); },
    setItem() { throw new Error('denied'); },
  };
  installStaleChunkGuard(win);
  win.dispatch('vite:preloadError', preloadErrorEvent().event);
  assert.equal(win.reloads, 1, 'storage failure still reloads once');
  win.dispatch('vite:preloadError', preloadErrorEvent().event);
  assert.equal(win.reloads, 1, 'in-memory flag bounds the page lifetime');
});
