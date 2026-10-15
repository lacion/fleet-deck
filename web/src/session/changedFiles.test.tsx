import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { SessionDiff } from '../gen';
import { CHANGED_FILES_MAX, changedPaths, loadChanged } from './changedFiles';
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

describe('loadChanged', () => {
  test('the paths and the cut flag of the session diff', async () => {
    expect(await loadChanged(actions(async () => diff({ truncated: true })))).toEqual({
      paths: ['src/a.ts', 'docs/new.md', 'moved/name.rs'],
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
        changed={{ paths: ['src/a.ts', 'README.md'], truncated }}
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
    expect(out.match(/<button/g)).toHaveLength(2);
  });

  test('a file at the root has no folder', () => {
    expect(html(false).match(/fd-files-dir/g)).toHaveLength(1);
  });

  test('the cut line shows only when truncated', () => {
    expect(html(true)).toContain('More changes than shown.');
    expect(html(false)).not.toContain('More changes than shown.');
  });
});
