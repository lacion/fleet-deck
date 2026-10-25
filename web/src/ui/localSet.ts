// DL-141: small per-browser sets of session keys (priority stars), kept in
// localStorage. Every read and write survives a blocked or full storage: the
// set then lives in memory only.
import { useSyncExternalStore } from 'react';

/** The key of one session in a set: `<station_id>/<session_id>`. */
export const sessionRef = (station: string, session: string): string => `${station}/${session}`;

export type LocalSet = {
  has(ref: string): boolean;
  /** Adds the ref when it is absent, removes it when it is present. */
  toggle(ref: string): void;
  add(refs: readonly string[]): void;
  remove(refs: readonly string[]): void;
  /** The current set; re-renders the component on every change. */
  use(): ReadonlySet<string>;
};

export function localSet(storageKey: string): LocalSet {
  let current: ReadonlySet<string> = read(storageKey);
  const listeners = new Set<() => void>();
  const write = (next: Set<string>) => {
    current = next;
    try {
      globalThis.localStorage?.setItem(storageKey, JSON.stringify([...next]));
    } catch {
      // The set stays in memory.
    }
    for (const l of listeners) l();
  };
  const subscribe = (l: () => void) => {
    listeners.add(l);
    return () => listeners.delete(l);
  };
  return {
    has: (ref) => current.has(ref),
    toggle: (ref) => {
      const next = new Set(current);
      if (next.has(ref)) next.delete(ref);
      else next.add(ref);
      write(next);
    },
    add: (refs) => write(new Set([...current, ...refs])),
    remove: (refs) => {
      const next = new Set(current);
      for (const r of refs) next.delete(r);
      write(next);
    },
    use: () =>
      useSyncExternalStore(
        subscribe,
        () => current,
        () => current,
      ),
  };
}

function read(storageKey: string): ReadonlySet<string> {
  try {
    const raw = globalThis.localStorage?.getItem(storageKey);
    const value: unknown = raw ? JSON.parse(raw) : [];
    return new Set(Array.isArray(value) ? value.filter((v) => typeof v === 'string') : []);
  } catch {
    return new Set();
  }
}

/** Sessions the owner starred (v1 priority). */
export const priority = localSet('fd-priority');
