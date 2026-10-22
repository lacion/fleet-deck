import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { ArtifactPage, ArtifactViewerView, FRAME_SANDBOX } from './ArtifactViewer';
import { detail, fakeActions } from './fixtures';
import type { ViewerData, ViewerReady } from './viewer';
import { mainFile, selectedVersion } from './viewer';

const ready = (over: Partial<ViewerReady> = {}, d = detail('a1')): ViewerData => ({
  kind: 'ok',
  value: {
    detail: d,
    version: d.artifact.latest_version,
    link: { kind: 'ok', value: '/a/tok/' },
    text: null,
    ...over,
  },
});

const render = (data: ViewerData, extra: { canShare?: boolean; variant?: 'panel' | 'page' } = {}) =>
  renderToStaticMarkup(
    <ArtifactViewerView
      data={data}
      variant={extra.variant ?? 'panel'}
      canShare={extra.canShare ?? true}
      onVersion={() => {}}
      onBack={() => {}}
      onShare={() => {}}
    />,
  );

describe('artifact viewer', () => {
  test('loading: one status line and no frame', () => {
    const html = render({ kind: 'loading' });
    expect(html).toContain('role="status"');
    expect(html).toContain('Loading the artifact…');
    expect(html).not.toContain('<iframe');
  });

  test('error: the text as an alert', () => {
    const html = render({ kind: 'error', text: 'unknown artifact `zz`' });
    expect(html).toContain('role="alert"');
    expect(html).toContain('unknown artifact `zz`');
    expect(html).not.toContain('<iframe');
  });

  test('html: a sandboxed frame without allow-same-origin', () => {
    const html = render(ready());
    expect(html).toContain('<iframe');
    expect(html).toContain('src="/a/tok/"');
    expect(html).toContain(`sandbox="${FRAME_SANDBOX}"`);
    expect(html).toContain('referrerPolicy="no-referrer"');
    expect(html).not.toContain('allow-same-origin');
    expect(FRAME_SANDBOX).toBe('allow-scripts allow-popups allow-forms allow-modals');
  });

  test('svg and pdf use the same sandboxed frame; an image is an img', () => {
    for (const kind of ['svg', 'pdf'] as const) {
      const html = render(ready({}, detail('a1', { kind }, [], ['main.x'])));
      expect(html).toContain('<iframe');
      expect(html).not.toContain('allow-same-origin');
    }
    const image = render(ready({}, detail('a1', { kind: 'image', title: 'Shot' }, [], ['s.png'])));
    expect(image).toContain('<img');
    expect(image).toContain('alt="Shot"');
    expect(image).not.toContain('<iframe');
  });

  test('markdown: the chat Markdown component renders the raw text', () => {
    const d = detail('a1', { kind: 'markdown' }, [], ['notes.md']);
    const html = render(ready({ text: { kind: 'ok', value: '# Hello\n\n- one' } }, d));
    expect(html).toContain('class="fd-art-md"');
    expect(html).toContain('<h1>Hello</h1>');
    expect(html).not.toContain('<iframe');
    expect(render(ready({ text: { kind: 'loading' } }, d))).toContain('Loading the document…');
    const failed = render(ready({ text: { kind: 'error', text: 'no files yet' } }, d));
    expect(failed).toContain('role="alert"');
    expect(failed).toContain('no files yet');
  });

  test('another file type has no preview but keeps Download', () => {
    const html = render(ready({}, detail('a1', { kind: 'other' }, [], ['data.csv'])));
    expect(html).toContain('This file type has no preview.');
    expect(html).toContain('download="data.csv"');
  });

  test('the toolbar: version picker, new tab, download and share', () => {
    const html = render(ready({}, detail('a1', { latest_version: 3 }, [], ['site/index.html'])));
    expect(html).toContain('aria-label="Version"');
    expect(html).toContain('>v3 (latest)<');
    expect(html).toContain('>v2<');
    expect(html).toContain('>v1<');
    expect(html).toMatch(/href="\/a\/tok\/" target="_blank" rel="noopener noreferrer"/);
    expect(html).toContain('Open in a new tab');
    expect(html).toContain('href="/api/artifacts/station-a/a1/3/raw/site/index.html"');
    expect(html).toContain('download="index.html"');
    expect(html).toContain('Share…');
  });

  test('Share… is for the owner only', () => {
    expect(render(ready(), { canShare: false })).not.toContain('Share…');
  });

  test('Share… has its icon in the bar; a narrow viewer has Download and Share… in the menu', () => {
    const html = render(ready({}, detail('a1', {}, [], ['index.html'])));
    expect(html).toMatch(
      /class="fd-ghost fd-sm fd-art-action fd-art-wide"><svg[^>]*lucide-share-2/,
    );
    expect(html).toContain('aria-label="More actions"');
    expect(html).toMatch(
      /<button type="button" class="fd-art-narrow"><svg[\s\S]*?<\/svg> Download/,
    );
    expect(html).toMatch(/<button type="button" class="fd-art-narrow"><svg[\s\S]*?<\/svg> Share…/);
    // No owner rows for the wide bar (no wall, no delete): the menu is for a narrow viewer only.
    expect(html).toContain('<span class="fd-art-more" data-wide="none">');
  });

  test('the page variant links back to the list, the panel has a back button', () => {
    expect(render(ready(), { variant: 'page' })).toContain('href="#/artifacts"');
    expect(render(ready())).toContain('Back to the list');
  });

  test('the page title is the h1 of the page, wide; the panel keeps an h2', () => {
    expect(render(ready(), { variant: 'page' })).toContain('<h1 class="fd-art-name fd-wide">');
    expect(render(ready())).toContain('<h2 class="fd-art-name">');
  });

  test('the page without a deck says so', () => {
    const html = renderToStaticMarkup(
      <ArtifactPage actions={undefined} station="s" id="a" version={null} onVersion={() => {}} />,
    );
    expect(html).toContain('Artifacts need a live board.');
  });

  test('the viewer page starts in the loading state', () => {
    const html = renderToStaticMarkup(
      <ArtifactPage
        actions={fakeActions()}
        station="station-a"
        id="a1"
        version={null}
        onVersion={() => {}}
      />,
    );
    expect(html).toContain('Loading the artifact…');
  });

  test('the shown version is the wanted one, else the latest; the main file is the first', () => {
    const d = detail('a1', { latest_version: 3 }, [], ['index.html', 'app.js']);
    expect(selectedVersion(d, null)).toBe(3);
    expect(selectedVersion(d, 2)).toBe(2);
    expect(mainFile(d, 2)).toBe('index.html');
    expect(mainFile(d, 9)).toBeUndefined();
  });
});
