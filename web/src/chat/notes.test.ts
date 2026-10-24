import { afterEach, describe, expect, test } from 'bun:test';
import { getNotes, notesStorageKey, resetNotes, setNotes } from './notes';

describe('note storage', () => {
  const store = new Map<string, string>();
  const install = () =>
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      value: {
        getItem: (k: string) => store.get(k) ?? null,
        setItem: (k: string, v: string) => void store.set(k, v),
        removeItem: (k: string) => void store.delete(k),
      },
    });
  afterEach(() => {
    store.clear();
    resetNotes();
    Reflect.deleteProperty(globalThis, 'localStorage');
  });

  test('notes are kept per session in memory and in localStorage', () => {
    install();
    const key = notesStorageKey('st', 's1');
    expect(key).toBe('fd-notes:st/s1');
    expect(getNotes(key)).toEqual([]);
    setNotes(key, [{ file: 'a.ts', line: 1, text: 'x', sent: false }]);
    expect(JSON.parse(store.get(key) ?? '[]')).toHaveLength(1);
    // A fresh page reads them back; another session has none.
    resetNotes();
    expect(getNotes(key)).toHaveLength(1);
    expect(getNotes(notesStorageKey('st', 's2'))).toEqual([]);
    setNotes(key, []);
    expect(store.has(key)).toBe(false);
  });

  test('a blocked or broken storage leaves the notes in memory', () => {
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      value: {
        getItem: () => {
          throw new Error('blocked');
        },
        setItem: () => {
          throw new Error('blocked');
        },
        removeItem: () => {
          throw new Error('blocked');
        },
      },
    });
    const key = notesStorageKey('st', 's3');
    expect(getNotes(key)).toEqual([]);
    setNotes(key, [{ file: 'a.ts', line: 1, text: 'x', sent: false }]);
    expect(getNotes(key)).toHaveLength(1);
  });

  test('a stored value of another shape is ignored', () => {
    install();
    const key = notesStorageKey('st', 's4');
    store.set(key, JSON.stringify([{ file: 1 }, { file: 'a', line: 2, text: 't', sent: true }]));
    expect(getNotes(key)).toEqual([{ file: 'a', line: 2, text: 't', sent: true }]);
  });
});
