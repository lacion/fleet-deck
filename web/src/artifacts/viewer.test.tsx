import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { card } from '../testing/fleet';
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

const render = (
  data: ViewerData,
  extra: {
    canShare?: boolean;
    variant?: 'panel' | 'page';
    sessions?: Parameters<typeof ArtifactViewerView>[0]['sessions'];
    now?: number;
  } = {},
) =>
  renderToStaticMarkup(
    <ArtifactViewerView
      data={data}
      variant={extra.variant ?? 'panel'}
      canShare={extra.canShare ?? true}
      onVersion={() => {}}
      onBack={() => {}}
      onShare={() => {}}
      sessions={extra.sessions}
      now={extra.now}
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
    // The version is a quiet view option (§4.3): a ghost select.
    expect(html).toMatch(/<select class="fd-ghost fd-sm" aria-label="Version"/);
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

  test('one version is a muted badge, not a select with one option', () => {
    const html = render(ready({}, detail('a1', { latest_version: 1 })));
    expect(html).not.toContain('<select');
    expect(html).not.toContain('(latest)');
    expect(html).toContain(
      '<span class="fd-badge fd-art-version" data-tone="muted"><span class="fd-vh">Version </span>v1</span>',
    );
  });

  test('the page has the Page header: the way back above the title, the panel a back button', () => {
    const html = render(ready(), { variant: 'page' });
    // §4.3: "‹ All artifacts" is a row of its own above the title row, not in it.
    expect(html).toMatch(
      /<div class="fd-page-backrow"><a class="fd-btn fd-ghost fd-sm fd-page-back" href="#\/artifacts">[\s\S]*All artifacts<\/a><\/div><div class="fd-page-titles"><div class="fd-page-titlerow"><h1 class="fd-page-title">/,
    );
    expect(html).toContain('aria-label="Artifact"');
    expect(render(ready())).toContain('Back to the list');
    expect(render(ready())).toContain('<h2 class="fd-art-name">');
  });

  test('the page says who published the artifact and when; kind and version are its filters', () => {
    const d = detail('a1', { title: 'Plan', session_id: 'sa' }, [], ['index.html']);
    const at = d.versions[d.versions.length - 1]?.created_at ?? 0;
    const html = render(ready({}, d), {
      variant: 'page',
      sessions: [card('sa', 'working', 'station-a', { title: 'Review' })],
      now: at + 5 * 3_600_000,
    });
    expect(html).toContain('<h1 class="fd-page-title">Plan</h1>');
    expect(html).toContain(
      `<p class="fd-page-desc">${d.artifact.station_name} · Review · published 5 h ago</p>`,
    );
    expect(html).toMatch(/<div class="fd-page-filters"><span class="fd-badge" data-kind="html">/);
    expect(html).toMatch(/<div class="fd-page-actions">[\s\S]*Open in a new tab/);
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
