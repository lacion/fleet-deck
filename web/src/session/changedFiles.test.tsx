import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { SessionDiff } from '../gen';
import { CHANGED_FILES_MAX, changedFiles, changedPaths, loadChanged } from './changedFiles';
import { ChangedFilesList } from './FilesPanel';
import type { FileActions } from './workspace';

const header = (a: string, b = a) => `diff --git a/${a} b/${b}\n`;

const patch =
  `${header('src/a.ts')}index 1..2 100644\n--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1 +1 @@\n-x\n+y\n` +
  `${header('docs/new.md')}new file mode 100644\n--- /dev/null\n+++ b/docs/new.md\n@@ -0,0 +1 @@\n+hi\n` +
  `${header('old/name.rs', 'moved/name.rs')}similarity index 90%\nrename from old/name.rs\nrename to moved/name.rs\n`;

const diff = (extra: Partial<SessionDiff> = {}): SessionDiff => ({
  session_id: 's1',
  base_oid: 'abc',
  summary: { files_changed: 1, insertions: 1, deletions: 0 },
  patch,
  truncated: false,
  ...extra,
});

const actions = (answer: () => Promise<SessionDiff>): FileActions => ({
  list: async () => ({ root: '/w', path: '', entries: [], truncated: false }),
  read: async () => {
    throw new Error('not used');
  },
  search: async () => {
    throw new Error('not used');
  },
  diff: async () => {
    throw new Error('not used');
  },
  sessionDiff: answer,
});

import { changedPaths as pathsOf } from './changedFiles';

test('a deleted file is not listed', () => {
  const patch = [
    'diff --git a/gone.rs b/gone.rs',
    'deleted file mode 100644',
    'index 1..0',
    'diff --git a/kept.rs b/kept.rs',
    'index 1..2',
  ].join('\n');
  expect(pathsOf(patch)).toEqual(['kept.rs']);
});

describe('changed paths of a patch', () => {
  test('a modified, a new and a renamed file: the b/ path, in patch order', () => {
    expect(changedPaths(patch)).toEqual(['src/a.ts', 'docs/new.md', 'moved/name.rs']);
  });

  test('a path with spaces stays whole, and a content line is not a header', () => {
    const text = `${header('my dir/a b.txt')}@@ -1 +1 @@\n+diff --git a/x b/x\n`;
    expect(changedPaths(text)).toEqual(['my dir/a b.txt']);
  });

  test('more than 50 paths: the first 50', () => {
    const many = Array.from({ length: 80 }, (_, n) => header(`f${n}.txt`)).join('');
    const paths = changedPaths(many);
    expect(paths).toHaveLength(CHANGED_FILES_MAX);
    expect(paths[0]).toBe('f0.txt');
    expect(paths[49]).toBe('f49.txt');
  });

  test('a patch without a header has no path', () => {
    expect(changedPaths('')).toEqual([]);
  });
});

describe('the change of each file', () => {
  test('the kind and the line counts; the --- and +++ header lines do not count', () => {
    expect(changedFiles(patch).changes).toEqual({
      'src/a.ts': { kind: 'M', insertions: 1, deletions: 1 },
      'docs/new.md': { kind: 'A', insertions: 1, deletions: 0 },
      'moved/name.rs': { kind: 'R', insertions: 0, deletions: 0 },
    });
  });

  test('a removed line that starts with --- counts as a deletion inside a hunk', () => {
    const text = `${header('a.md')}--- a/a.md\n+++ b/a.md\n@@ -1,2 +1 @@\n---- old rule\n+new\n same\n`;
    expect(changedFiles(text).changes['a.md']).toEqual({ kind: 'M', insertions: 1, deletions: 1 });
  });

  test('a deleted file has no change entry', () => {
    const text = `${header('gone.rs')}deleted file mode 100644\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n`;
    expect(changedFiles(text)).toEqual({ paths: [], changes: {} });
  });
});

describe('loadChanged', () => {
  test('the paths, their changes and the cut flag of the session diff', async () => {
    expect(await loadChanged(actions(async () => diff({ truncated: true })))).toEqual({
      paths: ['src/a.ts', 'docs/new.md', 'moved/name.rs'],
      changes: changedFiles(patch).changes,
      truncated: true,
    });
  });

  test('an error gives none', async () => {
    const files = actions(async () => {
      throw new Error('refused');
    });
    expect(await loadChanged(files)).toBeNull();
  });

  test('an empty patch gives none', async () => {
    expect(await loadChanged(actions(async () => diff({ patch: '' })))).toBeNull();
  });
});

describe('the changed files section', () => {
  const html = (truncated: boolean) =>
    renderToStaticMarkup(
      <ChangedFilesList
        changed={{
          paths: ['src/a.ts', 'README.md', 'src/b.ts'],
          changes: {
            'src/a.ts': { kind: 'M', insertions: 9, deletions: 3 },
            'README.md': { kind: 'A', insertions: 4, deletions: 0 },
            'src/b.ts': { kind: 'R', insertions: 0, deletions: 0 },
          },
          truncated,
        }}
        onOpen={() => {}}
      />,
    );

  test('a labelled section with the heading and one button per file', () => {
    const out = html(false);
    expect(out).toContain('<section class="fd-files-changed" aria-label="Changed files">');
    expect(out).toContain('>Changed since the session started<');
    expect(out).toContain('<span class="fd-files-name">a.ts</span>');
    expect(out).toContain('<span class="fd-files-dir">src</span>');
    expect(out).toContain('<span class="fd-files-name">README.md</span>');
    expect(out.match(/<button/g)).toHaveLength(3);
  });

  test('each row shows its +/− counts and the badge of a diff file header at the right', () => {
    const out = html(false);
    // A modified file has no badge (as a diff file header); its kind is a hidden name.
    expect(out).toContain(
      '<span class="fd-files-stat"><span class="fd-add">+9</span><span class="fd-del">−3</span>' +
        '<span class="fd-vh">modified</span></span>',
    );
    // A new file: the ok "new" badge. No deletions: no −0.
    expect(out).toContain(
      '<span class="fd-files-stat"><span class="fd-add">+4</span>' +
        '<span class="fd-badge" data-tone="ok" title="New file"><span aria-hidden="true">new</span>' +
        '<span class="fd-vh">added</span></span></span>',
    );
    // A renamed file: a neutral word.
    expect(out).toContain(
      '<span class="fd-badge" title="Renamed file"><span aria-hidden="true">renamed</span>' +
        '<span class="fd-vh">renamed</span></span>',
    );
    expect(out).not.toContain('−0');
    expect(out).not.toContain('data-tone="muted"');
  });

  test('a file at the root has no folder', () => {
    expect(html(false).match(/fd-files-dir/g)).toHaveLength(2);
  });

  test('the cut line shows only when truncated', () => {
    expect(html(true)).toContain('More changes than shown.');
    expect(html(false)).not.toContain('More changes than shown.');
  });
});
