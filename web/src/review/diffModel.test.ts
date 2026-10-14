import { describe, expect, test } from 'bun:test';
import type { ReviewSide } from '../gen';
import { anchorText, CUT_MARKER, diffPaths, diffRows, parseDiff } from './diffModel';

// The inputs and the expected results are those of
// crates/fleetdeck-deck/src/store/review_anchor_tests.rs.
const MODIFIED = `diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,4 +1,5 @@ fn main() {
 one
-two
+TWO
+two and a half
 three
 four
@@ -20,2 +21,2 @@
 twenty
-twenty-one
+TWENTY-ONE
\\ No newline at end of file
`;

const text = (patch: string, side: ReviewSide, path: string, a: number, b: number) =>
  anchorText(parseDiff(patch), side, path, a, b);

describe('anchors (ported from the deck tests)', () => {
  test('the new side counts context and added lines and the old side context and removed lines', () => {
    const d = parseDiff(MODIFIED);
    expect(anchorText(d, 'new', 'src/a.rs', 2, 3)).toBe('TWO\ntwo and a half');
    expect(anchorText(d, 'new', 'src/a.rs', 4, 4)).toBe('three');
    expect(anchorText(d, 'old', 'src/a.rs', 2, 2)).toBe('two');
    expect(anchorText(d, 'old', 'src/a.rs', 1, 4)).toBe('one\ntwo\nthree\nfour');
    expect(anchorText(d, 'new', 'src/a.rs', 21, 22)).toBe('twenty\nTWENTY-ONE');
    expect(anchorText(d, 'old', 'src/a.rs', 21, 21)).toBe('twenty-one');
    // The marker `\ No newline at end of file` is not a line.
    expect(anchorText(d, 'new', 'src/a.rs', 22, 23)).toBeNull();
    expect(anchorText(d, 'new', 'src/a.rs', 4, 5)).toBe('three\nfour');
  });

  test('a range over two hunks or over a line that no hunk shows is not in the diff', () => {
    const d = parseDiff(MODIFIED);
    const cases: [ReviewSide, number, number][] = [
      ['new', 5, 21],
      ['new', 6, 6],
      ['new', 1, 6],
      ['new', 100, 100],
      ['old', 4, 5],
      ['new', 0, 1],
      ['new', 3, 2],
    ];
    for (const [side, a, b] of cases) {
      expect(anchorText(d, side, 'src/a.rs', a, b)).toBeNull();
    }
    expect(anchorText(d, 'new', 'src/other.rs', 1, 1)).toBeNull();
  });

  test('a removed line that looks like a header stays a line of its hunk', () => {
    const patch = [
      'diff --git a/x b/x',
      '--- a/x',
      '+++ b/x',
      '@@ -1,3 +1 @@',
      ' keep',
      '--- not a header',
      '-+++ not a header',
      '',
    ].join('\n');
    expect(text(patch, 'old', 'x', 1, 3)).toBe('keep\n-- not a header\n+++ not a header');
    expect(text(patch, 'new', 'x', 1, 1)).toBe('keep');
  });

  test('a rename gives each side its own path', () => {
    const patch = [
      'diff --git a/old/name.rs b/new/name.rs',
      'similarity index 90%',
      'rename from old/name.rs',
      'rename to new/name.rs',
      '--- a/old/name.rs',
      '+++ b/new/name.rs',
      '@@ -1,2 +1,2 @@',
      ' same',
      '-before',
      '+after',
      '',
    ].join('\n');
    expect(text(patch, 'new', 'new/name.rs', 2, 2)).toBe('after');
    expect(text(patch, 'old', 'old/name.rs', 2, 2)).toBe('before');
    // The old path does not exist on the new side, nor the new one on the old.
    expect(text(patch, 'new', 'old/name.rs', 1, 1)).toBeNull();
    expect(text(patch, 'old', 'new/name.rs', 1, 1)).toBeNull();
    expect([...diffPaths(parseDiff(patch))].sort()).toEqual(['new/name.rs', 'old/name.rs']);
  });

  test('a copy uses the copy lines; a pure rename or a binary file has no lines', () => {
    const patch = [
      'diff --git a/src.rs b/copy.rs',
      'similarity index 100%',
      'copy from src.rs',
      'copy to copy.rs',
      'diff --git a/from.rs b/to.rs',
      'similarity index 100%',
      'rename from from.rs',
      'rename to to.rs',
      'diff --git a/pic.png b/pic.png',
      'index 1..2 100644',
      'Binary files a/pic.png and b/pic.png differ',
      '',
    ].join('\n');
    const cases: [ReviewSide, string][] = [
      ['new', 'copy.rs'],
      ['old', 'src.rs'],
      ['new', 'to.rs'],
      ['old', 'from.rs'],
      ['new', 'pic.png'],
      ['old', 'pic.png'],
    ];
    for (const [side, path] of cases) expect(text(patch, side, path, 1, 1)).toBeNull();
    // The copy and the rename paths are in the diff though.
    const paths = diffPaths(parseDiff(patch));
    for (const p of ['src.rs', 'copy.rs', 'from.rs', 'to.rs', 'pic.png']) {
      expect(paths.has(p)).toBe(true);
    }
  });

  test('a new file has no old side and a deleted file has no new side', () => {
    const patch = [
      'diff --git a/added.rs b/added.rs',
      'new file mode 100644',
      '--- /dev/null',
      '+++ b/added.rs',
      '@@ -0,0 +1,2 @@',
      '+first',
      '+second',
      'diff --git a/gone.rs b/gone.rs',
      'deleted file mode 100644',
      '--- a/gone.rs',
      '+++ /dev/null',
      '@@ -1,2 +0,0 @@',
      '-a',
      '-b',
      '',
    ].join('\n');
    expect(text(patch, 'new', 'added.rs', 1, 2)).toBe('first\nsecond');
    expect(text(patch, 'old', 'added.rs', 1, 1)).toBeNull();
    expect(text(patch, 'old', 'gone.rs', 1, 2)).toBe('a\nb');
    expect(text(patch, 'new', 'gone.rs', 1, 1)).toBeNull();
  });

  test('paths with spaces and quoted paths are read from the header', () => {
    const patch = [
      'diff --git a/my dir/my file.txt b/my dir/my file.txt',
      '--- a/my dir/my file.txt',
      '+++ b/my dir/my file.txt',
      '@@ -1 +1 @@',
      '-x',
      '+y',
      'diff --git "a/caf\\303\\251.txt" "b/caf\\303\\251.txt"',
      '--- "a/caf\\303\\251.txt"',
      '+++ "b/caf\\303\\251.txt"',
      '@@ -1 +1 @@',
      '-p',
      '+q',
      'diff --git a/a b/c.txt b/a b/c.txt',
      '--- a/a b/c.txt',
      '+++ b/a b/c.txt',
      '@@ -1 +1 @@',
      '-m',
      '+n',
      '',
    ].join('\n');
    expect(text(patch, 'new', 'my dir/my file.txt', 1, 1)).toBe('y');
    expect(text(patch, 'new', 'café.txt', 1, 1)).toBe('q');
    expect(text(patch, 'new', 'a b/c.txt', 1, 1)).toBe('n');
  });

  test('a cut patch loses its partial last line and nothing past the cut is in the diff', () => {
    // Cut in the middle of the line `four`: its text is not complete.
    const mid = `${MODIFIED.slice(0, MODIFIED.indexOf(' four') + 3)}${CUT_MARKER}`;
    expect(text(mid, 'new', 'src/a.rs', 4, 4)).toBe('three');
    expect(text(mid, 'new', 'src/a.rs', 5, 5)).toBeNull();
    expect(text(mid, 'new', 'src/a.rs', 21, 21)).toBeNull();
    // Cut at a line boundary: the last line is complete.
    const at = MODIFIED.indexOf(' four');
    const edge = `${MODIFIED.slice(0, at + 6)}${CUT_MARKER}`;
    expect(text(edge, 'new', 'src/a.rs', 5, 5)).toBe('four');
    expect(text(edge, 'new', 'src/a.rs', 21, 21)).toBeNull();
  });

  test('an untracked file patch of the station has lines and the no-newline marker', () => {
    const patch =
      'diff --git a/n.txt b/n.txt\nnew file mode 100644\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n\\ No newline at end of file\n';
    expect(text(patch, 'new', 'n.txt', 1, 2)).toBe('a\nb');
    expect(text(patch, 'new', 'n.txt', 3, 3)).toBeNull();
  });

  test('an anchor over 4 KiB is still found (the board does no size check)', () => {
    const line = 'x'.repeat(1000);
    let patch = 'diff --git a/big b/big\n--- a/big\n+++ b/big\n@@ -0,0 +1,6 @@\n';
    for (let i = 0; i < 6; i += 1) patch += `+${line}\n`;
    expect(text(patch, 'new', 'big', 1, 5)?.length).toBe(5004);
  });
});

