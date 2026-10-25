import { useSyncExternalStore } from 'react';

/** One terminal of the dock: a human shell or the provider TUI of a session (DL-140). */
export type TermTab = {
  readonly id: string;
  readonly station: string;
  readonly shellId: string;
  readonly kind: 'shell' | 'tui';
  readonly title: string;
  readonly session: { readonly station_id: string; readonly session_id: string } | null;
};

export type DockState = {
  readonly tabs: readonly TermTab[];
  readonly active: string | null;
  readonly open: boolean;
  /** The panel height in px (layout 'dock'). */
  readonly height: number;
};

export type TerminalStore = {
  getState(): DockState;
  subscribe(cb: () => void): () => void;
  /** Add a tab (an existing tab with the same id stays), make it active, open the dock. */
  add(tab: TermTab): void;
  remove(id: string): void;
  activate(id: string): void;
  setOpen(open: boolean): void;
  /** Clamped to 160..max. */
  setHeight(px: number, max?: number): void;
};

export const STORAGE_KEY = 'fd-terminals';
export const MIN_HEIGHT = 160;
export const DEFAULT_HEIGHT = 320;
export const DEFAULT_MAX_HEIGHT = 640;

const DEFAULTS: DockState = { tabs: [], active: null, open: false, height: DEFAULT_HEIGHT };

/** The id of a tab: `${station}/${shellId}`. */
export const tabId = (station: string, shellId: string): string => `${station}/${shellId}`;

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

function tabOf(v: unknown): TermTab | null {
  if (!record(v)) return null;
  const { id, station, shellId, kind, title, session } = v;
  if (
    typeof id !== 'string' ||
    typeof station !== 'string' ||
    typeof shellId !== 'string' ||
    (kind !== 'shell' && kind !== 'tui') ||
    typeof title !== 'string'
  ) {
    return null;
  }
  if (session === null) return { id, station, shellId, kind, title, session: null };
  if (!record(session)) return null;
  const { station_id, session_id } = session;
  if (typeof station_id !== 'string' || typeof session_id !== 'string') return null;
  return { id, station, shellId, kind, title, session: { station_id, session_id } };
}

function read(storage: Storage | null): DockState {
  if (storage === null) return DEFAULTS;
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (raw === null) return DEFAULTS;
    const v: unknown = JSON.parse(raw);
    if (!record(v) || !Array.isArray(v['tabs'])) return DEFAULTS;
    const tabs = v['tabs'].map(tabOf);
    if (tabs.some((t) => t === null)) return DEFAULTS;
    const list = tabs as TermTab[];
    const active = v['active'];
    const height = v['height'];
    return {
      tabs: list,
      active: typeof active === 'string' && list.some((t) => t.id === active) ? active : null,
      open: v['open'] === true && list.length > 0,
      height:
        typeof height === 'number' && Number.isFinite(height)
          ? Math.max(MIN_HEIGHT, Math.round(height))
          : DEFAULT_HEIGHT,
    };
  } catch {
    return DEFAULTS;
  }
}

export function createTerminalStore(storage: Storage | null): TerminalStore {
  let state = read(storage);
  const listeners = new Set<() => void>();
  const set = (next: DockState) => {
    state = next;
    if (storage !== null) {
      try {
        storage.setItem(STORAGE_KEY, JSON.stringify(next));
      } catch {
        // A full or blocked storage: the dock then lasts this page only.
      }
    }
    for (const cb of [...listeners]) cb();
  };
  return {
    getState: () => state,
    subscribe(cb) {
      listeners.add(cb);
      return () => {
        listeners.delete(cb);
      };
    },
    add(tab) {
      const tabs = state.tabs.some((t) => t.id === tab.id) ? state.tabs : [...state.tabs, tab];
      set({ ...state, tabs, active: tab.id, open: true });
    },
    remove(id) {
      const at = state.tabs.findIndex((t) => t.id === id);
      if (at < 0) return;
      const tabs = state.tabs.filter((t) => t.id !== id);
      let active = state.active;
      if (active === id) active = (tabs[at] ?? tabs[at - 1])?.id ?? null;
      set({ ...state, tabs, active, open: tabs.length === 0 ? false : state.open });
    },
    activate(id) {
      if (!state.tabs.some((t) => t.id === id) || state.active === id) return;
      set({ ...state, active: id });
    },
    setOpen(open) {
      if (state.open === open) return;
      set({ ...state, open });
    },
    setHeight(px, max = DEFAULT_MAX_HEIGHT) {
      const raw = Number.isFinite(px) ? px : DEFAULT_HEIGHT;
      const height = Math.max(MIN_HEIGHT, Math.min(max, Math.round(raw)));
      if (height === state.height) return;
      set({ ...state, height });
    },
  };
}

export function useDock(store: TerminalStore): DockState {
  return useSyncExternalStore(store.subscribe, store.getState, store.getState);
}
