// DL-149: the archived (cleared) sessions of the board live on the deck, so
// every device shows the same board. The browser keeps a cache in
// localStorage for the first paint; every answer of the deck replaces the
// set. Before `attach` (tests, no deck) the set works locally only.
import { useSyncExternalStore } from 'react';
import type { ArchiveChange, ArchiveItem } from '../gen';
import type { LocalSet } from './localSet';

const CACHE_KEY = 'fd-archived-cache';
/** The key of the per-browser set before DL-149; its refs move to the deck once. */
const LEGACY_KEY = 'fd-archived';
const DEVICE_KEY = 'fd-device-id';
const PATH = '/api/archive';
const REFRESH_MS = 60_000;
/** The most items of one `ArchiveChange` (the deck's limit). */
const MAX_CHANGES = 500;

export type ArchiveApi = {
  get(path: string): Promise<unknown>;
  post(path: string, body: unknown): Promise<unknown>;
};

export type Archive = LocalSet & {
  /** Loads the deck's list at once, on window focus and every 60 s. Returns the detach function. */
  attach(api: ArchiveApi): () => void;
};

const storage = (): Storage | null => {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
};

const readRefs = (key: string): string[] => {
  try {
    const raw = storage()?.getItem(key);
    const value: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(value) ? value.filter((v): v is string => typeof v === 'string') : [];
  } catch {
    return [];
  }
};

/** `<station_id>/<session_id>` split at the first slash; null for any other text. */
const splitRef = (ref: string): ArchiveItem | null => {
  const i = ref.indexOf('/');
  if (i <= 0 || i === ref.length - 1) return null;
  return { station_id: ref.slice(0, i), session_id: ref.slice(i + 1), archived: true };
};

const randomId = (): string =>
  typeof globalThis.crypto?.randomUUID === 'function'
    ? globalThis.crypto.randomUUID()
    : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 12)}`;

let memoryDeviceId: string | null = null;
/** A random id of this browser, kept in localStorage. */
const deviceId = (): string => {
  try {
    const kept = storage()?.getItem(DEVICE_KEY);
    if (kept) return kept;
    const made = randomId();
    storage()?.setItem(DEVICE_KEY, made);
    return made;
  } catch {
    memoryDeviceId ??= randomId();
    return memoryDeviceId;
  }
};

const sha256Hex16 = async (text: string): Promise<string> => {
  const digest = await globalThis.crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  return [...new Uint8Array(digest)]
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
    .slice(0, 16);
};

/** The refs of a deck answer; a bad answer throws. */
function parseList(v: unknown): string[] {
  const list = typeof v === 'object' && v !== null ? (v as { archived?: unknown }).archived : null;
  if (!Array.isArray(list)) throw new Error('The deck sent an invalid archive list.');
  return list.map((r: unknown) => {
    const o = (typeof r === 'object' && r !== null ? r : {}) as Record<string, unknown>;
    if (typeof o['station_id'] !== 'string' || typeof o['session_id'] !== 'string') {
      throw new Error('The deck sent an invalid archive list.');
    }
    return `${o['station_id']}/${o['session_id']}`;
  });
}

export function createArchive(): Archive {
  let current: ReadonlySet<string> = new Set(readRefs(CACHE_KEY));
  let api: ArchiveApi | null = null;
  let migrating = false;
  const listeners = new Set<() => void>();
  const set = (next: ReadonlySet<string>) => {
    current = next;
    try {
      storage()?.setItem(CACHE_KEY, JSON.stringify([...next]));
    } catch {
      // The set stays in memory.
    }
    for (const l of listeners) l();
  };
  const subscribe = (l: () => void) => {
    listeners.add(l);
    return () => listeners.delete(l);
  };

  const load = async (): Promise<void> => {
    const target = api;
    if (target === null) return;
    const refs = parseList(await target.get(PATH));
    if (api !== target) return;
    set(new Set(refs));
    await migrate(target, refs);
  };

  /** Moves the refs of the old per-browser set to the deck, once. */
  const migrate = async (target: ArchiveApi, deckRefs: readonly string[]): Promise<void> => {
    if (migrating) return;
    const old = [...new Set(readRefs(LEGACY_KEY).filter((r) => splitRef(r) !== null))].sort();
    if (old.length === 0 && storage()?.getItem(LEGACY_KEY) === null) return;
    migrating = true;
    try {
      const missing = old.filter((r) => !deckRefs.includes(r));
      if (missing.length > 0) {
        const hash = await sha256Hex16(old.join('\n'));
        const all = missing.flatMap((r) => splitRef(r) ?? []);
        for (let i = 0; i < all.length; i += MAX_CHANGES) {
          const change: ArchiveChange = {
            mutation_id: `migrate-${deviceId()}-${hash}${i === 0 ? '' : `-${i / MAX_CHANGES}`}`,
            changes: all.slice(i, i + MAX_CHANGES),
          };
          const answer = parseList(await target.post(PATH, change));
          if (api === target) set(new Set(answer));
        }
      }
      storage()?.removeItem(LEGACY_KEY);
    } catch (e) {
      console.warn('The archived sessions of this browser did not move to the deck yet.', e);
    } finally {
      migrating = false;
    }
  };

  const reload = () => load().catch((e) => console.warn('The deck archive list did not load.', e));

  /** The optimistic change, then one `ArchiveChange` to the deck. */
  const change = (items: readonly ArchiveItem[]) => {
    if (items.length === 0) return;
    const next = new Set(current);
    for (const i of items) {
      const ref = `${i.station_id}/${i.session_id}`;
      if (i.archived) next.add(ref);
      else next.delete(ref);
    }
    set(next);
    const target = api;
    if (target === null) return;
    // The deck takes at most MAX_CHANGES items in one change.
    for (let i = 0; i < items.length; i += MAX_CHANGES) {
      const body: ArchiveChange = {
        mutation_id: randomId(),
        changes: items.slice(i, i + MAX_CHANGES),
      };
      target.post(PATH, body).then(
        (answer) => {
          if (api === target) set(new Set(parseList(answer)));
        },
        (e) => {
          console.warn('The archive change did not reach the deck.', e);
          void reload();
        },
      );
    }
  };
  const items = (refs: readonly string[], archived: boolean): ArchiveItem[] =>
    refs.flatMap((r) => {
      const item = splitRef(r);
      return item === null ? [] : [{ ...item, archived }];
    });

  let detachCurrent: (() => void) | null = null;
  return {
    has: (ref) => current.has(ref),
    toggle: (ref) => change(items([ref], !current.has(ref))),
    add: (refs) => change(items(refs, true)),
    remove: (refs) => change(items(refs, false)),
    use: () =>
      useSyncExternalStore(
        subscribe,
        () => current,
        () => current,
      ),
    attach: (next) => {
      detachCurrent?.();
      api = next;
      void reload();
      const timer = setInterval(() => void reload(), REFRESH_MS);
      const onFocus = () => void reload();
      const win = typeof window === 'undefined' ? null : window;
      win?.addEventListener('focus', onFocus);
      const detach = () => {
        clearInterval(timer);
        win?.removeEventListener('focus', onFocus);
        if (api === next) api = null;
        if (detachCurrent === detach) detachCurrent = null;
      };
      detachCurrent = detach;
      return detach;
    },
  };
}

/** Ended or lost sessions the owner cleared from the board; a live session always shows. */
export const archived: Archive = createArchive();
