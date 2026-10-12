import { describe, expect, test } from 'bun:test';
import { diffLineKind } from './diffLines';

describe('diffLineKind', () => {
  test('a diff --git line starts a file', () => {
    expect(diffLineKind('diff --git a/src/a.ts b/src/a.ts')).toBe('file');
  });

  test('the header lines of a file are meta', () => {
    for (const line of [
      '--- a/src/a.ts',
      '+++ b/src/a.ts',
      'index 83db48f..bf2a1c3 100644',
      'new file mode 100644',
      'deleted file mode 100644',
      'similarity index 90%',
      'rename from old.ts',
      'rename to new.ts',
      'Binary files a/x.png and b/x.png differ',
      'old mode 100644',
      'new mode 100755',
    ])
      expect(diffLineKind(line)).toBe('meta');
  });

  test('a hunk header is a hunk', () => {
    expect(diffLineKind('@@ -1,3 +1,4 @@ function f() {')).toBe('hunk');
  });

  test('plus and minus lines are add and del', () => {
    expect(diffLineKind('+added')).toBe('add');
    expect(diffLineKind('-removed')).toBe('del');
    // a content line that starts with two dashes is still a deletion
    expect(diffLineKind('--x')).toBe('del');
  });

  test('a context line and an empty line are ctx', () => {
    expect(diffLineKind(' unchanged')).toBe('ctx');
    expect(diffLineKind('')).toBe('ctx');
  });
});
