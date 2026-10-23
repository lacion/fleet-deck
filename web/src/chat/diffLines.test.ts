import { describe, expect, test } from 'bun:test';
import { diffLineKind, diffLineKinds, fileHeads, oneSidedLines } from './diffLines';

describe('oneSidedLines', () => {
  const lines = [
    'diff --git a/new.ts b/new.ts',
    'new file mode 100644',
    'index 0000000..e69de29',
    '--- /dev/null',
    '+++ b/new.ts',
    '@@ -0,0 +1,2 @@',
    '+one',
    '+two',
    'diff --git a/old.ts b/old.ts',
    'deleted file mode 100644',
    '@@ -1 +0,0 @@',
    '-gone',
    'diff --git a/a.ts b/a.ts',
    '@@ -1 +1 @@',
    '-a',
    '+b',
    'diff --git a/two.ts b/two.ts',
    'new file mode 100644',
    '@@ -0,0 +1 @@',
    '+x',
    '@@ -0,0 +5 @@',
    '+y',
  ];
  const sides = oneSidedLines(lines, diffLineKinds(lines), fileHeads(lines));

  test('the lines of a whole new file have the new side, of a deleted file the old side', () => {
    for (const n of [1, 5, 6, 7]) expect(sides.get(n)).toBe('new');
    for (const n of [9, 10, 11]) expect(sides.get(n)).toBe('old');
    // The file header row is not a line of the section.
    expect(sides.has(0)).toBe(false);
    expect(sides.has(8)).toBe(false);
  });

  test('a changed file, or a new file of two hunks, keeps both sides', () => {
    for (const n of [13, 14, 15]) expect(sides.has(n)).toBe(false);
    for (const n of [17, 18, 19, 20, 21]) expect(sides.has(n)).toBe(false);
  });
});

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

describe('diffLineKinds', () => {
  const lines = [
    'diff --git a/q.sql b/q.sql',
    '--- a/q.sql',
    '+++ b/q.sql',
    '@@ -1,2 +1,2 @@',
    '--- note',
    '+++ x',
    ' select 1;',
    'diff --git a/r.sql b/r.sql',
    '--- a/r.sql',
    '+++ b/r.sql',
  ];

  test('a -/+ line in a hunk is del/add; the header lines of the next file are meta again', () => {
    expect(diffLineKinds(lines)).toEqual([
      'file',
      'meta',
      'meta',
      'hunk',
      'del',
      'add',
      'ctx',
      'file',
      'meta',
      'meta',
    ]);
  });

  test('the file header counts the -/+ lines of a hunk that look like header lines', () => {
    expect(fileHeads(lines).get(0)).toEqual({ path: 'q.sql', add: 1, del: 1, status: null });
    expect(fileHeads(lines).get(7)).toEqual({ path: 'r.sql', add: 0, del: 0, status: null });
  });
});

describe('fileHeads status', () => {
  const lines = [
    'diff --git a/new.ts b/new.ts',
    'new file mode 100644',
    'index 0000000..e69de29',
    'diff --git a/old.ts b/old.ts',
    'deleted file mode 100644',
    'index e69de29..0000000',
    '--- a/old.ts',
    '+++ /dev/null',
    '@@ -1 +0,0 @@',
    '-gone',
    'diff --git a/run.sh b/run.sh',
    'old mode 100644',
    'new mode 100755',
    'diff --git a/a.ts b/a.ts',
    'index 1..2 100644',
    '--- a/a.ts',
    '+++ b/a.ts',
    '@@ -1 +1 @@',
    '-new file mode 1',
    '+old mode 2',
  ];
  const heads = fileHeads(lines);

  test('a new, a deleted and a mode-changed file have their status', () => {
    expect(heads.get(0)).toEqual({ path: 'new.ts', add: 0, del: 0, status: { kind: 'added' } });
    expect(heads.get(3)).toEqual({ path: 'old.ts', add: 0, del: 1, status: { kind: 'deleted' } });
    expect(heads.get(10)).toEqual({
      path: 'run.sh',
      add: 0,
      del: 0,
      status: { kind: 'mode', old: '100644', new: '100755' },
    });
  });

  test('a hunk line that looks like a mode line does not set a status', () => {
    expect(heads.get(13)).toEqual({ path: 'a.ts', add: 1, del: 1, status: null });
  });
});
