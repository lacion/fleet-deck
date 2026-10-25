import { createHash } from 'node:crypto';
import { afterEach, describe, expect, test } from 'bun:test';
import { getNotes, type Note, notesStorageKey, resetNotes, setNotes } from '../chat/notes';
import type { NewComment, ReviewComment, SessionKey } from '../gen';
import {
  importFailureText,
  importKey,
  importNotes,
  importPrompt,
  sha256Hex,
  unsentNotes,
} from './importNotes';

const key: SessionKey = { station_id: 'st', session_id: 's1' };

const notes: Note[] = [
  { file: 'a.ts', line: 3, text: 'rename it', sent: false },
  { file: 'b.ts', line: 9, text: 'old and sent', sent: true },
  { file: 'c.ts', line: 1, text: 'why?', sent: false },
];

const comment = {} as ReviewComment;

describe('the import key', () => {
  test('it is the lower-case hex SHA-256 of the spec string', async () => {
    const note = notes[0] as Note;
    const spec = 'st/s1/a.ts/3/rename it';
    const expected = createHash('sha256').update(spec).digest('hex');
    expect(await importKey('st', 's1', note)).toBe(expected);
    expect(expected).toMatch(/^[0-9a-f]{64}$/);
    expect(await sha256Hex('abc')).toBe(
      'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
    );
  });
});

describe('importNotes', () => {
  test('every unsent note is added in order and the local notes are cleared', async () => {
    const sent: NewComment[] = [];
    let cleared = 0;
    const result = await importNotes({
      notes,
      station: 'st',
      session: 's1',
      key,
      snapshot: 'spawn:base',
      add: async (_k, body) => {
        sent.push(body);
        return comment;
      },
      clear: () => {
        cleared += 1;
      },
    });
    expect(result).toEqual({ imported: 2, total: 2, error: null });
    expect(cleared).toBe(1);
    expect(sent).toHaveLength(2);
    expect(sent[0]).toEqual({
      snapshot: 'spawn:base',
      path: 'a.ts',
      side: 'new',
      line_start: 3,
      line_end: 3,
      body: 'rename it',
      import_key: createHash('sha256').update('st/s1/a.ts/3/rename it').digest('hex'),
    });
    expect(sent[1]?.path).toBe('c.ts');
  });

  test('one failure keeps the local notes and reports the first error', async () => {
    let cleared = 0;
    let calls = 0;
    const result = await importNotes({
      notes,
      station: 'st',
      session: 's1',
      key,
      snapshot: 'spawn:base',
      add: async () => {
        calls += 1;
        if (calls === 1) throw new Error('the deck is busy');
        return comment;
      },
      clear: () => {
        cleared += 1;
      },
    });
    expect(result).toEqual({ imported: 1, total: 2, error: 'the deck is busy' });
    expect(cleared).toBe(0);
    expect(importFailureText(result)).toBe('Imported 1 of 2 notes. the deck is busy');
  });

  test('a repeat after a failure sends the same import keys', async () => {
    const keys: string[] = [];
    const add = async (_k: SessionKey, body: NewComment) => {
      keys.push(body.import_key ?? '');
      return comment;
    };
    const args = {
      notes,
      station: 'st',
      session: 's1',
      key,
      snapshot: 'spawn:base',
      add,
      clear: () => {},
    };
    await importNotes(args);
    await importNotes(args);
    expect(keys.slice(0, 2)).toEqual(keys.slice(2));
  });

  test('no unsent note means nothing to import', () => {
    expect(unsentNotes([{ file: 'a', line: 1, text: 't', sent: true }])).toEqual([]);
    expect(importPrompt(2)).toBe(
      'You have 2 local notes from an older version. Import them as draft comments?',
    );
  });
});

describe('the local notes key', () => {
  const store = new Map<string, string>();
  const storageKey = notesStorageKey('st', 's1');
  const install = () => {
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      value: {
        getItem: (k: string) => store.get(k) ?? null,
        setItem: (k: string, v: string) => void store.set(k, v),
        removeItem: (k: string) => void store.delete(k),
      },
    });
    setNotes(storageKey, notes);
  };
  afterEach(() => {
    store.clear();
    resetNotes();
    Reflect.deleteProperty(globalThis, 'localStorage');
  });
  const run = (add: () => Promise<ReviewComment>) =>
    importNotes({
      notes: getNotes(storageKey),
      station: 'st',
      session: 's1',
      key,
      snapshot: 'spawn:base',
      add,
      clear: () => setNotes(storageKey, []),
    });

  test('when every note is done the key is removed and the sent notes go with it', async () => {
    install();
    expect(store.has(storageKey)).toBe(true);
    await run(async () => comment);
    expect(store.has(storageKey)).toBe(false);
    expect(getNotes(storageKey)).toEqual([]);
  });

  test('with one failure the key stays unchanged', async () => {
    install();
    const before = store.get(storageKey);
    let calls = 0;
    const result = await run(async () => {
      calls += 1;
      if (calls === 2) throw new Error('boom');
      return comment;
    });
    expect(result.error).toBe('boom');
    expect(store.get(storageKey)).toBe(before);
    expect(getNotes(storageKey)).toHaveLength(3);
  });
});
