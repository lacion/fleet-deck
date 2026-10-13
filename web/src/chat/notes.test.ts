import { afterEach, describe, expect, test } from 'bun:test';
import {
  type Note,
  getNotes,
  linePositions,
  notesMessage,
  notesStorageKey,
  resetNotes,
  setNotes,
  withNote,
} from './notes';

const patch = [
  'diff --git a/src/a.ts b/src/a.ts',
  'index 83db48f..bf2a1c3 100644',
  '--- a/src/a.ts',
  '+++ b/src/a.ts',
  '@@ -3,3 +10,4 @@ fn',
  ' keep',
  '-old',
  '+new',
  '+more',
  'diff --git a/b.txt b/b.txt',
  'new file mode 100644',
  '--- /dev/null',
  '+++ b/b.txt',
  '@@ -0,0 +1,1 @@',
  '+hello',
].join('\n');

describe('line notes', () => {
  test('a line of a patch has its file and its line in the new file', () => {
    const at = linePositions(patch);
    expect(at).toEqual([
      null,
      null,
      null,
      null,
      null,
      { file: 'src/a.ts', line: 10 },
      null,
      { file: 'src/a.ts', line: 11 },
      { file: 'src/a.ts', line: 12 },
      null,
      null,
      null,
      null,
      null,
      { file: 'b.txt', line: 1 },
    ]);
  });

  test('the message has one line per note', () => {
    const notes: Note[] = [
      { file: 'a.ts', line: 3, text: 'rename   this', sent: false },
      { file: 'b.ts', line: 9, text: 'why?', sent: true },
    ];
    expect(notesMessage(notes)).toBe('Review notes:\n- a.ts L3: rename this\n- b.ts L9: why?');
  });

  test('a note is set, replaced and removed by an empty text', () => {
    let notes: readonly Note[] = [];
    notes = withNote(notes, 'a.ts', 3, '  fix it ');
    expect(notes).toEqual([{ file: 'a.ts', line: 3, text: 'fix it', sent: false }]);
    notes = withNote(notes, 'a.ts', 3, 'fix it better');
    expect(notes).toHaveLength(1);
    expect(notes[0]?.text).toBe('fix it better');
    notes = withNote(notes, 'a.ts', 3, '   ');
    expect(notes).toEqual([]);
  });
});

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