describe('diffRows', () => {
  test('add, context and delete lines get a side, a path and a line', () => {
    const rows = diffRows(MODIFIED);
    const lines = MODIFIED.split('\n');
    expect(rows).toHaveLength(lines.length);
    const at = (needle: string) => rows[lines.indexOf(needle)];
    expect(at(' one')).toEqual({ side: 'new', path: 'src/a.rs', line: 1 });
    expect(at('-two')).toEqual({ side: 'old', path: 'src/a.rs', line: 2 });
    expect(at('+TWO')).toEqual({ side: 'new', path: 'src/a.rs', line: 2 });
    expect(at('+two and a half')).toEqual({ side: 'new', path: 'src/a.rs', line: 3 });
    expect(at(' three')).toEqual({ side: 'new', path: 'src/a.rs', line: 4 });
    expect(at('-twenty-one')).toEqual({ side: 'old', path: 'src/a.rs', line: 21 });
    expect(at('+TWENTY-ONE')).toEqual({ side: 'new', path: 'src/a.rs', line: 22 });
    // Headers, hunk lines and the no-newline marker have no row.
    expect(rows[0]).toBeNull();
    expect(at('@@ -20,2 +21,2 @@')).toBeNull();
    expect(at('\\ No newline at end of file')).toBeNull();
    expect(at('+++ b/src/a.rs')).toBeNull();
  });

  test('a rename has the old path on a delete line and the new path on an add line', () => {
    const patch = [
      'diff --git a/old/name.rs b/new/name.rs',
      'rename from old/name.rs',
      'rename to new/name.rs',
      '--- a/old/name.rs',
      '+++ b/new/name.rs',
      '@@ -1,2 +1,2 @@',
      ' same',
      '-before',
      '+after',
    ].join('\n');
    const rows = diffRows(patch);
    expect(rows[6]).toEqual({ side: 'new', path: 'new/name.rs', line: 1 });
    expect(rows[7]).toEqual({ side: 'old', path: 'old/name.rs', line: 2 });
    expect(rows[8]).toEqual({ side: 'new', path: 'new/name.rs', line: 2 });
  });

  test('a new file has no old rows and a deleted file has no new rows', () => {
    const patch = [
      'diff --git a/added.rs b/added.rs',
      'new file mode 100644',
      '--- /dev/null',
      '+++ b/added.rs',
      '@@ -0,0 +1,1 @@',
      '+first',
      'diff --git a/gone.rs b/gone.rs',
      'deleted file mode 100644',
      '--- a/gone.rs',
      '+++ /dev/null',
      '@@ -1,1 +0,0 @@',
      '-a',
    ].join('\n');
    const rows = diffRows(patch);
    expect(rows[5]).toEqual({ side: 'new', path: 'added.rs', line: 1 });
    expect(rows[11]).toEqual({ side: 'old', path: 'gone.rs', line: 1 });
  });

  test('a quoted path is decoded', () => {
    const patch = [
      'diff --git "a/caf\\303\\251.txt" "b/caf\\303\\251.txt"',
      '--- "a/caf\\303\\251.txt"',
      '+++ "b/caf\\303\\251.txt"',
      '@@ -1 +1 @@',
      '-p',
      '+q',
    ].join('\n');
    const rows = diffRows(patch);
    expect(rows[4]).toEqual({ side: 'old', path: 'café.txt', line: 1 });
    expect(rows[5]).toEqual({ side: 'new', path: 'café.txt', line: 1 });
  });

  test('a cut patch gives null for the cut marker and the partial line before it', () => {
    const mid = `${MODIFIED.slice(0, MODIFIED.indexOf(' four') + 3)}${CUT_MARKER}`;
    const rows = diffRows(mid);
    const lines = mid.split('\n');
    expect(rows).toHaveLength(lines.length);
    expect(rows[lines.indexOf(' three')]).toEqual({ side: 'new', path: 'src/a.rs', line: 4 });
    expect(rows[lines.indexOf(' fo')]).toBeNull();
    expect(rows[lines.indexOf('[diff truncated]')]).toBeNull();
  });
});
