import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { FileContent } from '../gen';
import { FileView } from './FilesPanel';

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

  test('a binary file is not shown', () => {
    const html = view(file('logo.png', '', { binary: true, text: null }));
    expect(html).toContain('A binary file is not shown.');
    expect(html).toContain('aria-label="Copy the path"');
  });
});
