import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { WorkspaceListing } from '../gen';
import { card } from '../testing/fleet';
import { crumbsOf, FolderPicker, QUICK_ROWS } from './FolderPicker';

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
    expect(html).toContain('aria-label="Pin this folder"');
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
    // The folder name is sans, the same as the name of a recent folder.
    expect(html).toContain('<span class="fd-folder-base">app</span>');
    expect(html).toContain('git repository');
    // A listing row opens its folder: it ends in a chevron (a Recent row takes the folder).
    expect(html).toMatch(
      /aria-label="Open notes">.*?<span class="fd-folder-base">notes<\/span><svg[^>]*lucide-chevron-right[^>]*>/,
    );
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
    expect(html).not.toContain('fd-folder-more');
  });

  test('the panel shows the first recent folders and "Show all" with the count', () => {
    const recent = (pins: readonly string[]) => {
      const real = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
      const stored = JSON.stringify({ 'station-a': pins });
      Object.defineProperty(globalThis, 'localStorage', {
        configurable: true,
        value: { getItem: (k: string) => (k === 'fd-dir-pins' ? stored : null) },
      });
      try {
        const sessions = Array.from({ length: QUICK_ROWS + 1 }, (_, i) =>
          card(`s${i}`, 'idle', 'station-a', { cwd: `/w/f${i}`, updated_at: i + 1 }),
        );
        const html = render({ initialOpen: true, initialListing: listing, sessions });
        return html.slice(html.indexOf('aria-label="Recent directories"'), html.indexOf('<nav'));
      } finally {
        if (real === undefined) delete (globalThis as { localStorage?: unknown }).localStorage;
        else Object.defineProperty(globalThis, 'localStorage', real);
      }
    };
    // A pin and six recent folders: five rows, then "Show all" with the count.
    const folded = recent(['/w/pin']);
    expect(folded.match(/title="\/w\/[^"]+"/g)).toHaveLength(QUICK_ROWS);
    expect(folded).toContain(
      '<button type="button" class="fd-folder-more">Show all<span class="fd-count">7</span></button>',
    );
    // One hidden row is no reason for a toggle row: every row shows.
    const all = recent([]);
    expect(all.match(/title="\/w\/f\d"/g)).toHaveLength(QUICK_ROWS + 1);
    expect(all).not.toContain('fd-folder-more');
  });

  test('an empty folder says that it has no subfolders', () => {
    const empty = { ...listing, entries: [] };
    expect(render({ initialOpen: true, initialListing: empty })).toContain('No subfolders.');
  });

  test('the crumbs of a path carry the path up to each part; a home folder is one ~', () => {
    expect(crumbsOf('/home/me/app')).toEqual([
      { name: '~', path: '/home/me' },
      { name: 'app', path: '/home/me/app' },
    ]);
    expect(crumbsOf('/Users/me')).toEqual([{ name: '~', path: '/Users/me' }]);
    expect(crumbsOf('/srv/data')).toEqual([
      { name: 'srv', path: '/srv' },
      { name: 'data', path: '/srv/data' },
    ]);
    expect(crumbsOf('/')).toEqual([]);
  });

  test('the listing names are folder names, the foot names the folder it takes', () => {
    const html = render({ initialOpen: true, initialListing: listing });
    expect(html).toContain('<span class="fd-folder-base">app</span>');
    expect(html).toContain('title="/home/me/projects"><bdi>~/projects</bdi>');
    // Close first, then the choice (as Cancel, then Start session).
    expect(html.indexOf('>Close<')).toBeLessThan(html.indexOf('>Use this folder<'));
  });
});
