import { describe, expect, test } from 'bun:test';
import { hasChanges } from './changes';

describe('hasChanges', () => {
  test('a null summary has no changes', () => {
    expect(hasChanges(null)).toBe(false);
  });

  test('a summary with 0 files has no changes', () => {
    expect(hasChanges({ files_changed: 0, insertions: 0, deletions: 0 })).toBe(false);
  });

  test('a summary with 1 or more files has changes, also with 0 lines', () => {
    expect(hasChanges({ files_changed: 1, insertions: 2, deletions: 1 })).toBe(true);
    expect(hasChanges({ files_changed: 2, insertions: 0, deletions: 0 })).toBe(true);
  });
});
