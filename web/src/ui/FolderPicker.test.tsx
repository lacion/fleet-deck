import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { WorkspaceListing } from '../gen';
import { card } from '../testing/fleet';
import { crumbsOf, FolderPicker } from './FolderPicker';

const listing: WorkspaceListing = {
  path: '/home/me/projects',
  parent: '/home/me',
  entries: [
    { name: 'app', path: '/home/me/projects/app', repository: true },
    { name: 'notes', path: '/home/me/projects/notes', repository: false },
  ],
  truncated: false,
};

const list = async () => listing;

const render = (extra: Partial<Parameters<typeof FolderPicker>[0]> = {}) =>
  renderToStaticMarkup(
    <FolderPicker
      stationId="station-a"
      value=""
      onChange={() => {}}
      list={list}
      label="Folder"
      {...extra}
    />,
  );

describe('FolderPicker (DL-171)', () => {
  test('a text input and a Browse… button; the panel is closed', () => {
    const html = render({ value: '/home/me/app' });
    expect(html).toContain('<input type="text" aria-label="Folder"');
    expect(html).toContain('value="/home/me/app"');
    expect(html).toContain('Browse…');
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain('fd-folder-panel');
    // A typed path can be pinned.
    expect(html).toContain('aria-label="Pin this directory"');
  });

  test('without a listing call the picker is a text input only', () => {
    const html = render({ list: undefined });
    expect(html).not.toContain('Browse…');
    expect(html).toContain('<input type="text"');
  });

  test('Browse is off when the page says so', () => {
    expect(render({ browseDisabled: true })).toMatch(/<button[^>]*disabled=""[\s\S]*?Browse…/);
  });

  test('the open panel: breadcrumbs, folders with the git mark, Use this folder', () => {
    const html = render({ initialOpen: true, initialListing: listing });
    expect(html).toContain('aria-label="Browse the station"');
    expect(html).toContain('aria-label="Folder path"');
    expect(html).toContain('>Roots</button>');
    // The first crumbs sit above the allowed parent: text. The parent and the folder stay.
    expect(html).toContain('aria-current="location">projects</span>');
    expect(html).toContain('aria-label="Open app"');
    expect(html).toContain('git repository');
    expect(html).toContain('aria-label="Open notes"');
    expect(html).toContain('Use this folder');
  });

  test('the recent folders of the station sit in the panel', () => {
    const html = render({
      initialOpen: true,
      initialListing: listing,
      sessions: [card('a', 'idle', 'station-a', { cwd: '/w/one', updated_at: 5 })],
    });
    expect(html).toContain('aria-label="Recent directories"');
    expect(html).toContain('title="/w/one"');
  });

  test('the crumbs of a path carry the path up to each part', () => {
    expect(crumbsOf('/home/me/app')).toEqual([
      { name: 'home', path: '/home' },
      { name: 'me', path: '/home/me' },
      { name: 'app', path: '/home/me/app' },
    ]);
    expect(crumbsOf('/')).toEqual([]);
  });
});
