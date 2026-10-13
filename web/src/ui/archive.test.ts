import { afterEach, beforeEach, describe, expect, spyOn, test } from 'bun:test';
import { createArchive, type ArchiveApi } from './archive';

let items: Map<string, string>;

beforeEach(() => {
  items = new Map();
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    value: {
      getItem: (k: string) => items.get(k) ?? null,
      setItem: (k: string, v: string) => void items.set(k, v),
      removeItem: (k: string) => void items.delete(k),
    },
  });
});

afterEach(() => {
  Reflect.deleteProperty(globalThis, 'localStorage');
});

const flush = async () => {
  for (let i = 0; i < 20; i++) await Promise.resolve();
  await new Promise((r) => setTimeout(r, 5));
};

type Posted = {
  mutation_id: string;
  changes: { station_id: string; session_id: string; archived: boolean }[];
};

/** A fake deck: one list, every post is applied unless `failPost` is set. */
function deck(initial: string[] = []) {
  const state = { refs: new Set(initial), failPost: false, failGet: false, posts: [] as Posted[] };
  const list = () => ({
    archived: [...state.refs].sort().map((r) => {
      const [station_id = '', session_id = ''] = r.split('/');
      return { station_id, session_id };
    }),
  });
  const api: ArchiveApi = {
    get: async () => {
      if (state.failGet) throw new Error('down');
      return list();
    },
    post: async (_p, body) => {
      const b = body as Posted;
      state.posts.push(b);
      if (state.failPost) throw new Error('down');
      for (const c of b.changes) {
        const ref = `${c.station_id}/${c.session_id}`;
        if (c.archived) state.refs.add(ref);
        else state.refs.delete(ref);
      }
      return list();
    },
  };
  return { state, api };
}

