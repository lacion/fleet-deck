// DL-189: the earlier history of an imported session (design session-history §3.7). The loader
// holds the pages; the hook only wraps it, so a test can drive the loader without a DOM.
import { useEffect, useMemo, useRef, useSyncExternalStore } from 'react';
import type { HistoryItem, HistoryUnavailable, SessionHistory } from '../gen';
import { ApiError } from '../runtime/api';

/** One muted line under the history block; `retry` shows a "Try again" button. */
export type HistoryLine = { readonly text: string; readonly action: 'retry' | null };

export type HistorySnapshot = {
  /** Oldest first, all loaded pages. */
  readonly items: readonly HistoryItem[];
  /** The last answer had a non-null `before`. */
  readonly more: boolean;
  readonly loading: boolean;
  readonly line: HistoryLine | null;
};

export type HistoryView = HistorySnapshot & { loadEarlier(): void; retry(): void };

export type HistoryLoad = (before: string | null) => Promise<SessionHistory>;

const EMPTY: HistorySnapshot = { items: [], more: false, loading: false, line: null };

/** The view of a session without history (no reader). */
export const NO_HISTORY: HistoryView = { ...EMPTY, loadEarlier: () => {}, retry: () => {} };

const BUSY = 'The station is busy. Try again in a moment.';
const GONE = 'The earlier history is not available: ';

/** DL-189: the answer of the deck has the shape of `SessionHistory`. */
export function isSessionHistory(v: unknown): v is SessionHistory {
  if (typeof v !== 'object' || v === null || Array.isArray(v)) return false;
  const o = v as Record<string, unknown>;
  const state = o['state'];
  return (
    Array.isArray(o['items']) &&
    (o['before'] === null || typeof o['before'] === 'string') &&
    typeof state === 'object' &&
    state !== null &&
    typeof (state as Record<string, unknown>)['kind'] === 'string'
  );
}

/** The plain words for a reason that the history is not available. */
export function unavailableText(reason: HistoryUnavailable): HistoryLine {
  switch (reason) {
    case 'no_native_id':
      return {
        text: `${GONE}the session has no provider session id.`,
        action: null,
      };
    case 'not_found':
      return { text: `${GONE}the transcript file was not found.`, action: null };
    case 'ambiguous':
      return { text: `${GONE}more than one transcript file matches.`, action: null };
    case 'not_trusted':
      return { text: `${GONE}the transcript file failed a safety check.`, action: null };
    case 'too_large':
      return { text: `${GONE}the transcript file is too large.`, action: null };
    case 'unsupported_format':
      return { text: `${GONE}the transcript format is not known.`, action: null };
    case 'not_captured':
      return {
        text: 'The earlier history of this session is shown to the owner only.',
        action: null,
      };
    case 'busy':
      return { text: BUSY, action: 'retry' };
  }
}

/** The line for a failed request; `null` when the caller may not read the history (no line at all). */
export function historyErrorLine(e: unknown): HistoryLine | null {
  const code = e instanceof ApiError ? e.code : null;
  if (code === 'history_not_shared' || code === 'owner_only') return null;
  if (code === 'history_unsupported') {
    return {
      text: 'This station runs an older Fleet Deck. On that computer, install the new version, then run `fleetdeck service install`.',
      action: null,
    };
  }
  if (code === 'stale_cursor') {
    return { text: 'The history changed. Load it again from the start.', action: 'retry' };
  }
  if (code === 'busy') return { text: BUSY, action: 'retry' };
  const message = e instanceof Error ? e.message : 'The request failed.';
  return { text: `The earlier history could not be loaded: ${message}`, action: 'retry' };
}

/** The muted line for a `partial` or `unavailable` state; `null` for the others. */
export function historyStateLine(state: SessionHistory['state']): HistoryLine | null {
  if (state.kind === 'partial') {
    return { text: 'Some lines of the transcript could not be read.', action: null };
  }
  if (state.kind === 'unavailable') return unavailableText(state.reason);
  return null;
}

/** The pages of one session's history. Answers that arrive after `dispose` or a `retry` are dropped. */
export class HistoryLoader {
  private snap: HistorySnapshot = EMPTY;
  private readonly listeners = new Set<() => void>();
  private cursor: string | null = null;
  private started = false;
  private disposed = false;
  private token = 0;

  constructor(private readonly load: HistoryLoad) {}

  getSnapshot = (): HistorySnapshot => this.snap;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /** The first page, once. A start after `dispose` takes the loader back (a remount in StrictMode). */
  start(): void {
    this.disposed = false;
    if (this.started) return;
    this.started = true;
    this.fetch(null, true);
  }

  /** The next older page; nothing while a page loads or when there is none. */
  loadEarlier(): void {
    if (this.disposed || !this.started || this.snap.loading || !this.snap.more) return;
    this.fetch(this.cursor, false);
  }

  /** The first page again; the loaded items go. */
  retry(): void {
    if (this.disposed || !this.started) return;
    this.fetch(null, true);
  }

  dispose(): void {
    this.disposed = true;
  }

  private set(next: HistorySnapshot): void {
    this.snap = next;
    for (const listener of [...this.listeners]) listener();
  }

  private stale(token: number): boolean {
    return this.disposed || token !== this.token;
  }

  private fetch(before: string | null, reset: boolean): void {
    this.token += 1;
    const token = this.token;
    this.set({ ...(reset ? EMPTY : this.snap), loading: true, line: null });
    this.load(before).then(
      (page) => {
        if (this.stale(token)) return;
        this.cursor = page.before;
        this.set({
          items: reset ? page.items : [...page.items, ...this.snap.items],
          more: page.before !== null,
          loading: false,
          line: historyStateLine(page.state),
        });
      },
      (error: unknown) => {
        if (this.stale(token)) return;
        const line = historyErrorLine(error);
        // A caller who may not read the history sees nothing more (no block, no button).
        this.set({
          ...this.snap,
          more: line === null ? false : this.snap.more,
          loading: false,
          line,
        });
      },
    );
  }
}

/**
 * The history view of one session. The first page is asked once, the first time `open` is true.
 * A new `sessionKey` starts a new loader; `load === null` gives an empty view.
 */
export function useSessionHistory(
  load: HistoryLoad | null,
  sessionKey: string,
  open: boolean,
): HistoryView {
  // `actions` is a new object on every render: the loader reads the latest `load` and is not rebuilt.
  const latest = useRef(load);
  latest.current = load;
  const enabled = load !== null;
  const loader = useMemo(
    () =>
      enabled
        ? new HistoryLoader((before) => {
            const current = latest.current;
            return current === null ? Promise.reject(new Error('No reader.')) : current(before);
          })
        : null,
    [sessionKey, enabled],
  );
  useEffect(() => () => loader?.dispose(), [loader]);
  useEffect(() => {
    if (open) loader?.start();
  }, [loader, open]);
  const snap = useSyncExternalStore(
    loader === null ? NEVER : loader.subscribe,
    loader === null ? NONE : loader.getSnapshot,
    loader === null ? NONE : loader.getSnapshot,
  );
  return useMemo(
    () =>
      loader === null
        ? NO_HISTORY
        : { ...snap, loadEarlier: () => loader.loadEarlier(), retry: () => loader.retry() },
    [loader, snap],
  );
}

const NEVER = (): (() => void) => () => {};
const NONE = (): HistorySnapshot => EMPTY;
