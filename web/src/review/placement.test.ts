import { describe, expect, test } from 'bun:test';
import type { ReviewComment } from '../gen';
import { CUT_MARKER, parseDiff } from './diffModel';
import { place, spawnSnapshot, turnSnapshot } from './placement';

const PATCH = [
  'diff --git a/src/a.ts b/src/a.ts',
  '--- a/src/a.ts',
  '+++ b/src/a.ts',
  '@@ -1,3 +1,3 @@',
  ' one',
  '-two',
  '+TWO',
  ' three',
  'diff --git a/src/b.ts b/src/b.ts',
  '--- a/src/b.ts',
  '+++ b/src/b.ts',
  '@@ -1 +1 @@',
  '-x',
  '+y',
  '',
].join('\n');

function comment(patch: Partial<ReviewComment> = {}): ReviewComment {
  return {
    comment_id: '01J0000000000000000000000A',
    short_id: 'c-abc234',
    thread_id: '01J0000000000000000000000A',
    snapshot: spawnSnapshot('base1'),
    path: 'src/a.ts',
    side: 'new',
    line_start: 2,
    line_end: 2,
    anchor_text: 'TWO',
    anchor_kind: 'diff',
    body: 'why?',
    author_kind: 'owner',
    state: 'sent',
    resolved: false,
    created_at: 1,
    updated_at: 1,
    ...patch,
  };
}

const spawnView = (patch = PATCH, truncated = false) => ({
  snapshot: spawnSnapshot('base1'),
  parsed: parseDiff(patch),
  truncated,
});

describe('place', () => {
  test('a spawn comment whose lines read the same is on its line', () => {
    expect(place(comment(), spawnView())).toEqual({
      kind: 'line',
      side: 'new',
      path: 'src/a.ts',
      line: 2,
    });
  });

  test('the line is the last line of a range', () => {
    const p = place(comment({ line_start: 1, line_end: 2, anchor_text: 'one\nTWO' }), spawnView());
    expect(p).toEqual({ kind: 'line', side: 'new', path: 'src/a.ts', line: 2 });
  });

  test('a spawn comment whose lines changed is outdated', () => {
    const changed = PATCH.replace('+TWO', '+TWO!');
    expect(place(comment(), spawnView(changed))).toEqual({ kind: 'outdated', path: 'src/a.ts' });
  });

  test('a spawn comment whose file left the diff is gone', () => {
    const only = PATCH.split('diff --git a/src/b.ts')[0] ?? '';
    const c = comment({ path: 'src/b.ts', anchor_text: 'y' });
    expect(place(c, spawnView(only))).toEqual({ kind: 'gone' });
  });

  test('a spawn comment with another base oid is outdated or gone', () => {
    const other = { ...spawnView(), snapshot: spawnSnapshot('base2') };
    expect(place(comment(), other)).toEqual({ kind: 'outdated', path: 'src/a.ts' });
    expect(place(comment({ path: 'src/zzz.ts' }), other)).toEqual({ kind: 'gone' });
  });

  test('a cut diff lists a comment past the cut as cut, not outdated', () => {
    const cut = `${PATCH.split('diff --git a/src/b.ts')[0] ?? ''}${CUT_MARKER}`;
    const c = comment({ path: 'src/b.ts', anchor_text: 'y' });
    expect(place(c, spawnView(cut, true))).toEqual({ kind: 'cut' });
    // The anchor is not found in a file that the diff still lists: cut too.
    const late = comment({ line_start: 3, line_end: 3, anchor_text: 'three' });
    const early = `${(PATCH.split(' three')[0] ?? '').trimEnd()}\n${CUT_MARKER}`;
    expect(place(late, spawnView(early, true))).toEqual({ kind: 'cut' });
    // The same missing file in a diff that is not cut is gone.
    expect(place(c, spawnView(cut, false))).toEqual({ kind: 'gone' });
  });

  test('a cut diff keeps an anchor that it still shows but changed as outdated', () => {
    const changed = PATCH.replace('+TWO', '+TWO!');
    expect(place(comment(), spawnView(changed, true))).toEqual({
      kind: 'outdated',
      path: 'src/a.ts',
    });
  });

  test('a turn comment shows only in its own turn view', () => {
    const snap = turnSnapshot('c1', 'c2');
    const c = comment({ snapshot: snap });
    const own = { snapshot: snap, parsed: parseDiff(PATCH), truncated: false };
    expect(place(c, own)).toEqual({ kind: 'line', side: 'new', path: 'src/a.ts', line: 2 });
    // The lines are the identity of a turn: the stored text does not matter.
    expect(place(comment({ snapshot: snap, anchor_text: 'old text' }), own)).toEqual({
      kind: 'line',
      side: 'new',
      path: 'src/a.ts',
      line: 2,
    });
    const another = { ...own, snapshot: turnSnapshot('c2', 'c3') };
    expect(place(c, another)).toBeNull();
    expect(place(c, spawnView())).toBeNull();
  });

  test('a turn comment whose lines are not in the turn diff follows the spawn rules', () => {
    const snap = turnSnapshot('c1', 'c2');
    const view = { snapshot: snap, parsed: parseDiff(PATCH), truncated: false };
    expect(place(comment({ snapshot: snap, line_start: 40, line_end: 40 }), view)).toEqual({
      kind: 'outdated',
      path: 'src/a.ts',
    });
    expect(place(comment({ snapshot: snap, path: 'nope.ts' }), view)).toEqual({ kind: 'gone' });
  });

  test('a spawn comment is not shown in a turn view', () => {
    const view = {
      snapshot: turnSnapshot('c1', 'c2'),
      parsed: parseDiff(PATCH),
      truncated: false,
    };
    expect(place(comment(), view)).toBeNull();
  });

  test('an imported comment is never on a line', () => {
    const c = comment({ anchor_kind: 'imported', anchor_text: '' });
    expect(place(c, spawnView())).toEqual({ kind: 'outdated', path: 'src/a.ts' });
    expect(place({ ...c, path: 'src/zzz.ts' }, spawnView())).toEqual({ kind: 'gone' });
    // A cut diff does not make an imported note "cut".
    const cut = `${PATCH.split('diff --git a/src/b.ts')[0] ?? ''}${CUT_MARKER}`;
    expect(place({ ...c, path: 'src/b.ts' }, spawnView(cut, true))).toEqual({ kind: 'gone' });
  });
});