describe('archived sessions on the deck (DL-149)', () => {
  test('before attach the set works locally and keeps a cache', () => {
    const a = createArchive();
    a.toggle('st/s1');
    a.add(['st/s2', 'not-a-ref']);
    expect(a.has('st/s1')).toBe(true);
    expect(a.has('st/s2')).toBe(true);
    expect(a.has('not-a-ref')).toBe(false);
    a.remove(['st/s1']);
    expect(a.has('st/s1')).toBe(false);
    expect(JSON.parse(items.get('fd-archived-cache') ?? '[]')).toEqual(['st/s2']);
    expect(createArchive().has('st/s2')).toBe(true);
  });

  test('a toggle is optimistic, then the deck list wins', async () => {
    const d = deck(['st/other']);
    const a = createArchive();
    const detach = a.attach(d.api);
    await flush();
    expect(a.has('st/other')).toBe(true);
    // Another device unarchived `other` before this device posts.
    d.state.refs.delete('st/other');
    a.toggle('st/s1');
    expect(a.has('st/s1')).toBe(true);
    expect(a.has('st/other')).toBe(true);
    await flush();
    expect(a.has('st/s1')).toBe(true);
    expect(a.has('st/other')).toBe(false);
    expect(d.state.posts).toHaveLength(1);
    expect(d.state.posts[0]?.changes).toEqual([
      { station_id: 'st', session_id: 's1', archived: true },
    ]);
    expect(d.state.posts[0]?.mutation_id.length).toBeGreaterThanOrEqual(8);
    expect(JSON.parse(items.get('fd-archived-cache') ?? '[]')).toEqual(['st/s1']);
    detach();
  });

  test('one call posts one change with all refs and a new id each time', async () => {
    const d = deck();
    const a = createArchive();
    const detach = a.attach(d.api);
    await flush();
    a.add(['st/a', 'st/b']);
    a.remove(['st/a']);
    await flush();
    expect(d.state.posts).toHaveLength(2);
    expect(d.state.posts[0]?.changes).toHaveLength(2);
    expect(d.state.posts[0]?.mutation_id).not.toBe(d.state.posts[1]?.mutation_id);
    expect(a.has('st/b')).toBe(true);
    expect(a.has('st/a')).toBe(false);
    detach();
  });

  test('a failed post reloads the list and the optimistic change is lost', async () => {
    const warn = spyOn(console, 'warn').mockImplementation(() => {});
    const d = deck();
    const a = createArchive();
    const detach = a.attach(d.api);
    await flush();
    d.state.failPost = true;
    a.toggle('st/s1');
    expect(a.has('st/s1')).toBe(true);
    await flush();
    expect(a.has('st/s1')).toBe(false);
    expect(warn).toHaveBeenCalled();
    detach();
    warn.mockRestore();
  });

  test('the migration posts the missing refs once with a stable id and then clears the old key', async () => {
    items.set('fd-archived', JSON.stringify(['st/b', 'st/a', 'st/known']));
    const d = deck(['st/known']);
    const a = createArchive();
    const detach = a.attach(d.api);
    await flush();
    expect(d.state.posts).toHaveLength(1);
    const post = d.state.posts[0];
    expect(post?.mutation_id).toMatch(/^migrate-.+-[0-9a-f]{16}$/);
    expect(post?.changes.map((c) => c.session_id)).toEqual(['a', 'b']);
    expect(post?.changes.every((c) => c.archived)).toBe(true);
    expect(items.has('fd-archived')).toBe(false);
    expect(items.get('fd-device-id')).toBeTruthy();
    expect(a.has('st/a')).toBe(true);
    // A later load posts nothing more.
    detach();
    const again = a.attach(d.api);
    await flush();
    expect(d.state.posts).toHaveLength(1);
    again();
  });

  test('a failed migration keeps the old key and the next attach reuses the id', async () => {
    const warn = spyOn(console, 'warn').mockImplementation(() => {});
    items.set('fd-archived', JSON.stringify(['st/a', 'st/b']));
    const d = deck();
    d.state.failPost = true;
    const a = createArchive();
    const first = a.attach(d.api);
    await flush();
    first();
    expect(items.has('fd-archived')).toBe(true);
    expect(d.state.posts).toHaveLength(1);
    d.state.failPost = false;
    const detach = a.attach(d.api);
    await flush();
    expect(d.state.posts).toHaveLength(2);
    expect(d.state.posts[1]?.mutation_id).toBe(d.state.posts[0]?.mutation_id);
    expect(items.has('fd-archived')).toBe(false);
    expect(a.has('st/a')).toBe(true);
    detach();
    warn.mockRestore();
  });

  test('a migration that stops between batches keeps each batch id on retry', async () => {
    const warn = spyOn(console, 'warn').mockImplementation(() => {});
    const old = Array.from({ length: 501 }, (_, i) => `st/s${String(i).padStart(3, '0')}`);
    items.set('fd-archived', JSON.stringify(old));
    const d = deck();
    // The real deck ignores a known mutation id.
    const applied = new Set<string>();
    let failAfter = 1;
    const api: ArchiveApi = {
      get: d.api.get,
      post: async (path, body) => {
        const b = body as Posted;
        if (failAfter === 0) throw new Error('down');
        failAfter -= 1;
        if (applied.has(b.mutation_id)) return d.api.get(path);
        applied.add(b.mutation_id);
        return d.api.post(path, body);
      },
    };
    const a = createArchive();
    const first = a.attach(api);
    await flush();
    first();
    expect(d.state.refs.size).toBe(500);
    expect(items.has('fd-archived')).toBe(true);
    failAfter = 10;
    const detach = a.attach(api);
    await flush();
    expect(d.state.refs.size).toBe(501);
    expect(items.has('fd-archived')).toBe(false);
    const ids = d.state.posts.map((p) => p.mutation_id);
    expect(new Set(ids).size).toBe(2);
    detach();
    warn.mockRestore();
  });

  test('a deck that is down leaves the cached set', async () => {
    const warn = spyOn(console, 'warn').mockImplementation(() => {});
    items.set('fd-archived-cache', JSON.stringify(['st/cached']));
    const d = deck();
    d.state.failGet = true;
    const a = createArchive();
    const detach = a.attach(d.api);
    await flush();
    expect(a.has('st/cached')).toBe(true);
    detach();
    warn.mockRestore();
  });
});
