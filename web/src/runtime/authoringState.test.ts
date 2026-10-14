import { afterEach, describe, expect, test } from 'bun:test';
import { clearAuthoringState } from './authoringState';

function memory(initial: Record<string, string> = {}): Storage {
  const data = new Map(Object.entries(initial));
  return {
    get length() {
      return data.size;
    },
    clear: () => data.clear(),
    getItem: (k) => data.get(k) ?? null,
    key: (i) => [...data.keys()][i] ?? null,
    removeItem: (k) => {
      data.delete(k);
    },
    setItem: (k, v) => {
      data.set(k, v);
    },
  };
}

type Name = 'sessionStorage' | 'localStorage';
const original = {
  sessionStorage: Object.getOwnPropertyDescriptor(globalThis, 'sessionStorage'),
  localStorage: Object.getOwnPropertyDescriptor(globalThis, 'localStorage'),
};
function use(name: Name, value: Storage | undefined) {
  Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
}
afterEach(() => {
  for (const name of ['sessionStorage', 'localStorage'] as const) {
    const saved = original[name];
    if (saved) Object.defineProperty(globalThis, name, saved);
    else Reflect.deleteProperty(globalThis, name);
  }
});

describe('authoring state (NV-05)', () => {
  test('clear removes every draft and every legacy note and keeps other keys', () => {
    const session = memory({ 'fd-draft:a/b': 'x', 'fd-draft:c/d': 'y', other: '1' });
    const local = memory({ 'fd-notes:a/b': '[]', 'fd-draft:keep': 'z', other: '2' });
    use('sessionStorage', session);
    use('localStorage', local);
    clearAuthoringState();
    expect(session.length).toBe(1);
    expect(session.getItem('other')).toBe('1');
    expect(local.length).toBe(2);
    expect(local.getItem('other')).toBe('2');
    expect(local.getItem('fd-draft:keep')).toBe('z');
  });

  test('clear does not throw when storage is missing or throws', () => {
    const broken = {
      get length(): number {
        throw new Error('blocked');
      },
    } as unknown as Storage;
    use('sessionStorage', broken);
    use('localStorage', undefined);
    expect(() => clearAuthoringState()).not.toThrow();
  });
});
