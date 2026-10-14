import { describe, expect, test } from 'bun:test';
import type { ReviewComment } from '../gen';
import {
  commentLabel,
  commentOptions,
  hashMenu,
  hashRows,
  matchHash,
  pickComment,
} from './hashMenu';

describe('matchHash', () => {
  test('a # word with at least one character matches', () => {
    expect(matchHash('#ab', 3)).toEqual({ query: 'ab', start: 0 });
    expect(matchHash('see #c-ab', 9)).toEqual({ query: 'c-ab', start: 4 });
  });
  test('a bare # does not match', () => {
    expect(matchHash('#', 1)).toBeNull();
    expect(matchHash('see #', 5)).toBeNull();
  });
  test('a # inside a word does not match', () => {
    expect(matchHash('a#b', 3)).toBeNull();
    expect(matchHash('ab', 2)).toBeNull();
  });
  test('the word ends at the cursor', () => {
    expect(matchHash('#abc def', 3)).toEqual({ query: 'ab', start: 0 });
    expect(matchHash('#abc def', 8)).toBeNull();
  });
});

function comment(short: string, patch: Partial<ReviewComment> = {}): ReviewComment {
  return {
    comment_id: short,
    short_id: short,
    thread_id: short,
    snapshot: 'spawn:b',
    path: 'src/a.ts',
    side: 'new',
    line_start: 4,
    line_end: 4,
    anchor_text: '',
    anchor_kind: 'diff',
    body: 'rename this\nsecond line',
    author_kind: 'owner',
    state: 'sent',
    resolved: false,
    created_at: 1,
    updated_at: 1,
    ...patch,
  };
}

describe('the comment rows', () => {
  const options = commentOptions([
    comment('c-abc234', { created_at: 1 }),
    comment('c-xyz789', { created_at: 3, path: 'b.ts', body: 'use a map' }),
    comment('c-mno345', { created_at: 2, body: 'x'.repeat(100) }),
  ]);
  test('the options are newest first with the label path:line and the first line', () => {
    expect(options.map((o) => o.shortId)).toEqual(['c-xyz789', 'c-mno345', 'c-abc234']);
    expect(options[0]?.label).toBe('b.ts:4 use a map');
    expect(commentLabel(comment('c-abc234'))).toBe('src/a.ts:4 rename this');
  });
  test('the label cuts the first line to 60 characters with an ellipsis', () => {
    const label = options[1]?.label ?? '';
    expect(label).toBe(`src/a.ts:4 ${'x'.repeat(59)}…`);
  });
  test('a row matches the short id or the label, in any case', () => {
    expect(hashRows(options, 'ab').map((r) => r.key)).toEqual(['c-abc234']);
    expect(hashRows(options, 'MAP').map((r) => r.key)).toEqual(['c-xyz789']);
    expect(hashRows(options, 'src/a')).toHaveLength(2);
    expect(hashRows(options, 'zzzz')).toEqual([]);
  });
  test('a row has the short id as text and the label as badge', () => {
    expect(hashRows(options, 'xyz')).toEqual([
      { key: 'c-xyz789', text: 'c-xyz789', badge: 'b.ts:4 use a map' },
    ]);
  });
  test('there are at most 20 rows', () => {
    const many = Array.from({ length: 30 }, (_, i) => ({ shortId: `c-a${i}`, label: 'x' }));
    expect(hashRows(many, 'c-a')).toHaveLength(20);
  });
});

describe('hashMenu', () => {
  const options = [{ shortId: 'c-abc234', label: 'a.ts:1 hi' }];
  test('it opens for #ab', () => {
    const menu = hashMenu('#ab', 3, options, false);
    expect(menu?.rows.map((r) => r.key)).toEqual(['c-abc234']);
    expect(menu?.match).toEqual({ query: 'ab', start: 0 });
  });
  test('the / menu has precedence', () => {
    expect(hashMenu('#ab', 3, options, true)).toBeNull();
  });
  test('no match gives no menu and no status row', () => {
    expect(hashMenu('#zz', 3, options, false)).toBeNull();
    expect(hashMenu('#ab', 3, [], false)).toBeNull();
  });
});

describe('pickComment', () => {
  test('it replaces #<query> with [c-xxxxxx] and puts the caret after it', () => {
    expect(pickComment('see #ab now', 7, { start: 4 }, 'c-abc234')).toEqual({
      text: 'see [c-abc234]  now',
      caret: 15,
    });
    expect(pickComment('#ab', 3, { start: 0 }, 'c-abc234')).toEqual({
      text: '[c-abc234] ',
      caret: 11,
    });
  });
});
