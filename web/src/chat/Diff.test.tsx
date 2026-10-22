import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ReviewComment, ReviewThread } from '../gen';
import { spawnSnapshot } from '../review/placement';
import { Diff, type DiffReview, splitDiffFileLine } from './Diff';

const patch = [
  'diff --git a/src/a.ts b/src/a.ts',
  'index 83db48f..bf2a1c3 100644',
  '--- a/src/a.ts',
  '+++ b/src/a.ts',
  '@@ -1 +1 @@',
  '-old',
  '+new',
].join('\n');

const SNAPSHOT = spawnSnapshot('base1');

function comment(patch: Partial<ReviewComment> = {}): ReviewComment {
  return {
    comment_id: 'c1',
    short_id: 'c-abc234',
    thread_id: 't-1',
    snapshot: SNAPSHOT,
    path: 'src/a.ts',
    side: 'new',
    line_start: 1,
    line_end: 1,
    anchor_text: 'new',
    anchor_kind: 'diff',
    body: 'why?',
    author_kind: 'owner',
    state: 'sent',
    resolved: false,
    created_at: 1,
    updated_at: 1_760_000_000_000,
    ...patch,
  };
}

const thread = (c: ReviewComment): ReviewThread => ({ thread_id: c.thread_id, comments: [c] });

function review(patch: Partial<DiffReview> = {}): DiffReview {
  const none = async () => {};
  return {
    snapshot: SNAPSHOT,
    threads: [],
    canWrite: true,
    self: 'owner',
    focus: null,
    onAdd: none,
    onReply: none,
    onEdit: none,
    onResolve: none,
    onDelete: none,
    ...patch,
  };
}

