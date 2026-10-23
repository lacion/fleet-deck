import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { FileContent } from '../gen';
import { FilesPanel, FileView } from './FilesPanel';
import type { FileActions } from './workspace';

const file = (path: string, text: string, extra: Partial<FileContent> = {}): FileContent => ({
  path,
  size: text.length,
  binary: false,
  truncated: false,
  text,
  ...extra,
});

const view = (f: FileContent, root: string | null = null) =>
  renderToStaticMarkup(<FileView file={f} root={root} onBack={() => {}} />);

describe('file view', () => {
  test('a Markdown file is rendered, with the Rendered and Raw chips', () => {
    const html = view(file('docs/a.md', '# Title\n\n| a |\n|---|\n| `x` |\n'));
    expect(html).toContain('class="fd-files-md"');
    expect(html).toContain('<h1>Title</h1>');
    expect(html).toMatch(/class="fd-chip" aria-pressed="true"[^>]*>Rendered</);
    expect(html).toMatch(/class="fd-chip" aria-pressed="false"[^>]*>Raw</);
    expect(html).not.toContain('fd-file-text');
    expect(view(file('README.markdown', 'hi'))).toContain('fd-files-md');
  });

  test('another file is plain text without the chips', () => {
    const html = view(file('src/a.ts', 'let a = 1;'));
    expect(html).toContain('fd-file-text');
    expect(html).not.toContain('fd-files-md');
    expect(html).not.toContain('Rendered');
  });

  test('Copy the path is always there', () => {
    expect(view(file('src/a.ts', 'x'))).toContain('aria-label="Copy the path"');
    expect(view(file('docs/a.md', 'x'), '/home/me/app')).toContain('aria-label="Copy the path"');
  });

  test('the size is in decimal units, the same as the other sizes of the board', () => {
    expect(view(file('src/a.ts', 'x', { size: 3200 }))).toContain('3.2 KB');
    expect(view(file('src/b.ts', 'x', { size: 812 }))).toContain('812 B');
  });

  test('a binary file is not shown', () => {
    const html = view(file('logo.png', '', { binary: true, text: null }));
    expect(html).toContain('A binary file is not shown.');
    expect(html).toContain('aria-label="Copy the path"');
  });
});

describe('files panel', () => {
  // A read that never answers: the panel shows its first state.
  const never = () => new Promise<never>(() => {});
  const files: FileActions = {
    list: never,
    read: never,
    search: never,
    diff: never,
    sessionDiff: never,
  };

  test('"Files in" and the crumbs label the listing; a read is the §4.7 loading line', () => {
    const html = renderToStaticMarkup(<FilesPanel files={files} root="/home/me/v2" />);
    // The label of the list, not its first entry: no folder icon, the current crumb marked.
    expect(html).toContain(
      '<nav class="fd-files-crumbs" aria-label="Folder path"><span class="fd-files-crumbs-lead">Files in</span>',
    );
    expect(html).toContain(
      'aria-current="location" title="/home/me/v2">v2<span class="fd-vh">, root folder</span></button>',
    );
    expect(html).not.toContain('lucide-folder-open');
    expect(html).toContain('<p class="fd-note fd-loading" role="status">Loading the folder…</p>');
    expect(html).not.toContain('role="alert"');
  });
});
