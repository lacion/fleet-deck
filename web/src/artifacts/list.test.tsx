import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import { card, station } from '../testing/fleet';
import { ArtifactList, visible } from './ArtifactList';
import { ArtifactsListBody, ArtifactsPage } from './ArtifactsPage';
import { deckArtifact, fakeActions, NOW } from './fixtures';
import { publishable, kindOfPath } from './kinds';
import { SessionArtifactsList } from './SessionArtifacts';

describe('artifact list', () => {
  test('newest first, and one kind only when asked', () => {
    const items = [
      deckArtifact('old', { updated_at: 1 }),
      deckArtifact('new', { updated_at: 9, kind: 'markdown' }),
      deckArtifact('mid', { updated_at: 5 }),
    ];
    expect(visible(items, 'all').map((a) => a.artifact_id)).toEqual(['new', 'mid', 'old']);
    expect(visible(items, 'html').map((a) => a.artifact_id)).toEqual(['mid', 'old']);
    expect(visible(items, 'pdf')).toEqual([]);
  });

  test('a row: title, kind, version and age; the global page rows link to the viewer', () => {
    const html = renderToStaticMarkup(
      <ArtifactList
        items={[deckArtifact('a1', { title: 'Plan' })]}
        now={NOW}
        where={() => 'Alpha · Review'}
      />,
    );
    expect(html).toContain('Plan');
    expect(html).toContain('>HTML<');
    expect(html).toContain('v2');
    expect(html).toContain('5m');
    expect(html).toContain('Alpha · Review');
    expect(html).toContain('href="#/artifact/station-a/a1"');
  });

  test('a session row is a button that opens in place', () => {
    const html = renderToStaticMarkup(
      <ArtifactList items={[deckArtifact('a1')]} now={NOW} onOpen={() => {}} />,
    );
    expect(html).toContain('<button type="button" class="fd-art-row"');
    expect(html).not.toContain('href=');
  });

  test('the kind of a path follows the extension, and only known kinds are publishable', () => {
    expect(kindOfPath('out/Report.HTML')).toBe('html');
    expect(kindOfPath('a.md')).toBe('markdown');
    expect(kindOfPath('a.png')).toBe('image');
    expect(kindOfPath('a.svg')).toBe('svg');
    expect(kindOfPath('a.pdf')).toBe('pdf');
    expect(kindOfPath('data.csv')).toBe('other');
    expect(kindOfPath('README')).toBe('other');
    expect(publishable('a.htm')).toBe(true);
    expect(publishable('a.rs')).toBe(false);
  });
});

const body = (props: Partial<Parameters<typeof ArtifactsListBody>[0]>) =>
  renderToStaticMarkup(
    <ArtifactsListBody
      state={{ kind: 'loading' }}
      filtered={false}
      now={NOW}
      where={() => ''}
      onRetry={() => {}}
      {...props}
    />,
  );

describe('artifacts page states', () => {
  test('loading, error, empty and no match', () => {
    expect(body({})).toContain('Loading the artifacts…');
    const failed = body({ state: { kind: 'error', text: 'the deck is down' } });
    expect(failed).toContain('role="alert"');
    expect(failed).toContain('Try again');
    expect(body({ state: { kind: 'ok', items: [] } })).toContain('No artifacts yet');
    expect(body({ state: { kind: 'ok', items: [] }, filtered: true })).toContain(
      'No artifact matches',
    );
  });

  test('the page has the search, kind, station and session filters', () => {
    const html = renderToStaticMarkup(
      <ArtifactsPage
        actions={fakeActions()}
        stations={[station()]}
        sessions={[card('s1', 'working', 'station-a', { title: 'Review' })]}
        now={NOW}
        initial={{ kind: 'ok', items: [deckArtifact('a1', { title: 'Plan' })] }}
      />,
    );
    expect(html).toContain('<h1 class="fd-page-title">Artifacts</h1>');
    expect(html).toContain('aria-label="Search by title"');
    expect(html).toContain('All kinds');
    expect(html).toContain('>Markdown<');
    expect(html).toContain('All stations');
    expect(html).toContain('>Alpha<');
    expect(html).toContain('All sessions');
    expect(html).toContain('>Review<');
    expect(html).toContain('Plan');
  });

  test('the page without a deck says so', () => {
    const html = renderToStaticMarkup(
      <ArtifactsPage actions={undefined} stations={[]} sessions={[]} />,
    );
    expect(html).toContain('Artifacts need a live board.');
  });

  test('the session list states', () => {
    const list = (state: Parameters<typeof SessionArtifactsList>[0]['state']) =>
      renderToStaticMarkup(
        <SessionArtifactsList state={state} now={NOW} onOpen={() => {}} onRetry={() => {}} />,
      );
    expect(list({ kind: 'loading' })).toContain('Loading the artifacts…');
    expect(list({ kind: 'error', text: 'nope' })).toContain('Try again');
    expect(list({ kind: 'ok', items: [] })).toContain('This session has no artifacts yet.');
    expect(list({ kind: 'ok', items: [deckArtifact('a1')] })).toContain('Report a1');
  });
});

describe('the board shell', () => {
  test('the artifacts page and the viewer page route in the app', () => {
    const page = renderToStaticMarkup(
      <App
        initialRoute={{ name: 'artifacts' }}
        initialLayout="desktop"
        artifacts={fakeActions()}
      />,
    );
    expect(page).toContain('aria-label="Search by title"');
    const viewer = renderToStaticMarkup(
      <App
        initialRoute={{ name: 'artifact', station: 'station-a', id: 'a1', version: null }}
        initialLayout="desktop"
        artifacts={fakeActions()}
      />,
    );
    expect(viewer).toContain('Loading the artifact…');
  });

  test('the sidebar and More link to the page', () => {
    const html = renderToStaticMarkup(<App initialLayout="desktop" artifacts={fakeActions()} />);
    expect(html).toContain('href="#/artifacts"');
    const more = renderToStaticMarkup(
      <App initialRoute={{ name: 'more' }} initialLayout="phone" artifacts={fakeActions()} />,
    );
    expect(more).toContain('href="#/artifacts"');
  });
});
