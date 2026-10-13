// R1.3: line notes on a diff. A note is one line of text on one line of a
// file. The notes of a session live in memory and in `localStorage`
// (`fd-notes:<station>/<session>`); "Send notes" sends them as one follow-up.
// Every read and write survives a blocked or full storage.
import { useSyncExternalStore } from 'react';

export type Note = {
  readonly file: string;
  /** The line number in the new version of the file. */
  readonly line: number;
  readonly text: string;
  /** A follow-up with this note was sent. */
  readonly sent: boolean;
};

/** The most characters of one note. */
export const MAX_NOTE_CHARS = 500;

/** The `localStorage` key of the notes of one session. */
export const notesStorageKey = (station: string, session: string): string =>
  `fd-notes:${station}/${session}`;

/** Where each line of a unified diff lies: the file and the new line number, or `null` (no new line). */
export function linePositions(
  patch: string,
): readonly ({ readonly file: string; readonly line: number } | null)[] {
  let file: string | null = null;
  let next = 0;
  let inHunk = false;
  return patch.split('\n').map((line) => {
    if (line.startsWith('diff --git ')) {
      file = /^diff --git a\/.* b\/(.+)$/.exec(line)?.[1] ?? null;
      inHunk = false;
      return null;
    }
    const hunk = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
    if (hunk?.[1] !== undefined) {
      next = Number(hunk[1]);
      inHunk = true;
      return null;
    }
    if (!inHunk || file === null) return null;
    if (line.startsWith('+') || line.startsWith(' ')) {
      const at = { file, line: next };
      next += 1;
      return at;
    }
    return null;
  });
}

/** The follow-up text for `notes`: one line per note. */
export function notesMessage(notes: readonly Note[]): string {
  const lines = notes.map((n) => `- ${n.file} L${n.line}: ${n.text.replace(/\s+/g, ' ').trim()}`);
  return `Review notes:\n${lines.join('\n')}`;
}

/** `notes` with the note on `file` line `line` set to `text`; an empty text removes it. */
export function withNote(
  notes: readonly Note[],
  file: string,
  line: number,
  text: string,
): readonly Note[] {
  const rest = notes.filter((n) => !(n.file === file && n.line === line));
  const clean = text.replace(/\s+/g, ' ').trim().slice(0, MAX_NOTE_CHARS);
  if (clean === '') return rest;
  return [...rest, { file, line, text: clean, sent: false }];
}

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