describe('Diff', () => {
  test('a file is a header row: the path and its +/−, without the git plumbing', () => {
    const html = renderToStaticMarkup(<Diff patch={patch} truncated={false} id="t1" />);
    expect(html).toContain(
      '<span class="fd-dl-path"><span class="fd-dl-dir">src/</span><span class="fd-dl-base">a.ts</span></span>',
    );
    expect(html).toContain('<span data-sign="add">+1</span><span data-sign="del">−1</span>');
    expect(html).not.toContain('fd-dl-file');
    for (const plumbing of ['diff --git', 'index 83db48f', '--- a/src/a.ts', '+++ b/src/a.ts']) {
      expect(html).not.toContain(plumbing);
    }
    expect(html).toContain('@@ -1 +1 @@');
  });

  test('in a hunk, a line that looks like a file header is a content line', () => {
    // A deleted `-- note` (SQL, Lua) is `--- note` in the patch; an added `++ x` is `+++ x`.
    const sql = [
      'diff --git a/q.sql b/q.sql',
      'index 1..2 100644',
      '--- a/q.sql',
      '+++ b/q.sql',
      '@@ -1,2 +1,2 @@',
      '--- note',
      '+++ x',
      ' select 1;',
    ].join('\n');
    const html = renderToStaticMarkup(
      <Diff patch={sql} truncated={false} id="t1" review={review()} />,
    );
    expect(html).not.toContain('--- a/q.sql');
    expect(html).not.toContain('+++ b/q.sql');
    expect(html).toContain('<span data-sign="add">+1</span><span data-sign="del">−1</span>');
    expect(html).toMatch(/<span class="fd-dl" data-kind="del">.*--- note<\/span>/);
    expect(html).toMatch(/<span class="fd-dl" data-kind="add">.*\+\+\+ x<\/span>/);
    expect(html).toContain('aria-label="Comment on q.sql line 1 (old side)"');
    expect(html).toContain('aria-label="Comment on q.sql line 1"');
  });

  test('a file header has the path as a button with onOpenFile', () => {
    const html = renderToStaticMarkup(
      <Diff patch={patch} truncated={false} id="t1" onOpenFile={() => {}} />,
    );
    expect(html).toContain(
      '<button type="button" class="fd-dl-file" aria-label="src/a.ts"><span class="fd-dl-dir">src/</span><span class="fd-dl-base">a.ts</span></button>',
    );
    // only the file header has a button
    expect(html.match(/fd-dl-file/g)).toHaveLength(1);
  });

  test('with review, the old and the new number have a gutter each', () => {
    const ctx = [
      'diff --git a/x b/x',
      '--- a/x',
      '+++ b/x',
      '@@ -4,2 +4,2 @@',
      ' same',
      '-a',
      '+b',
    ].join('\n');
    const html = renderToStaticMarkup(
      <Diff patch={ctx} truncated={false} id="t1" review={review({ canWrite: false })} />,
    );
    const gutters = (old: string, now: string) =>
      `<span class="fd-dl-g" data-side="old">${old}</span><span class="fd-dl-g" data-side="new">${now}</span>`;
    // context: both numbers (the new one is the comment target)
    expect(html).toContain(gutters('4', '<span class="fd-dl-n">4</span>'));
    // deleted: the new gutter is blank; added: the old gutter is blank
    expect(html).toContain(gutters('<span class="fd-dl-n">5</span>', ''));
    expect(html).toContain(gutters('', '<span class="fd-dl-n">5</span>'));
  });

  test('without review the line numbers are not shown', () => {
    const plain = renderToStaticMarkup(<Diff patch={patch} truncated={false} id="t1" />);
    expect(plain).not.toContain('fd-dl-n');
    expect(plain).not.toContain('fd-diffwrap');
  });

  test('with review, a line that can take a comment has its number as a button', () => {
    const html = renderToStaticMarkup(
      <Diff patch={patch} truncated={false} id="t1" review={review()} />,
    );
    // `-old` is line 1 on the old side, `+new` line 1 on the new side.
    expect(html.match(/class="fd-dl-n"/g)).toHaveLength(2);
    expect(html).toContain('aria-label="Comment on src/a.ts line 1"');
    expect(html).toContain('aria-label="Comment on src/a.ts line 1 (old side)"');
  });

  test('a context line is commented on the new side only', () => {
    const ctx = [
      'diff --git a/x b/x',
      '--- a/x',
      '+++ b/x',
      '@@ -1,2 +1,2 @@',
      ' same',
      '-a',
      '+b',
    ].join('\n');
    const html = renderToStaticMarkup(
      <Diff patch={ctx} truncated={false} id="t1" review={review()} />,
    );
    expect(html.match(/aria-label="Comment on x line 1"/g)).toHaveLength(1);
    expect(html).not.toContain('aria-label="Comment on x line 1 (old side)"');
    expect(html).toContain('aria-label="Comment on x line 2 (old side)"');
    expect(html).toContain('aria-label="Comment on x line 2"');
  });

  test('without canWrite the numbers are plain spans', () => {
    const html = renderToStaticMarkup(
      <Diff patch={patch} truncated={false} id="t1" review={review({ canWrite: false })} />,
    );
    expect(html).toContain('<span class="fd-dl-n">1</span>');
    expect(html).not.toContain('<button type="button" class="fd-dl-n"');
  });

  test('a thread shows under the row of its last line', () => {
    const html = renderToStaticMarkup(
      <Diff
        patch={patch}
        truncated={false}
        id="t1"
        review={review({ threads: [thread(comment({ body: 'use a **map**' }))] })}
      />,
    );
    expect(html).toContain('data-thread="t-1"');
    // The thread follows the `+new` row.
    expect(html.indexOf('+new')).toBeLessThan(html.indexOf('data-thread="t-1"'));
    expect(html).toContain('<strong>map</strong>');
    expect(html).toContain('c-abc234');
    expect(html).toContain('>You<');
  });

  test('the owner shows as "Owner" to another reader', () => {
    const html = renderToStaticMarkup(
      <Diff
        patch={patch}
        truncated={false}
        id="t1"
        review={review({ self: 'ana', threads: [thread(comment())] })}
      />,
    );
    expect(html).toContain('>Owner<');
    expect(html).not.toContain('>You<');
  });

  test('own draft: "You", the Draft badge, Edit and Delete', () => {
    const html = renderToStaticMarkup(
      <Diff
        patch={patch}
        truncated={false}
        id="t1"
        review={review({ threads: [thread(comment({ state: 'draft' }))] })}
      />,
    );
    expect(html).toContain('>You<');
    expect(html).toContain('>Draft<');
    expect(html).toContain('>Edit</button>');
    expect(html).toContain('>Delete</button>');
    expect(html).toContain('>Reply</button>');
    // A draft of the caller can be resolved.
    expect(html).toContain('>Resolve</button>');
  });

  test("another author's comment has no Edit or Delete", () => {
    const other = comment({ author_kind: 'member', author_name: 'ana', state: 'sent' });
    const html = renderToStaticMarkup(
      <Diff
        patch={patch}
        truncated={false}
        id="t1"
        review={review({ threads: [thread(other)] })}
      />,
    );
    expect(html).toContain('>ana<');
    expect(html).not.toContain('>Edit</button>');
    expect(html).not.toContain('>Delete</button>');
    expect(html).not.toContain('>Draft<');
    expect(html).toContain('>Reply</button>');
  });

  test('a member sees its own comment as "You" and the owner as "Owner"', () => {
    const mine = comment({ author_kind: 'member', author_name: 'ana', state: 'draft' });
    const reply = comment({ comment_id: 'c2', short_id: 'c-def567', body: 'ok' });
    const html = renderToStaticMarkup(
      <Diff
        patch={patch}
        truncated={false}
        id="t1"
        review={review({ self: 'ana', threads: [{ thread_id: 't-1', comments: [mine, reply] }] })}
      />,
    );
    expect(html).toContain('>You<');
    expect(html).toContain('>Owner<');
  });

  test('a draft of another author is not resolvable, a sent thread is', () => {
    const draft = comment({ author_kind: 'member', author_name: 'ana', state: 'draft' });
    expect(
      renderToStaticMarkup(
        <Diff
          patch={patch}
          truncated={false}
          id="t1"
          review={review({ threads: [thread(draft)] })}
        />,
      ),
    ).not.toContain('Resolve');
    expect(
      renderToStaticMarkup(
        <Diff
          patch={patch}
          truncated={false}
          id="t1"
          review={review({ threads: [thread(comment({ state: 'sent' }))] })}
        />,
      ),
    ).toContain('>Resolve</button>');
  });

  test('read-only: no Reply, Resolve, Edit or Delete button', () => {
    const html = renderToStaticMarkup(
      <Diff
        patch={patch}
        truncated={false}
        id="t1"
        review={review({ canWrite: false, threads: [thread(comment({ state: 'sent' }))] })}
      />,
    );
    expect(html).toContain('data-thread="t-1"');
    for (const name of ['Reply', 'Resolve', 'Edit', 'Delete']) {
      expect(html).not.toContain(`>${name}</button>`);
    }
    expect(html).not.toContain('<button type="button" class="fd-dl-n"');
  });

  test('a resolved thread is collapsed to one line with Show', () => {
    const top = comment({ resolved: true });
    const reply = comment({ comment_id: 'c2', short_id: 'c-def567', body: 'second' });
    const html = renderToStaticMarkup(
      <Diff
        patch={patch}
        truncated={false}
        id="t1"
        review={review({ threads: [{ thread_id: 't-1', comments: [top, reply] }] })}
      />,
    );
    expect(html).toContain('Resolved thread c-abc234 · 2 comments');
    expect(html).toContain('aria-expanded="false"');
    expect(html).toContain('>Show</button>');
    expect(html).not.toContain('second');
  });

  test('a focused thread is marked', () => {
    const html = renderToStaticMarkup(
      <Diff
        patch={patch}
        truncated={false}
        id="t1"
        review={review({ focus: 't-1', threads: [thread(comment())] })}
      />,
    );
    expect(html).toContain('data-focus="true"');
  });

  test('an outdated thread is listed by file with its anchor text', () => {
    const moved = comment({ anchor_text: 'something else', line_start: 1, line_end: 1 });
    const html = renderToStaticMarkup(
      <Diff
        patch={patch}
        truncated={false}
        id="t1"
        review={review({ threads: [thread(moved)] })}
      />,
    );
    expect(html).toContain('aria-label="Outdated"');
    expect(html).toContain('<pre class="fd-thread-anchor">something else</pre>');
    // It is listed above the diff.
    expect(html.indexOf('aria-label="Outdated"')).toBeLessThan(
      html.indexOf('<pre class="fd-diff"'),
    );
  });

  test('a comment on a file that left the diff is listed as not in this diff', () => {
    const gone = comment({ path: 'src/zzz.ts' });
    const html = renderToStaticMarkup(
      <Diff patch={patch} truncated={false} id="t1" review={review({ threads: [thread(gone)] })} />,
    );
    expect(html).toContain('aria-label="Comments not in this diff"');
    expect(html).not.toContain('aria-label="Outdated"');
  });

  test('a comment past the cut is listed as not shown', () => {
    const late = comment({ path: 'src/zzz.ts' });
    const html = renderToStaticMarkup(
      <Diff patch={patch} truncated id="t1" review={review({ threads: [thread(late)] })} />,
    );
    expect(html).toContain('aria-label="Not shown: the diff is cut"');
  });

  test('an imported note says so and has no anchor text', () => {
    const note = comment({ anchor_kind: 'imported', anchor_text: '' });
    const html = renderToStaticMarkup(
      <Diff patch={patch} truncated={false} id="t1" review={review({ threads: [thread(note)] })} />,
    );
    expect(html).toContain('Imported note');
    expect(html).not.toContain('fd-thread-anchor');
  });

  test('a comment of another turn is not shown in this view', () => {
    const other = comment({ snapshot: 'turn:c1..c2' });
    const html = renderToStaticMarkup(
      <Diff
        patch={patch}
        truncated={false}
        id="t1"
        review={review({ threads: [thread(other)] })}
      />,
    );
    expect(html).not.toContain('data-thread');
  });

  test('hostile Markdown in a comment body renders as text', () => {
    const evil = comment({ body: '<script>alert(1)</script><img src=x onerror=alert(1)>' });
    const html = renderToStaticMarkup(
      <Diff patch={patch} truncated={false} id="t1" review={review({ threads: [thread(evil)] })} />,
    );
    expect(html).not.toContain('<script');
    expect(html).not.toContain('<img');
  });

  test('a new, a deleted and a mode-changed file show a status badge in the header', () => {
    const html = renderToStaticMarkup(
      <Diff
        patch={[
          'diff --git a/empty.ts b/empty.ts',
          'new file mode 100644',
          'index 0000000..e69de29',
          'diff --git a/gone.ts b/gone.ts',
          'deleted file mode 100644',
          'diff --git a/run.sh b/run.sh',
          'old mode 100644',
          'new mode 100755',
        ].join('\n')}
        truncated={false}
        id="t1"
      />,
    );
    expect(html).toContain(
      '<span class="fd-badge fd-dl-status" data-tone="ok" title="New file">new</span>',
    );
    expect(html).toContain(
      '<span class="fd-badge fd-dl-status" data-tone="hazard" title="Deleted file">deleted</span>',
    );
    expect(html).toContain('title="The file mode changed">mode 100644 → 100755</span>');
    for (const plumbing of ['new file mode', 'deleted file mode', 'old mode', 'new mode']) {
      expect(html).not.toContain(plumbing);
    }
  });

  test('a file that only changes its lines has no status badge', () => {
    const html = renderToStaticMarkup(<Diff patch={patch} truncated={false} id="t1" />);
    expect(html).not.toContain('fd-dl-status');
  });

  test('splitDiffFileLine takes the path after b/', () => {
    expect(splitDiffFileLine('diff --git a/x/old.ts b/x/new.ts')).toEqual({
      head: 'diff --git a/x/old.ts b/',
      path: 'x/new.ts',
    });
    expect(splitDiffFileLine('+diff --git a/x b/x')).toBeNull();
    expect(splitDiffFileLine('index 1..2')).toBeNull();
  });
});
