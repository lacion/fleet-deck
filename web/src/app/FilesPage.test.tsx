import { afterEach, describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { FileActions } from '../session/workspace';
import { card, station } from '../testing/fleet';
import { FilesPage } from './FilesPage';

const files: FileActions = {
  list: async () => ({ root: '/srv', path: '', entries: [], truncated: false }),
  read: async () => {
    throw new Error('not used');
  },
  search: async () => {
    throw new Error('not used');
  },
  diff: async () => {
    throw new Error('not used');
  },
};

const store = (items: Record<string, string>) => {
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    value: { getItem: (k: string) => items[k] ?? null },
  });
};

afterEach(() => {
  Reflect.deleteProperty(globalThis, 'localStorage');
});

describe('Files page (DL-141)', () => {
  test('without a files provider it shows the note and no form', () => {
    const html = renderToStaticMarkup(
      <FilesPage stations={[station()]} sessions={[]} filesFor={undefined} />,
    );
    expect(html).toContain('<h1 class="fd-screen-title">Files</h1>');
    expect(html).toContain('Files need a live board with the drive scope.');
    expect(html).not.toContain('<form');
  });

  test('the folder form: stations with a link first, the field and Open', () => {
    const html = renderToStaticMarkup(
      <FilesPage
        stations={[
          station('station-b', { reachable: false, name: 'Down' }),
          station('station-a', { name: 'Up' }),
        ]}
        sessions={[]}
        filesFor={() => files}
      />,
    );
    expect(html.indexOf('>Up<')).toBeGreaterThan(-1);
    expect(html.indexOf('>Up<')).toBeLessThan(html.indexOf('>Down<'));
    expect(html).toContain('Station');
    expect(html).toContain('Folder');
    expect(html).toContain('placeholder="/absolute/path/inside/an/allowed/root"');
    expect(html).toContain('Open</button>');
    expect(html).not.toContain('Pinned and recent');
    expect(html).not.toContain('aria-label="Files"><h3');
  });

  test('the chip row: the pins first, then the session folders, newest first', () => {
    store({ 'fd-dir-pins': JSON.stringify({ 'station-a': ['/srv/pinned/app'] }) });
    const at = (id: string, cwd: string, updated: number, stationId = 'station-a') => ({
      ...card(id, 'working', stationId, { cwd, updated_at: updated }),
    });
    const html = renderToStaticMarkup(
      <FilesPage
        stations={[station()]}
        sessions={[
          at('s1', '/home/me/old', 1),
          at('s2', '/home/me/new', 5),
          at('s3', '/home/me/new', 3),
          at('s4', '/srv/pinned/app', 9),
          at('s5', '/home/other/elsewhere', 7, 'station-b'),
        ]}
        filesFor={() => files}
      />,
    );
    expect(html).toContain('Pinned and recent');
    expect(html).toContain('title="/srv/pinned/app"');
    expect(html).toContain('pinned/app');
    expect(html).toContain('title="/home/me/new"');
    expect(html).toContain('me/new');
    expect(html).not.toContain('elsewhere');
    expect(html.match(/title="\/srv\/pinned\/app"/g)?.length).toBe(1);
    expect(html.indexOf('title="/srv/pinned/app"')).toBeLessThan(
      html.indexOf('title="/home/me/new"'),
    );
    expect(html.indexOf('title="/home/me/new"')).toBeLessThan(html.indexOf('title="/home/me/old"'));
  });

  test('the last opened folder is opened on mount', () => {
    store({ 'fd-files-root': JSON.stringify({ station: 'station-a', folder: '/srv/pinned/app' }) });
    const html = renderToStaticMarkup(
      <FilesPage stations={[station()]} sessions={[]} filesFor={() => files} />,
    );
    expect(html).toContain('aria-label="Files"');
    expect(html).toContain('value="/srv/pinned/app"');
    expect(html).toContain('aria-label="Search the files"');
  });
});
