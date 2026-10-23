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
  sessionDiff: async () => {
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
    expect(html).toContain('<h1 class="fd-page-title">Files</h1>');
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
    expect(html).toContain('placeholder="Folder path on Up"');
    expect(html).toContain('aria-label="Open a folder"');
    // What to do is the hint of the Folder field: no empty state under the form repeats it.
    expect(html).toMatch(
      /aria-describedby="([^"]+)-hint"[\s\S]*<span id="\1-hint" class="fd-fieldhint">Type the path of a folder on this station\.<\/span>/,
    );
    expect(html).not.toContain('fd-emptystate');
    expect(html).toContain('Open</button>');
    expect(html).not.toContain('Pinned and recent');
    expect(html).not.toContain('aria-label="Files"><h3');
  });

  test('the folder rows: the pins first, then the session folders, newest first', () => {
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
    // The folder rows of the New session picker (§5 paths): the name, then the parent from the
    // root; the full path is the title. No chip cloud.
    expect(html).toContain('<ul class="fd-folder-list fd-files-folders">');
    expect(html).not.toContain('fd-chip');
    expect(html).toContain(
      '<button type="button" data-pinned="" title="/srv/pinned/app" aria-label="/srv/pinned/app">',
    );
    expect(html).toContain(
      '<span class="fd-folder-base">app</span><span class="fd-folder-parent"><bdi>/srv/pinned/</bdi></span>',
    );
    expect(html).toContain('title="/home/me/new" aria-label="~/new"');
    expect(html).toContain(
      '<span class="fd-folder-base">new</span><span class="fd-folder-parent"><bdi>~/</bdi></span>',
    );
    expect(html).not.toContain('elsewhere');
    expect(html.match(/title="\/srv\/pinned\/app"/g)?.length).toBe(1);
    expect(html.indexOf('title="/srv/pinned/app"')).toBeLessThan(
      html.indexOf('title="/home/me/new"'),
    );
    expect(html.indexOf('title="/home/me/new"')).toBeLessThan(html.indexOf('title="/home/me/old"'));
    expect(html).toContain(
      'class="fd-fieldhint">Pick a recent folder, or type the path of a folder on this station.',
    );
  });

  test('without a pinned folder the rows say Recent; the home folder is Home; six rows at most', () => {
    store({});
    const html = renderToStaticMarkup(
      <FilesPage
        stations={[station()]}
        sessions={[
          card('s1', 'working', 'station-a', { cwd: '/home/me', updated_at: 9 }),
          ...[1, 2, 3, 4, 5, 6, 7].map((n) =>
            card(`r${n}`, 'working', 'station-a', { cwd: `/srv/app${n}`, updated_at: 8 - n }),
          ),
        ]}
        filesFor={() => files}
      />,
    );
    // A row field of §4.7: the group label in the 200px column.
    expect(html).toMatch(
      /<div class="fd-fieldwrap" data-layout="row" role="group" aria-labelledby="[^"]+"><span id="[^"]+" class="fd-fieldlabel">Recent<\/span>/,
    );
    expect(html).not.toContain('Pinned and recent');
    expect(html).toContain('<button type="button" title="/home/me" aria-label="Home"><svg');
    expect(html).toContain('<span class="fd-folder-base">Home</span></button>');
    expect(html.match(/<li>/g)).toHaveLength(6);
    expect(html).not.toContain('/srv/app6');
  });

  test('the fields are row fields: Station and Folder in the 200px label column', () => {
    store({});
    const html = renderToStaticMarkup(
      <FilesPage stations={[station()]} sessions={[]} filesFor={() => files} />,
    );
    expect(html).toContain(
      '<div class="fd-fieldwrap" data-layout="row"><label class="fd-fieldcontrol"><span class="fd-fieldlabel">Station</span><select>',
    );
    expect(html).toMatch(
      /<div class="fd-fieldwrap" data-layout="row"><label class="fd-fieldlabel" for="[^"]+-folder">Folder<\/label><div class="fd-files-folder-path"><input id="[^"]+-folder"/,
    );
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
