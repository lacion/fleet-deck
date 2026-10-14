// R1.3: the old line notes on a diff. A note is one line of text on one line of a
// file. The notes of a session live in memory and in `localStorage`
// (`fd-notes:<station>/<session>`). Since DL-179 the board writes no new note: the Changes tab
// offers to import the unsent ones as draft review comments. Every read and write survives a
// blocked or full storage.
import { useSyncExternalStore } from 'react';

export type Note = {
  readonly file: string;
  /** The line number in the new version of the file. */
  readonly line: number;
  readonly text: string;
  /** A follow-up with this note was sent. */
  readonly sent: boolean;
};

/** The `localStorage` key of the notes of one session. */
export const notesStorageKey = (station: string, session: string): string =>
  `fd-notes:${station}/${session}`;

const isNote = (v: unknown): v is Note => {
  if (typeof v !== 'object' || v === null) return false;
  const n = v as Record<string, unknown>;
  return (
    typeof n['file'] === 'string' &&
    typeof n['line'] === 'number' &&
    typeof n['text'] === 'string' &&
    typeof n['sent'] === 'boolean'
  );
};

function read(storageKey: string): readonly Note[] {
  try {
    const raw = globalThis.localStorage?.getItem(storageKey);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? parsed.filter(isNote) : [];
  } catch {
    return [];
  }
}

const cache = new Map<string, readonly Note[]>();
const listeners = new Set<() => void>();

/** The notes of one session. */
export function getNotes(storageKey: string): readonly Note[] {
  const known = cache.get(storageKey);
  if (known !== undefined) return known;
  const loaded = read(storageKey);
  cache.set(storageKey, loaded);
  return loaded;
}

/** Replaces the notes of one session and keeps them in `localStorage`. */
export function setNotes(storageKey: string, notes: readonly Note[]): void {
  cache.set(storageKey, notes);
  try {
    if (notes.length === 0) globalThis.localStorage?.removeItem(storageKey);
    else globalThis.localStorage?.setItem(storageKey, JSON.stringify(notes));
  } catch {
    // The notes stay in memory.
  }
  for (const l of listeners) l();
}

/** Forgets the in-memory notes (tests). */
export function resetNotes(): void {
  cache.clear();
  for (const l of listeners) l();
}

/** The notes of one session; re-renders the component on every change. */
export function useNotes(storageKey: string): readonly Note[] {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => getNotes(storageKey),
    () => getNotes(storageKey),
  );
}
