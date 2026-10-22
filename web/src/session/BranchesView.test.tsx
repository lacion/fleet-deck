import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { RepoBranch, RepoBranches, SessionDiff } from '../gen';
import { BranchDiffView, BranchListView, type DiffState, type ListState } from './BranchesView';

const now = Math.floor(Date.now() / 1000);
const row = (extra: Partial<RepoBranch> = {}): RepoBranch => ({
  name: 'feat/x',
  head: 'a'.repeat(40),
  ahead: 2,
  behind: 3,
  at: now - 3 * 3600,
  subject: 'Add the thing',
  ...extra,
});
const data = (extra: Partial<RepoBranches> = {}): RepoBranches => ({
  head: 'b'.repeat(40),
  branch: 'main',
  branches: [row()],
  truncated: false,
  ...extra,
});
const list = (state: ListState) =>
  renderToStaticMarkup(<BranchListView list={state} onOpen={() => {}} onReload={() => {}} />);
const ready = (extra: Partial<RepoBranches> = {}): ListState => ({
  kind: 'ready',
  data: data(extra),
});

describe('the branch list (DL-191)', () => {
  test('a row has the name, ahead, behind, the subject and the age', () => {
    const html = list(ready());
    expect(html).toContain('<ul class="fd-branches">');
    expect(html).toContain('<button type="button" class="fd-branch">');
    expect(html).toContain('<span class="fd-branch-name">feat/x</span>');
    expect(html).toContain('2 ahead');
    expect(html).toContain('3 behind');
    expect(html).toContain('Add the thing');
    expect(html).toContain('>3 h</span>');
    expect(html).not.toContain('merged');
  });

  test('zero ahead says merged and hides the zero counts', () => {
    const html = list(ready({ branches: [row({ ahead: 0, behind: 0 })] }));
    expect(html).toContain('<span class="fd-branch-merged">merged</span>');
    expect(html).not.toContain('ahead');
    expect(html).not.toContain('behind');
  });

  test('rows keep the order of the deck', () => {
    const html = list(
      ready({ branches: [row({ name: 'b-new', head: '1' }), row({ name: 'a-old', head: '2' })] }),
    );
    expect(html.indexOf('b-new')).toBeLessThan(html.indexOf('a-old'));
  });

  test('the header names the branch of the session, or says there is none', () => {
    const on = list(ready());
    expect(on).toContain('Branches of this repository, newest first.');
    expect(on).toContain('This session is on <code>main</code>.');
    expect(on).toContain('Uncommitted changes in other folders are not shown.');
    expect(list(ready({ branch: null }))).toContain('The session is not on a branch.');
  });

  test('a cut list and an empty list say so', () => {
    expect(list(ready({ truncated: true }))).toContain('Only the 200 newest branches are listed.');
    expect(list(ready())).not.toContain('Only the 200');
    const empty = list(ready({ branches: [] }));
    expect(empty).toContain('No other branches.');
    expect(empty).not.toContain('fd-branches');
  });

  test('loading and errors', () => {
    expect(list({ kind: 'loading' })).toContain('Loading the branches…');
    const busy = list({
      kind: 'error',
      failure: { message: 'The station is busy. Try again in a moment.', retry: true },
    });
    expect(busy).toContain('role="alert"');
    expect(busy).toContain('The station is busy. Try again in a moment.');
    expect(busy).toContain('>Try again</button>');
    const fatal = list({ kind: 'error', failure: { message: 'Not for you.', retry: false } });
    expect(fatal).toContain('Not for you.');
    expect(fatal).not.toContain('Try again');
  });
});

const diffOf = (patch: string, files = 2, ins = 5, del = 0): SessionDiff => ({
  session_id: 's',
  base_oid: 'a',
  summary: { files_changed: files, insertions: ins, deletions: del },
  patch,
  truncated: false,
});
const pane = (state: DiffState, origin = 'main') =>
  renderToStaticMarkup(
    <BranchDiffView
      row={row()}
      state={state}
      origin={origin}
      onBack={() => {}}
      onRetry={() => {}}
    />,
  );

describe('the diff of one branch (DL-191)', () => {
  const patch = 'diff --git a/x.ts b/x.ts\n--- a/x.ts\n+++ b/x.ts\n@@ -1 +1 @@\n-a\n+b\n';

  test('it has Back to branches, the stat line and the diff', () => {
    const html = pane({ kind: 'ready', diff: diffOf(patch) });
    expect(html).toContain('Back to branches');
    expect(html).toContain('fd-ghost');
    expect(html).toMatch(/feat\/x: 2 files <span class="fd-add">\+5<\/span> since it left main/);
    expect(html).not.toContain('fd-del');
    expect(html).toContain('fd-diff');
  });

  test('one file is singular and a deletion count shows when above zero', () => {
    const html = pane({ kind: 'ready', diff: diffOf(patch, 1, 0, 4) }, 'HEAD');
    expect(html).toMatch(/1 file <span class="fd-del">−4<\/span> since it left HEAD/);
    expect(html).not.toContain('fd-add');
  });

  test('an empty patch says there are no committed changes', () => {
    const html = pane({ kind: 'ready', diff: diffOf('  \n', 0, 0, 0) });
    expect(html).toContain('No committed changes.');
    expect(html).toContain('Back to branches');
    expect(html).not.toContain('fd-diff');
  });

  test('loading and error keep the Back button; Try again only for a retry error', () => {
    expect(pane({ kind: 'loading' })).toContain('Loading the diff…');
    const retry = pane({ kind: 'error', failure: { message: 'Timed out.', retry: true } });
    expect(retry).toContain('Back to branches');
    expect(retry).toContain('role="alert"');
    expect(retry).toContain('>Try again</button>');
    const plain = pane({ kind: 'error', failure: { message: 'Bad head.', retry: false } });
    expect(plain).toContain('Back to branches');
    expect(plain).not.toContain('Try again');
  });
});
